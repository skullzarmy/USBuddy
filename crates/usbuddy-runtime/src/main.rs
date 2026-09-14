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
    extract::{Path as AxumPath, Request, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use usbuddy_core::{
    bridge,
    catalog::{Advisory, Catalog, ModelEntry, load_catalog},
    compiled_version,
    gguf::ArchMeta,
    layout::{DriveLayout, DropInModel},
    platform::detect_platform,
    ram::{FitBand, RamDecision, RamEstimateInput, assess_fit, detect_memory},
};

// The web UI is a React SPA built by Vite into ui/web/dist with fixed
// filenames (no content hashes) precisely so these embeds stay stable.
// The built dist/ is committed, so a plain `cargo build` needs no npm.
// After changing ui/web sources, run `npm --prefix ui/web run build`.
const INDEX_HTML: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ui/web/dist/index.html"
));
const APP_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ui/web/dist/assets/app.js"
));
const STYLES_CSS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../ui/web/dist/assets/styles.css"
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

/// Default port for the runtime's own HTTP server (chat UI + `/api` + the
/// editor bridge's `/v1`).
const DEFAULT_PORT: u16 = 8765;

/// Request-body ceiling for the chat UI proxy.
const CHAT_BODY_LIMIT_BYTES: usize = 10 * 1024 * 1024;

/// Request-body ceiling for the editor bridge. Higher than the chat UI's:
/// agentic clients (Cline, Roo Code) attach whole files, and a hard failure
/// at the transport layer is far more confusing than a context-overflow
/// error from the model.
const BRIDGE_BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;

/// Conservative KV-cache fallback (non-GQA worst case) for models whose GGUF
/// header we couldn't parse. Matches the figure the UI's RAM preview uses, so
/// the gate and the badge never disagree.
const FALLBACK_KV_BYTES_PER_TOKEN: u64 = 524_288;

/// Non-KV runtime overhead assumed by the RAM advisor for a llama-server
/// process (weights and KV cache are accounted separately).
const RUNTIME_OVERHEAD_BYTES: u64 = 512 * 1024 * 1024;

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
        #[arg(long, default_value_t = DEFAULT_PORT)]
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
    /// Port this runtime is listening on. Needed by the Origin allowlist and
    /// by the bridge panel, which shows the user the base URL to paste.
    port: u16,
    /// In-RAM cache of the bridge bearer token so authenticating a request
    /// doesn't hit the USB drive on every call. `None` until first read.
    bridge_token: Arc<Mutex<Option<String>>>,
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
            let state = load_state(drive, DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_PORT)?;
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
    let state = Arc::new(load_state(drive, idle_timeout_secs, port)?);
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
        .route("/api/bridge", get(api_get_bridge).put(api_put_bridge))
        .route("/api/bridge/rotate", post(api_rotate_bridge_token))
        // OpenAI-compatible editor bridge. Token-gated and disabled by
        // default — see docs/EDITOR-INTEGRATION.md.
        .route("/v1/models", get(v1_models))
        .route("/v1/chat/completions", post(v1_chat_completions))
        .route("/v1/completions", post(v1_completions))
        .layer(middleware::from_fn_with_state(state.clone(), origin_guard))
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

fn load_state(drive: PathBuf, idle_timeout_secs: u64, port: u16) -> anyhow::Result<RuntimeState> {
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
        port,
        bridge_token: Arc::new(Mutex::new(None)),
    })
}

