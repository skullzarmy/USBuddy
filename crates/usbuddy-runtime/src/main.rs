use std::{
    net::SocketAddr,
    path::PathBuf,
    process::Child,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use axum::{
    Json, Router,
    body::Body,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use usbuddy_core::{
    catalog::{Advisory, Catalog, ModelEntry, load_catalog},
    compiled_version,
    gguf::ArchMeta,
    layout::{DriveLayout, DropInModel},
    platform::detect_platform,
    ram::{FitBand, RamDecision, RamEstimateInput, assess_fit, detect_memory},
};

const INDEX_HTML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ui/web/src/index.html"
));
const APP_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ui/web/src/app.js"
));
const STYLES_CSS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ui/web/src/styles.css"
));
const MARKDOWN_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ui/web/src/markdown.js"
));
/// Embedded JPG icon, decoded at runtime into RGBA for both the tray and
/// the in-browser favicon-ish PNG endpoint. Cross-platform: same bytes are
/// used on macOS, Linux, and Windows.
const ICON_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/usbuddy-icon.png"
));

mod chats;
mod tray;

/// Port used internally by llama-server; separate from the runtime's own port.
const LLAMA_SERVER_PORT: u16 = 8766;

/// Default idle-unload threshold in seconds. After this much inactivity the
/// runtime SIGTERMs llama-server to release mlocked weights. Set to 0 via
/// `--idle-timeout-secs 0` to disable.
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;

/// How often the idle-watch task wakes up to check the last-activity stamp.
const IDLE_CHECK_INTERVAL_SECS: u64 = 15;

/// How long /api/launch will wait for llama-server to become healthy before
/// giving up and reporting the failure to the UI. A cold load of a 7-8B Q4
/// model off USB 3.0 is typically 5–30s; an 8B Q8 off a slow stick can push
/// past 60s. Five minutes is a deliberately generous ceiling — past that
/// something is genuinely wrong (corrupt weights, wrong-arch binary).
const LLAMA_READY_TIMEOUT_SECS: u64 = 300;

/// Poll interval against llama-server's /health endpoint while it's loading.
const LLAMA_READY_POLL_MS: u64 = 250;

#[derive(Debug, Parser)]
#[command(name = "usbuddy-runtime", version = compiled_version(), about = "USBuddy portable runtime wrapper")]
struct Cli {
    #[command(subcommand)]
    command: RuntimeCommand,
}

#[derive(Debug, Subcommand)]
enum RuntimeCommand {
    /// Serve the chat UI and runtime API on localhost.
    Serve {
        #[arg(long)]
        drive: PathBuf,
        #[arg(long, default_value_t = 8765)]
        port: u16,
        #[arg(long, default_value_t = false)]
        open_browser: bool,
        /// Idle-unload threshold in seconds. After this much inactivity the
        /// runtime stops llama-server so weights leave mlocked RAM. 0 disables.
        #[arg(long, default_value_t = DEFAULT_IDLE_TIMEOUT_SECS)]
        idle_timeout_secs: u64,
    },
    /// Print drive and catalog state to stdout without starting the server.
    Inspect {
        #[arg(long)]
        drive: PathBuf,
    },
}

// ---------------------------------------------------------------------------
// Shared runtime state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct RuntimeState {
    layout: DriveLayout,
    catalog: Option<Catalog>,
    llama_process: Arc<Mutex<Option<Child>>>,
    /// Unix-epoch seconds of the last activity that should keep llama-server
    /// alive (model launch or chat proxy hit). Read by the idle-watcher.
    last_activity: Arc<AtomicU64>,
    idle_timeout_secs: u64,
    /// Notified by `/api/shutdown` (and other shutdown paths) to ask the
    /// HTTP server to exit cleanly. Lets the chat UI quit the runtime
    /// without any external supervisor.
    shutdown: Arc<Notify>,
    /// Parameters of the most recent successful launch. Lets the chat proxy
    /// wake llama-server transparently after the idle-unload (or a crash)
    /// instead of failing with "unreachable".
    last_launch: Arc<Mutex<Option<LaunchParams>>>,
    /// Serializes spawn/health-wait so concurrent chat requests arriving
    /// after an idle-unload trigger exactly one relaunch.
    launch_lock: Arc<tokio::sync::Mutex<()>>,
}