/// Catalog entries whose GGUF is actually present in `models/`.
///
/// The stick UI is a launcher, not a storefront: the full catalog lives in the
/// installer, and both the status payload and the bridge's `/v1/models` offer
/// only what can actually be loaded right now.
fn present_catalog_models(state: &RuntimeState) -> Vec<ModelEntry> {
    state
        .catalog
        .as_ref()
        .map(|c| {
            c.models
                .iter()
                .filter(|m| state.layout.models_dir().join(&m.file_name).exists())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Loose `.gguf` files in `models/` that have no catalog entry. A downloaded
/// catalog model is not listed twice.
fn present_drop_in_models(state: &RuntimeState, catalog_models: &[ModelEntry]) -> Vec<DropInModel> {
    state
        .layout
        .discover_drop_in_models()
        .unwrap_or_default()
        .into_iter()
        .filter(|d| {
            d.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| !catalog_models.iter().any(|m| m.file_name == n))
                .unwrap_or(true)
        })
        .collect()
}

/// The id a drop-in model is addressed by: its filename without `.gguf`.
/// Matches what `resolve_model_path` accepts and what the web UI derives.
fn drop_in_id(model: &DropInModel) -> Option<String> {
    model
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

fn status_payload(state: &RuntimeState, message: &str) -> RuntimeStatus {
    let current = state.layout.read_current().ok();
    let catalog_models = present_catalog_models(state);
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

    // All listed models are on disk, so each gets a real arch_meta probe
    // (actual KV-per-token); non-GGUF parse failures return None and the UI
    // falls back to a conservative heuristic.
    let catalog_arch_meta: Vec<Option<ArchMeta>> = catalog_models
        .iter()
        .map(|m| usbuddy_core::gguf::read_arch_meta(&state.layout.models_dir().join(&m.file_name)))
        .collect();

    let drop_in_models = present_drop_in_models(state, &catalog_models);

    RuntimeStatus {
        message: message.into(),
        version: compiled_version().into(),
        platform: detect_platform(),
        current,
        models: catalog_models.clone(),
        drop_in_models,
        advisories,
        ram: memory,
        ram_previews: catalog_models
            .iter()
            .zip(catalog_arch_meta.iter())
            .map(|(m, arch)| {
                let kv_bytes_per_token = arch
                    .as_ref()
                    .map(|a| a.kv_bytes_per_token_f16())
                    .unwrap_or(FALLBACK_KV_BYTES_PER_TOKEN);
                assess_fit(
                    memory,
                    RamEstimateInput {
                        model_bytes: m.size_bytes,
                        context_tokens: 4_096,
                        kv_bytes_per_token,
                        runtime_overhead_bytes: RUNTIME_OVERHEAD_BYTES,
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
// Origin allowlist
// ---------------------------------------------------------------------------

/// Rejects cross-site browser requests to the whole HTTP surface.
///
/// The runtime listens on loopback, but loopback is not a security boundary
/// against a *browser*: any page the user happens to visit can issue requests
/// to `http://127.0.0.1:8765` from their tab. Without this, a random website
/// could drive the model, enumerate saved chats, or hit `/api/shutdown-eject`.
///
/// Non-browser clients (every editor, extension host, curl) send no `Origin`
/// and pass through untouched; that's what keeps the bridge usable. Browsers
/// attach `Origin` to cross-origin requests and to same-origin non-GET ones,
/// so the chat UI's own calls are matched by the allowlist rather than waved
/// through. `*` is never used.
async fn origin_guard(
    State(state): State<Arc<RuntimeState>>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());
    if !origin_allowed(origin, state.port) {
        return Err(AppError {
            status: StatusCode::FORBIDDEN,
            message: format!(
                "cross-origin request rejected (Origin: {}). The USBuddy runtime only \
                 accepts requests from its own UI or from non-browser clients.",
                origin.unwrap_or("<none>")
            ),
            openai_type: None,
        });
    }
    Ok(next.run(req).await)
}

/// `None` (no Origin header) means a non-browser client — allowed. Anything
/// else must be this server's own origin, spelled either way round.
fn origin_allowed(origin: Option<&str>, port: u16) -> bool {
    match origin {
        None => true,
        Some(o) => {
            let o = o.trim();
            o == format!("http://127.0.0.1:{port}")
                || o == format!("http://localhost:{port}")
                || o == format!("http://[::1]:{port}")
        }
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
    // Price the KV cache from the model's own attention shape rather than a
    // single constant. This matters much more now that the editor bridge runs
    // 16K+ contexts, and it makes the gate agree with the band the UI already
    // previews for the same model.
    let kv_bytes_per_token = usbuddy_core::gguf::read_arch_meta(&params.model_path)
        .map(|a| a.kv_bytes_per_token_f16())
        .unwrap_or(FALLBACK_KV_BYTES_PER_TOKEN);
    let decision = assess_fit(
        memory,
        RamEstimateInput {
            model_bytes: params.model_bytes,
            context_tokens: params.context_tokens,
            kv_bytes_per_token,
            runtime_overhead_bytes: RUNTIME_OVERHEAD_BYTES,
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
        // Apply the model's own chat template. Without --jinja llama-server
        // never emits OpenAI-shaped `tool_calls`, which means agent mode in
        // Cline/Continue/Roo simply doesn't work — and templated models get
        // better-formed prompts in the chat UI too.
        .arg("--jinja")
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

    if llama_alive(state)? {
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

/// Is llama-server still up? Reaps the handle if the child has exited so the
/// caller can relaunch. Callers must hold `launch_lock`.
fn llama_alive(state: &RuntimeState) -> Result<bool, AppError> {
    let mut guard = state
        .llama_process
        .lock()
        .map_err(|_| AppError::internal("llama-server process mutex poisoned"))?;
    match guard.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(None) => Ok(true),
            // Exited (crash) — drop the dead handle so the caller relaunches.
            Ok(Some(_)) => {
                guard.take();
                Ok(false)
            }
            Err(e) => Err(AppError::internal(format!("inspecting llama-server: {e}"))),
        },
        None => Ok(false),
    }
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
    let path = uri.path().replacen("/api/chat", "/v1/chat", 1);
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let body_bytes = axum::body::to_bytes(body, CHAT_BODY_LIMIT_BYTES)
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    proxy_to_llama(method, &path, &query, &headers, body_bytes).await
}

/// Forwards an already-buffered request to llama-server and streams the
/// response straight back. Shared by the chat UI proxy and the editor bridge.
///
/// `path` is the upstream path (`/v1/chat/completions`), `query` the raw
/// query string including its leading `?`, or empty.
async fn proxy_to_llama(
    method: Method,
    path: &str,
    query: &str,
    headers: &HeaderMap,
    body_bytes: axum::body::Bytes,
) -> Result<Response, AppError> {
    let client = reqwest::Client::new();
    let upstream = format!("http://127.0.0.1:{LLAMA_SERVER_PORT}{path}{query}");

    let mut upstream_req = client.request(method, &upstream).body(body_bytes);
    for (name, value) in headers {
        // HOST belongs to us, not upstream. The credentials are ours too:
        // the bridge token authenticates the client to USBuddy and has no
        // meaning to llama-server, so it stops here rather than leaking into
        // a subprocess's logs. CONTENT_LENGTH is recomputed by reqwest.
        if name == header::HOST
            || name == header::AUTHORIZATION
            || name == header::CONTENT_LENGTH
            || name.as_str().eq_ignore_ascii_case("x-api-key")
        {
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

/// Accepts a partial update. The chat header (incognito) and the bridge panel
/// both write prefs and each knows only its own fields — a full-object PUT
/// from either would silently clobber the other's settings.
async fn api_put_prefs(
    State(state): State<Arc<RuntimeState>>,
    Json(patch): Json<chats::RuntimePrefsPatch>,
) -> Result<Json<chats::RuntimePrefs>, AppError> {
    let path = state.layout.runtime_prefs_path();
    let mut prefs = chats::RuntimePrefs::load(&path);
    prefs.apply(&patch);
    prefs
        .save(&path)
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
// Route handlers — editor bridge (OpenAI-compatible /v1)
// ---------------------------------------------------------------------------

/// Reads the bridge bearer token, caching it in RAM so authenticating a
/// request never touches the USB drive twice.
///
/// `create` materializes a token when none exists. That's reserved for paths
/// the user explicitly drove — enabling the bridge, rotating the token, or
/// serving a request while the bridge is already enabled. Merely *reading*
/// the bridge panel must not write to the drive.
fn bridge_token(state: &RuntimeState, create: bool) -> Result<Option<String>, AppError> {
    {
        let cached = state
            .bridge_token
            .lock()
            .map_err(|_| AppError::internal("bridge token mutex poisoned"))?;
        if let Some(token) = cached.as_ref() {
            return Ok(Some(token.clone()));
        }
    }

    let path = state.layout.bridge_token_path();
    let token = if create {
        Some(
            bridge::load_or_create_token(&path)
                .map_err(|e| AppError::internal(format!("bridge token: {e}")))?,
        )
    } else {
        std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    if let Some(token) = &token {
        *state
            .bridge_token
            .lock()
            .map_err(|_| AppError::internal("bridge token mutex poisoned"))? = Some(token.clone());
    }
    Ok(token)
}

/// Gate for every `/v1` route: the bridge must be enabled and the caller must
/// present the token. Returns the prefs so callers get `bridge_ctx_tokens`
/// without reading the file twice.
///
/// Prefs are read per request rather than cached so the UI toggle takes effect
/// immediately and a hand-edited `runtime-prefs.toml` is honored.
fn bridge_guard(
    state: &RuntimeState,
    headers: &HeaderMap,
) -> Result<chats::RuntimePrefs, AppError> {
    let prefs = chats::RuntimePrefs::load(&state.layout.runtime_prefs_path());
    if !prefs.bridge_enabled {
        return Err(AppError {
            status: StatusCode::NOT_FOUND,
            message: "the USBuddy editor bridge is disabled. Turn on \"Developer bridge\" \
                      in the USBuddy chat UI sidebar to enable it."
                .into(),
            openai_type: Some("invalid_request_error"),
        });
    }

    let presented = headers
        .get(header::AUTHORIZATION)
        .or_else(|| headers.get("x-api-key"))
        .and_then(|v| v.to_str().ok())
        .and_then(bridge::parse_bearer);

    // The bridge is on, so the user has opted in — materialize the token if
    // the file went missing rather than wedging every request.
    let stored = bridge_token(state, true)?.unwrap_or_default();

    match presented {
        Some(token) if bridge::token_matches(token, &stored) => Ok(prefs),
        _ => Err(AppError {
            status: StatusCode::UNAUTHORIZED,
            message: "invalid or missing API key. Copy the token from the USBuddy \
                      chat UI (sidebar → Developer bridge) into your editor's \
                      API-key field."
                .into(),
            openai_type: Some("invalid_request_error"),
        }),
    }
}

/// Every id the drive can serve — catalog ids first, then drop-in file stems.
fn bridge_model_ids(state: &RuntimeState) -> Vec<String> {
    let catalog_models = present_catalog_models(state);
    let drop_ins = present_drop_in_models(state, &catalog_models);
    catalog_models
        .iter()
        .map(|m| m.id.clone())
        .chain(drop_ins.iter().filter_map(drop_in_id))
        .collect()
}

/// Turns a client-supplied model name into launch parameters, or `None` when
/// the drive can't serve it. Accepts catalog ids, catalog aliases, and
/// drop-in file stems — the same vocabulary `resolve_model_path` accepts.
fn resolve_bridge_model(state: &RuntimeState, name: &str, ctx_tokens: u32) -> Option<LaunchParams> {
    let catalog_models = present_catalog_models(state);
    let (model_id, model_path, model_bytes) = if let Some(entry) = catalog_models
        .iter()
        .find(|m| m.id == name || m.aliases.iter().any(|a| a == name))
    {
        let path = state.layout.models_dir().join(&entry.file_name);
        (entry.id.clone(), path, entry.size_bytes)
    } else {
        let drop_in = present_drop_in_models(state, &catalog_models)
            .into_iter()
            .find(|d| drop_in_id(d).as_deref() == Some(name))?;
        let bytes = if drop_in.size_bytes > 0 {
            drop_in.size_bytes
        } else {
            std::fs::metadata(&drop_in.path)
                .map(|m| m.len())
                .unwrap_or(0)
        };
        (name.to_string(), drop_in.path, bytes)
    };

    // Never ask llama-server for more context than the model was trained for.
    let trained_cap = usbuddy_core::gguf::read_arch_meta(&model_path).map(|a| a.context_length);
    Some(LaunchParams {
        model_id,
        model_path,
        model_bytes,
        context_tokens: bridge::clamp_ctx_tokens(ctx_tokens, trained_cap),
    })
}

/// Makes llama-server ready to serve `requested`, starting it cold, waking it
/// after an idle-unload, or swapping models as needed. Returns the id actually
/// being served.
///
/// The chat UI's launch flow can't be a prerequisite here: an editor connects
/// whenever it likes and names its model in the request body.
async fn ensure_bridge_model(
    state: &RuntimeState,
    requested: Option<&str>,
    ctx_tokens: u32,
) -> Result<String, AppError> {
    let _launching = state.launch_lock.lock().await;
    let running = llama_alive(state)?;
    let loaded = state
        .last_launch
        .lock()
        .map_err(|_| AppError::internal("last-launch mutex poisoned"))?
        .clone();

    let resolved = requested
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .and_then(|m| resolve_bridge_model(state, m, ctx_tokens));

    let params = match resolved {
        Some(p) => p,
        // Unrecognized or absent model name. If we already have something
        // loaded (or idle-unloaded but remembered), serve that instead of
        // failing a request we can obviously satisfy — clients hardcode and
        // mangle model names, and a working completion beats a 400.
        None => match loaded.clone() {
            Some(prev) => prev,
            None => {
                let ids = bridge_model_ids(state);
                let available = if ids.is_empty() {
                    "none — this drive has no models installed".to_string()
                } else {
                    ids.join(", ")
                };
                return Err(AppError::bad_request(format!(
                    "model '{}' is not on this drive. Available: {available}",
                    requested.unwrap_or("").trim()
                ))
                .with_openai_type("invalid_request_error"));
            }
        },
    };

    // Already serving exactly this? Then nothing to do. The context is part
    // of the comparison: raising the slider in the bridge panel has to take
    // effect on the next request, and both sides are post-clamp so a model
    // whose trained cap is below the pref doesn't reload forever.
    let already_serving = running
        && loaded.as_ref().is_some_and(|p| {
            p.model_id == params.model_id && p.context_tokens == params.context_tokens
        });
    if already_serving {
        return Ok(params.model_id);
    }

    // Cold start, wake-after-idle, crash recovery, a model swap, or a context
    // change. The chat UI and the bridge share one llama-server, so naming a
    // different model here swaps it out from under an open chat tab — same as
    // Ollama.
    eprintln!(
        "USBuddy bridge: starting model '{}' ({} ctx)",
        params.model_id, params.context_tokens
    );
    let model_id = params.model_id.clone();
    start_llama(state, &params)
        .await
        .map_err(AppError::into_openai)?;
    *state
        .last_launch
        .lock()
        .map_err(|_| AppError::internal("last-launch mutex poisoned"))? = Some(params);
    Ok(model_id)
}

async fn v1_models(
    State(state): State<Arc<RuntimeState>>,
    headers: HeaderMap,
) -> Result<Json<OpenAiModelList>, AppError> {
    bridge_guard(&state, &headers)?;
    let created = now_epoch_secs();
    Ok(Json(OpenAiModelList {
        object: "list",
        data: bridge_model_ids(&state)
            .into_iter()
            .map(|id| OpenAiModel {
                id,
                object: "model",
                created,
                owned_by: "usbuddy",
            })
            .collect(),
    }))
}

async fn v1_chat_completions(
    State(state): State<Arc<RuntimeState>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    bridge_completion(state, headers, body, "/v1/chat/completions").await
}

async fn v1_completions(
    State(state): State<Arc<RuntimeState>>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    bridge_completion(state, headers, body, "/v1/completions").await
}

/// Shared body of both bridge completion routes: authenticate, buffer the
/// request (we need to read `model` out of it), make that model ready, then
/// hand the original bytes to llama-server unmodified.
async fn bridge_completion(
    state: Arc<RuntimeState>,
    headers: HeaderMap,
    body: Body,
    upstream_path: &str,
) -> Result<Response, AppError> {
    let prefs = bridge_guard(&state, &headers)?;
    touch_activity(&state);

    let body_bytes = axum::body::to_bytes(body, BRIDGE_BODY_LIMIT_BYTES)
        .await
        .map_err(|e| {
            AppError::bad_request(format!("request body too large or unreadable: {e}"))
                .with_openai_type("invalid_request_error")
        })?;

    let requested = serde_json::from_slice::<serde_json::Value>(&body_bytes)
        .ok()
        .and_then(|v| {
            v.get("model")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        });

    ensure_bridge_model(&state, requested.as_deref(), prefs.bridge_ctx_tokens).await?;
    touch_activity(&state);

    // llama-server ignores the `model` field, so the body goes through as-is.
    proxy_to_llama(Method::POST, upstream_path, "", &headers, body_bytes)
        .await
        .map_err(AppError::into_openai)
}

// ---------------------------------------------------------------------------
// Route handlers — bridge control (used by the chat UI's settings panel)
// ---------------------------------------------------------------------------

fn bridge_info(state: &RuntimeState, create_token: bool) -> Result<BridgeInfo, AppError> {
    let prefs = chats::RuntimePrefs::load(&state.layout.runtime_prefs_path());
    Ok(BridgeInfo {
        enabled: prefs.bridge_enabled,
        ctx_tokens: prefs.bridge_ctx_tokens,
        base_url: format!("http://127.0.0.1:{}/v1", state.port),
        token: bridge_token(state, create_token)?,
        models: bridge_model_ids(state),
        min_ctx_tokens: bridge::MIN_BRIDGE_CTX_TOKENS,
    })
}

async fn api_get_bridge(
    State(state): State<Arc<RuntimeState>>,
) -> Result<Json<BridgeInfo>, AppError> {
    // Read-only: never mints a token just because the panel was opened.
    bridge_info(&state, false).map(Json)
}

async fn api_put_bridge(
    State(state): State<Arc<RuntimeState>>,
    Json(patch): Json<chats::RuntimePrefsPatch>,
) -> Result<Json<BridgeInfo>, AppError> {
    let prefs_path = state.layout.runtime_prefs_path();
    let mut prefs = chats::RuntimePrefs::load(&prefs_path);
    prefs.apply(&patch);
    prefs
        .save(&prefs_path)
        .map_err(|e| AppError::internal(format!("saving prefs: {e}")))?;
    // Turning the bridge on is the user action that mints the token.
    bridge_info(&state, prefs.bridge_enabled).map(Json)
}

async fn api_rotate_bridge_token(
    State(state): State<Arc<RuntimeState>>,
) -> Result<Json<BridgeInfo>, AppError> {
    let token = bridge::rotate_token(&state.layout.bridge_token_path())
        .map_err(|e| AppError::internal(format!("rotating bridge token: {e}")))?;
    *state
        .bridge_token
        .lock()
        .map_err(|_| AppError::internal("bridge token mutex poisoned"))? = Some(token);
    bridge_info(&state, false).map(Json)
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
    /// When set, the body is rendered in OpenAI's `{"error":{"message",…}}`
    /// envelope instead of USBuddy's flat `{"error": "…"}`. Editor clients
    /// parse the former and will show a raw status code for anything else.
    openai_type: Option<&'static str>,
}

impl AppError {
    fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
            openai_type: None,
        }
    }
    fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
            openai_type: None,
        }
    }
    fn bad_gateway(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: msg.into(),
            openai_type: None,
        }
    }

    /// Marks this error for OpenAI-envelope rendering with an explicit type.
    fn with_openai_type(mut self, error_type: &'static str) -> Self {
        self.openai_type = Some(error_type);
        self
    }

    /// Re-labels an error raised by shared (non-bridge) code so it reaches an
    /// editor in the shape that editor understands. 4xx are the caller's
    /// fault, 5xx are ours.
    fn into_openai(self) -> Self {
        if self.openai_type.is_some() {
            return self;
        }
        let kind = if self.status.is_client_error() {
            "invalid_request_error"
        } else {
            "api_error"
        };
        self.with_openai_type(kind)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = match self.openai_type {
            Some(error_type) => serde_json::json!({
                "error": { "message": self.message, "type": error_type }
            }),
            None => serde_json::json!({ "error": self.message }),
        };
        (self.status, Json(body)).into_response()
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
    /// Parallel to `models` — Some(meta) when we could parse the GGUF
    /// header, None on parse failure (UI falls back to a conservative
    /// KV heuristic). All listed models are present on disk.
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

/// State of the editor bridge, for the chat UI's settings panel.
#[derive(Debug, Serialize)]
struct BridgeInfo {
    enabled: bool,
    ctx_tokens: u32,
    /// What the user pastes into their editor's "base URL" field.
    base_url: String,
    /// `None` until the bridge has been enabled at least once — reading the
    /// panel must not mint a token.
    token: Option<String>,
    models: Vec<String>,
    min_ctx_tokens: u32,
}

#[derive(Debug, Serialize)]
struct OpenAiModel {
    id: String,
    object: &'static str,
    created: u64,
    owned_by: &'static str,
}

#[derive(Debug, Serialize)]
struct OpenAiModelList {
    object: &'static str,
    data: Vec<OpenAiModel>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_browser_clients_pass_the_origin_guard() {
        // Editors, extension hosts, and curl send no Origin at all.
        assert!(origin_allowed(None, 8765));
    }

    #[test]
    fn own_origin_is_allowed_either_spelling() {
        assert!(origin_allowed(Some("http://127.0.0.1:8765"), 8765));
        assert!(origin_allowed(Some("http://localhost:8765"), 8765));
        assert!(origin_allowed(Some("http://[::1]:8765"), 8765));
    }

    #[test]
    fn foreign_origins_are_rejected() {
        assert!(!origin_allowed(Some("https://evil.example"), 8765));
        assert!(!origin_allowed(Some("null"), 8765));
        // Right host, wrong port — another local service, not us.
        assert!(!origin_allowed(Some("http://127.0.0.1:3000"), 8765));
        // https:// to our own port is still not an origin we serve.
        assert!(!origin_allowed(Some("https://127.0.0.1:8765"), 8765));
        // Prefix tricks must not match.
        assert!(!origin_allowed(
            Some("http://127.0.0.1:8765.evil.com"),
            8765
        ));
    }

    #[test]
    fn errors_render_in_the_shape_the_caller_expects() {
        // Flat shape for the chat UI.
        let plain = AppError::bad_request("nope");
        assert!(plain.openai_type.is_none());

        // OpenAI envelope for editor clients, with severity-appropriate type.
        assert_eq!(
            AppError::bad_request("nope").into_openai().openai_type,
            Some("invalid_request_error")
        );
        assert_eq!(
            AppError::internal("boom").into_openai().openai_type,
            Some("api_error")
        );
        // An explicit label is never overwritten.
        assert_eq!(
            AppError::internal("boom")
                .with_openai_type("invalid_request_error")
                .into_openai()
                .openai_type,
            Some("invalid_request_error")
        );
    }
}