/// Everything needed to (re)start llama-server for a given model.
#[derive(Clone)]
struct LaunchParams {
    model_id: String,
    model_path: PathBuf,
    model_bytes: u64,
    context_tokens: u32,
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn touch_activity(state: &RuntimeState) {
    state
        .last_activity
        .store(now_epoch_secs(), Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        RuntimeCommand::Inspect { drive } => {
            let state = load_state(drive, DEFAULT_IDLE_TIMEOUT_SECS)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&status_payload(&state, "Inspection only"))?
            );
            Ok(())
        }
        RuntimeCommand::Serve {
            drive,
            port,
            open_browser,
            idle_timeout_secs,
        } => run_serve(drive, port, open_browser, idle_timeout_secs),
    }
}

fn run_serve(
    drive: PathBuf,
    port: u16,
    open_browser: bool,
    idle_timeout_secs: u64,
) -> anyhow::Result<()> {
    let state = Arc::new(load_state(drive, idle_timeout_secs)?);
    let url = format!("http://127.0.0.1:{port}");

    // HTTP server runs on a background tokio runtime so the OS main thread
    // is free for the tray-icon event loop (required by macOS Cocoa).
    let server_state = state.clone();
    let _server_thread = std::thread::Builder::new()
        .name("usbuddy-http".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[fatal] failed to build tokio runtime: {e}");
                    std::process::exit(1);
                }
            };
            let code = match rt.block_on(serve_http(server_state, port, idle_timeout_secs)) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("[fatal] HTTP server exited with error: {e}");
                    1
                }
            };
            // The main thread is parked in the tray event loop and never
            // returns on its own — once the HTTP server has shut down
            // (web Quit, Ctrl-C), the whole process must go with it. This
            // is also what frees the drive so a scheduled eject can succeed.
            std::process::exit(code);
        })?;

    eprintln!("USBuddy runtime serving on {url}");
    if open_browser {
        let _ = open_browser_best_effort(&url);
    }

    // Tray event loop on the main thread. Returns / diverges only when
    // the user clicks Quit, at which point we signal shutdown and exit.
    crate::tray::run_tray(state, url)
}

async fn serve_http(
    state: Arc<RuntimeState>,
    port: u16,
    idle_timeout_secs: u64,
) -> anyhow::Result<()> {
    // Kill llama-server on Ctrl-C and trigger graceful shutdown.
    let cleanup_state = state.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        kill_llama_server(&cleanup_state.llama_process);
        cleanup_state.shutdown.notify_waiters();
    });

    // Idle-unload watcher: if llama-server is running and there's been
    // no activity for `idle_timeout_secs`, stop it.
    if idle_timeout_secs > 0 {
        let watch_state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(IDLE_CHECK_INTERVAL_SECS));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let running = watch_state
                    .llama_process
                    .lock()
                    .map(|g| g.is_some())
                    .unwrap_or(false);
                if !running {
                    continue;
                }
                let last = watch_state.last_activity.load(Ordering::Relaxed);
                let now = now_epoch_secs();
                if now.saturating_sub(last) >= watch_state.idle_timeout_secs {
                    eprintln!(
                        "USBuddy: stopping llama-server after {}s idle (footprint policy)",
                        watch_state.idle_timeout_secs
                    );
                    kill_llama_server(&watch_state.llama_process);
                }
            }
        });
    }

    let app = Router::new()
        .route("/", get(index))
        .route("/assets/app.js", get(app_js))
        .route("/assets/markdown.js", get(markdown_js))
        .route("/assets/styles.css", get(styles_css))
        .route("/assets/icon.png", get(icon_png))
        .route("/api/status", get(api_status))
        .route("/api/launch", post(api_launch))
        .route("/api/stop", post(api_stop))
        .route("/api/shutdown", post(api_shutdown))
        .route("/api/shutdown-eject", post(api_shutdown_eject))
        .route("/api/chat", axum::routing::any(api_chat_proxy))
        .route("/api/chat/{*rest}", axum::routing::any(api_chat_proxy))
        .route("/api/prefs", get(api_get_prefs).put(api_put_prefs))
        .route("/api/chats", get(api_list_chats))
        .route(
            "/api/chats/{id}",
            get(api_get_chat).put(api_put_chat).delete(api_delete_chat),
        )
        .with_state(state.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr).await?;

    let shutdown_signal = state.shutdown.clone();
    let kill_on_exit = state.llama_process.clone();
    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal.notified().await;
        })
        .await;
    kill_llama_server(&kill_on_exit);
    serve_result.context("runtime HTTP server exited unexpectedly")
}

// ---------------------------------------------------------------------------
// State helpers
// ---------------------------------------------------------------------------

fn load_state(drive: PathBuf, idle_timeout_secs: u64) -> anyhow::Result<RuntimeState> {
    let layout = DriveLayout::new(drive);
    let catalog = if layout.catalog_path().exists() {
        Some(load_catalog(&layout.catalog_path())?)
    } else {
        None
    };
    Ok(RuntimeState {
        layout,
        catalog,
        llama_process: Arc::new(Mutex::new(None)),
        last_activity: Arc::new(AtomicU64::new(now_epoch_secs())),
        idle_timeout_secs,
        shutdown: Arc::new(Notify::new()),
        last_launch: Arc::new(Mutex::new(None)),
        launch_lock: Arc::new(tokio::sync::Mutex::new(())),
    })
}

fn status_payload(state: &RuntimeState, message: &str) -> RuntimeStatus {
    let current = state.layout.read_current().ok();
    let catalog_models = state
        .catalog
        .as_ref()
        .map(|c| c.models.clone())
        .unwrap_or_default();
    let advisories = state
        .catalog
        .as_ref()
        .map(|c| c.advisories.clone())
        .unwrap_or_default();
    let memory = detect_memory();
    let llama_running = state
        .llama_process
        .lock()
        .map(|g| g.is_some())
        .unwrap_or(false);

    // Catalog models that have already been downloaded get a real arch_meta
    // probe (so the UI can show actual KV-per-token instead of a fixed
    // constant); undownloaded entries return None and the UI falls back to a
    // conservative heuristic.
    let catalog_arch_meta: Vec<Option<ArchMeta>> = catalog_models
        .iter()
        .map(|m| {
            let path = state.layout.models_dir().join(&m.file_name);
            if path.exists() {
                usbuddy_core::gguf::read_arch_meta(&path)
            } else {
                None
            }
        })
        .collect();

    RuntimeStatus {
        message: message.into(),
        version: compiled_version().into(),
        platform: detect_platform(),
        current,
        models: catalog_models.clone(),
        drop_in_models: state.layout.discover_drop_in_models().unwrap_or_default(),
        advisories,
        ram: memory,
        ram_previews: catalog_models
            .iter()
            .zip(catalog_arch_meta.iter())
            .map(|(m, arch)| {
                let kv_bytes_per_token = arch
                    .as_ref()
                    .map(|a| a.kv_bytes_per_token_f16())
                    .unwrap_or(524_288); // non-GQA worst case as a safe fallback
                assess_fit(
                    memory,
                    RamEstimateInput {
                        model_bytes: m.size_bytes,
                        context_tokens: 4_096,
                        kv_bytes_per_token,
                        runtime_overhead_bytes: 512 * 1024 * 1024,
                    },
                )
            })
            .collect(),
        catalog_arch_meta,
        llama_running,
        llama_port: LLAMA_SERVER_PORT,
        idle_timeout_secs: state.idle_timeout_secs,
        last_activity_epoch_secs: state.last_activity.load(Ordering::Relaxed),
    }
}

// ---------------------------------------------------------------------------
// Route handlers — static assets
// ---------------------------------------------------------------------------

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn app_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
}

async fn markdown_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        MARKDOWN_JS,
    )
}

async fn styles_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLES_CSS,
    )
}

async fn icon_png() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/png")],
        axum::body::Bytes::from_static(ICON_PNG),
    )
}

// ---------------------------------------------------------------------------
// Route handlers — API
// ---------------------------------------------------------------------------

async fn api_status(State(state): State<Arc<RuntimeState>>) -> Json<RuntimeStatus> {
    Json(status_payload(&state, "Runtime ready on localhost"))
}

async fn api_launch(
    State(state): State<Arc<RuntimeState>>,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<LaunchResponse>, AppError> {
    touch_activity(&state);
    let model_path = resolve_model_path(&state, &req.model_id)?;

    let model_bytes = req.model_size_bytes.unwrap_or_else(|| {
        state
            .catalog
            .as_ref()
            .and_then(|c| {
                c.models
                    .iter()
                    .find(|m| m.id == req.model_id || m.aliases.iter().any(|a| a == &req.model_id))
                    .map(|m| m.size_bytes)
            })
            .or_else(|| std::fs::metadata(&model_path).ok().map(|m| m.len()))
            .unwrap_or(0)
    });
    let params = LaunchParams {
        model_id: req.model_id.clone(),
        model_path,
        model_bytes,
        context_tokens: req.context_tokens.unwrap_or(4_096),
    };

    let _launching = state.launch_lock.lock().await;
    let decision = start_llama(&state, &params).await?;
    *state.last_launch.lock().unwrap() = Some(params);

    let band_label = match decision.band {
        FitBand::Green => "green",
        FitBand::Yellow => "yellow",
        FitBand::Red => "red",
    };

    Ok(Json(LaunchResponse {
        launched: true,
        model_id: req.model_id,
        llama_port: LLAMA_SERVER_PORT,
        ram_band: band_label.into(),
    }))
}

/// Spawns llama-server for `params` and blocks until it is actually serving.
///
/// Re-runs the RAM-fit gate on every (re)start — available memory on the
/// host may have shifted since the original launch, and Red still refuses
/// (swap-to-disk is the #1 footprint leak). Callers must hold `launch_lock`.
async fn start_llama(state: &RuntimeState, params: &LaunchParams) -> Result<RamDecision, AppError> {
    let memory = detect_memory();
    let decision = assess_fit(
        memory,
        RamEstimateInput {
            model_bytes: params.model_bytes,
            context_tokens: params.context_tokens,
            kv_bytes_per_token: 131_072,
            runtime_overhead_bytes: 512 * 1024 * 1024,
        },
    );
    if decision.band == FitBand::Red {
        return Err(AppError::bad_request(format!(
            "RAM check failed (red band): model requires {} bytes but only {} bytes available. \
             Reduce model size or shorten context length.",
            decision.required_bytes, memory.available_bytes
        )));
    }

    let llama_bin = resolve_llama_server_bin(state)?;
    kill_llama_server(&state.llama_process);

    let child = std::process::Command::new(&llama_bin)
        .arg("--model")
        .arg(&params.model_path)
        .arg("--port")
        .arg(LLAMA_SERVER_PORT.to_string())
        .arg("--ctx-size")
        .arg(params.context_tokens.to_string())
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--no-webui")
        .spawn()
        .map_err(|e| AppError::internal(format!("failed to spawn llama-server: {e}")))?;

    *state.llama_process.lock().unwrap() = Some(child);

    // Block until llama-server's /health reports OK. Without this, the UI
    // unlocks the chat input the instant the process spawns — but
    // llama-server binds its port ~3ms in and only finishes loading weights
    // 5–30s later. Any chat request in that window gets HTTP 503 with
    // {"error":{"message":"Loading model"}}, which the UI renders verbatim.
    // We hold the request open until the engine is actually serving.
    if let Err(reason) = wait_for_llama_ready(&state.llama_process).await {
        kill_llama_server(&state.llama_process);
        return Err(AppError::bad_gateway(format!(
            "llama-server failed to become ready: {reason}"
        )));
    }
    Ok(decision)
}

/// Wake-on-request: if llama-server is gone (idle-unloaded after 5 min, or
/// crashed), relaunch it with the last launch parameters before proxying.
/// No-op while it's alive. Serialized via `launch_lock` so a burst of chat
/// requests after an idle stop triggers exactly one reload.
async fn ensure_llama_running(state: &RuntimeState) -> Result<(), AppError> {
    let _launching = state.launch_lock.lock().await;

    let alive = {
        let mut guard = state
            .llama_process
            .lock()
            .map_err(|_| AppError::internal("llama-server process mutex poisoned"))?;
        match guard.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(None) => true,
                // Exited (crash) — drop the dead handle so we relaunch below.
                Ok(Some(_)) => {
                    guard.take();
                    false
                }
                Err(e) => {
                    return Err(AppError::internal(format!("inspecting llama-server: {e}")));
                }
            },
            None => false,
        }
    };
    if alive {
        return Ok(());
    }

    let params = state
        .last_launch
        .lock()
        .map_err(|_| AppError::internal("last-launch mutex poisoned"))?
        .clone()
        .ok_or_else(|| {
            AppError::bad_request("no model is loaded — launch a model before chatting")
        })?;

    eprintln!(
        "USBuddy: waking llama-server for model '{}' (was idle-unloaded or exited)",
        params.model_id
    );
    start_llama(state, &params).await?;
    touch_activity(state);
    Ok(())
}

/// Polls `/health` on the spawned llama-server until it returns 200, the
/// process exits (load failure), or [`LLAMA_READY_TIMEOUT_SECS`] elapses.
///
/// llama.cpp's /health contract:
/// - 503 + `{"status":"loading model"}` while loading weights
/// - 200 + `{"status":"ok"}` once serving
/// - 500 on internal failure
///
/// A connect-refused before the port binds is also treated as "still
/// starting." Any exit by the child process is fatal — we report it.
async fn wait_for_llama_ready(process: &Mutex<Option<Child>>) -> Result<(), String> {
    let health_url = format!("http://127.0.0.1:{LLAMA_SERVER_PORT}/health");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(|e| format!("build health client: {e}"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(LLAMA_READY_TIMEOUT_SECS);

    loop {
        // Did the child die? If so, no point polling — surface the real cause.
        {
            let mut guard = process
                .lock()
                .map_err(|_| "process mutex poisoned".to_string())?;
            match guard.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(Some(status)) => {
                        return Err(format!(
                            "llama-server exited before becoming ready (status: {status}). \
                             Check the runtime terminal for its error output."
                        ));
                    }
                    Ok(None) => { /* still running, fall through to health probe */ }
                    Err(e) => return Err(format!("inspecting llama-server: {e}")),
                },
                None => {
                    return Err("llama-server was killed before becoming ready".into());
                }
            }
        }

        match client.get(&health_url).send().await {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            // 503 = still loading; anything else non-fatal we just retry.
            Ok(_) => {}
            Err(_) => { /* connect refused / timeout — port not bound yet */ }
        }

        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "timed out after {}s waiting for /health. The model may be too large \
                 for available RAM, the GGUF may be corrupt, or USB I/O is unusually slow.",
                LLAMA_READY_TIMEOUT_SECS
            ));
        }
        tokio::time::sleep(Duration::from_millis(LLAMA_READY_POLL_MS)).await;
    }
}

async fn api_stop(State(state): State<Arc<RuntimeState>>) -> Json<serde_json::Value> {
    // Forget the launch params too: an explicit stop must stay stopped —
    // wake-on-request is only for idle unloads and crashes.
    if let Ok(mut guard) = state.last_launch.lock() {
        guard.take();
    }
    kill_llama_server(&state.llama_process);
    Json(serde_json::json!({ "stopped": true }))
}

/// Initiates a clean shutdown of the whole runtime (kills llama-server,
/// signals the axum graceful-shutdown future, then exits the process so
/// the tray thread also unwinds). Called from the web UI Quit button or
/// from external tooling.
async fn api_shutdown(State(state): State<Arc<RuntimeState>>) -> Json<serde_json::Value> {
    kill_llama_server(&state.llama_process);
    state.shutdown.notify_waiters();
    spawn_exit_backstop();
    Json(serde_json::json!({ "shutting_down": true }))
}

/// Hard-exit fallback for the web shutdown paths. The normal exit happens
/// when graceful shutdown completes and the HTTP thread calls
/// `process::exit` — but a wedged in-flight connection (e.g. an SSE stream
/// in another tab) can stall graceful shutdown indefinitely. An OS thread
/// (not a tokio task — those die with the runtime) guarantees we still go.
fn spawn_exit_backstop() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(750));
        std::process::exit(0);
    });
}

/// Like `/api/shutdown`, but also asks the OS to eject the drive. The eject
/// runs in a detached host-resident helper that retries until the runtime
/// (which lives on the drive) has fully exited and the volume can let go.
async fn api_shutdown_eject(State(state): State<Arc<RuntimeState>>) -> Json<serde_json::Value> {
    kill_llama_server(&state.llama_process);
    let eject_scheduled = match usbuddy_core::eject::spawn_detached_eject(state.layout.root()) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("USBuddy: failed to schedule drive eject: {e}");
            false
        }
    };
    state.shutdown.notify_waiters();
    spawn_exit_backstop();
    Json(serde_json::json!({ "shutting_down": true, "eject_scheduled": eject_scheduled }))
}

/// Transparent reverse-proxy: `/api/chat/**` → llama-server `/v1/chat/**`.
async fn api_chat_proxy(
    State(state): State<Arc<RuntimeState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    touch_activity(&state);
    ensure_llama_running(&state).await?;
    let client = reqwest::Client::new();
    let path = uri.path().replacen("/api/chat", "/v1/chat", 1);
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let upstream = format!("http://127.0.0.1:{LLAMA_SERVER_PORT}{path}{query}");

    let body_bytes = axum::body::to_bytes(body, 10 * 1024 * 1024)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;

    let mut upstream_req = client.request(method, &upstream).body(body_bytes);
    for (name, value) in &headers {
        if name == header::HOST {
            continue;
        }
        if let Ok(v) = value.to_str() {
            upstream_req = upstream_req.header(name.as_str(), v);
        }
    }

    let upstream_resp = upstream_req
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("llama-server unreachable: {e}")))?;

    let status = StatusCode::from_u16(upstream_resp.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut resp_headers = HeaderMap::new();
    for (name, value) in upstream_resp.headers() {
        resp_headers.insert(name, value.clone());
    }
    // Stream the response body through instead of buffering it. This is what
    // makes server-sent-events / token-by-token streaming work end-to-end.
    let stream = upstream_resp.bytes_stream();
    let body = Body::from_stream(stream);
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = resp_headers;
    Ok(response)
}

// ---------------------------------------------------------------------------
// Route handlers — prefs & chats
// ---------------------------------------------------------------------------

async fn api_get_prefs(State(state): State<Arc<RuntimeState>>) -> Json<chats::RuntimePrefs> {
    Json(chats::RuntimePrefs::load(
        &state.layout.runtime_prefs_path(),
    ))
}

async fn api_put_prefs(
    State(state): State<Arc<RuntimeState>>,
    Json(prefs): Json<chats::RuntimePrefs>,
) -> Result<Json<chats::RuntimePrefs>, AppError> {
    prefs
        .save(&state.layout.runtime_prefs_path())
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(prefs))
}

async fn api_list_chats(
    State(state): State<Arc<RuntimeState>>,
) -> Result<Json<Vec<chats::ChatSummary>>, AppError> {
    chats::list(&state.layout.chats_dir())
        .map(Json)
        .map_err(|e| AppError::internal(e.to_string()))
}

async fn api_get_chat(
    State(state): State<Arc<RuntimeState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<chats::Chat>, AppError> {
    match chats::read(&state.layout.chats_dir(), &id) {
        Ok(c) => Ok(Json(c)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(AppError::bad_request("chat not found"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
            Err(AppError::bad_request(e.to_string()))
        }
        Err(e) => Err(AppError::internal(e.to_string())),
    }
}

async fn api_put_chat(
    State(state): State<Arc<RuntimeState>>,
    AxumPath(id): AxumPath<String>,
    Json(mut chat): Json<chats::Chat>,
) -> Result<Json<chats::Chat>, AppError> {
    // Don't let a mismatched body silently save under the URL id.
    chat.id = id;
    chats::write(&state.layout.chats_dir(), &chat)
        .map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(chat))
}

async fn api_delete_chat(
    State(state): State<Arc<RuntimeState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    chats::delete(&state.layout.chats_dir(), &id).map_err(|e| AppError::internal(e.to_string()))?;
    Ok(Json(serde_json::json!({ "deleted": true })))
}

// ---------------------------------------------------------------------------
// llama-server helpers
// ---------------------------------------------------------------------------

fn resolve_model_path(state: &RuntimeState, model_id: &str) -> Result<PathBuf, AppError> {
    if let Some(catalog) = &state.catalog
        && let Some(entry) = catalog
            .models
            .iter()
            .find(|m| m.id == model_id || m.aliases.contains(&model_id.to_string()))
    {
        let path = state.layout.models_dir().join(&entry.file_name);
        if path.exists() {
            return Ok(path);
        }
    }
    if let Ok(drops) = state.layout.discover_drop_in_models()
        && let Some(drop) = drops.iter().find(|d| {
            d.path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s == model_id)
                .unwrap_or(false)
        })
    {
        return Ok(drop.path.clone());
    }
    Err(AppError::bad_request(format!(
        "model '{model_id}' not found on drive"
    )))
}

fn resolve_llama_server_bin(state: &RuntimeState) -> Result<PathBuf, AppError> {
    let current = state
        .layout
        .read_current()
        .map_err(|e| AppError::internal(format!("cannot read current.json: {e}")))?;
    let platform = detect_platform();
    let arch = match platform.arch.as_str() {
        "x86_64" => "x64".to_string(),
        "aarch64" => "arm64".to_string(),
        other => other.to_string(),
    };
    let bin_name = if cfg!(target_os = "windows") {
        "llama-server.exe"
    } else {
        "llama-server"
    };
    let candidates = [
        state
            .layout
            .version_dir(&current.active)
            .join("bin")
            .join(format!("{}-{arch}", platform.os))
            .join(bin_name),
        state
            .layout
            .version_dir(&current.active)
            .join("bin")
            .join(format!("{}-{}", platform.os, platform.arch))
            .join(bin_name),
        state
            .layout
            .version_dir(&current.active)
            .join("bin")
            .join(&platform.os)
            .join(bin_name),
        state
            .layout
            .version_dir(&current.active)
            .join("bin")
            .join(bin_name),
    ];
    candidates.into_iter().find(|p| p.exists()).ok_or_else(|| {
        AppError::internal(format!(
            "llama-server not found on drive for version {} ({}-{arch}). \
                 Provision it with `usbuddy-installer-cli engine install <drive>`.",
            current.active, platform.os
        ))
    })
}

fn kill_llama_server(process: &Mutex<Option<Child>>) {
    if let Ok(mut guard) = process.lock()
        && let Some(mut child) = guard.take()
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }
    fn bad_gateway(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: msg.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Browser open helper
// ---------------------------------------------------------------------------

fn open_browser_best_effort(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .status()
            .map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .status()
            .map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .status()
            .map(|_| ())
    }
}

// ---------------------------------------------------------------------------
// API data types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct RuntimeStatus {
    message: String,
    version: String,
    platform: usbuddy_core::platform::PlatformInfo,
    current: Option<usbuddy_core::layout::CurrentVersionPointer>,
    models: Vec<ModelEntry>,
    drop_in_models: Vec<DropInModel>,
    advisories: Vec<Advisory>,
    ram: usbuddy_core::ram::MemorySnapshot,
    ram_previews: Vec<RamDecision>,
    /// Parallel to `models` — Some(meta) when the model is on disk and we
    /// could parse its GGUF header, None when undownloaded or non-GGUF.
    catalog_arch_meta: Vec<Option<ArchMeta>>,
    llama_running: bool,
    llama_port: u16,
    idle_timeout_secs: u64,
    last_activity_epoch_secs: u64,
}

#[derive(Debug, Deserialize)]
struct LaunchRequest {
    model_id: String,
    model_size_bytes: Option<u64>,
    context_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
struct LaunchResponse {
    launched: bool,
    model_id: String,
    llama_port: u16,
    ram_band: String,
}
