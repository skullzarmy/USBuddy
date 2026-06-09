use std::{
    net::SocketAddr,
    path::PathBuf,
    process::Child,
    sync::{Arc, Mutex},
};

use anyhow::Context;
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use usbuddy_core::{
    catalog::{Advisory, Catalog, ModelEntry, load_catalog},
    compiled_version,
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

/// Port used internally by llama-server; separate from the runtime's own port.
const LLAMA_SERVER_PORT: u16 = 8766;

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
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        RuntimeCommand::Inspect { drive } => {
            let state = load_state(drive)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&status_payload(&state, "Inspection only"))?
            );
        }
        RuntimeCommand::Serve {
            drive,
            port,
            open_browser,
        } => {
            let state = Arc::new(load_state(drive)?);

            // Kill llama-server on Ctrl-C.
            let cleanup_state = state.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                kill_llama_server(&cleanup_state.llama_process);
                std::process::exit(0);
            });

            let app = Router::new()
                .route("/", get(index))
                .route("/assets/app.js", get(app_js))
                .route("/assets/styles.css", get(styles_css))
                .route("/api/status", get(api_status))
                .route("/api/launch", post(api_launch))
                .route("/api/stop", post(api_stop))
                .route("/api/chat", post(api_chat_proxy))
                .route("/api/chat/{*rest}", post(api_chat_proxy))
                .with_state(state.clone());

            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            let listener = TcpListener::bind(addr).await?;
            let url = format!("http://{addr}");
            eprintln!("USBuddy runtime serving on {url}");
            if open_browser {
                let _ = open_browser_best_effort(&url);
            }
            axum::serve(listener, app)
                .await
                .context("runtime HTTP server exited unexpectedly")?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// State helpers
// ---------------------------------------------------------------------------

fn load_state(drive: PathBuf) -> anyhow::Result<RuntimeState> {
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
            .map(|m| {
                assess_fit(
                    memory,
                    RamEstimateInput {
                        model_bytes: m.size_bytes,
                        context_tokens: 4_096,
                        kv_bytes_per_token: 131_072,
                        runtime_overhead_bytes: 512 * 1024 * 1024,
                    },
                )
            })
            .collect(),
        llama_running,
        llama_port: LLAMA_SERVER_PORT,
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
    let model_path = resolve_model_path(&state, &req.model_id)?;

    // RAM-fit check.
    let memory = detect_memory();
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
    let decision = assess_fit(
        memory,
        RamEstimateInput {
            model_bytes,
            context_tokens: req.context_tokens.unwrap_or(4_096),
            kv_bytes_per_token: 131_072,
            runtime_overhead_bytes: 512 * 1024 * 1024,
        },
    );
    if decision.band == FitBand::Red {
        return Err(AppError::bad_request(format!(
            "RAM check failed (red band): model requires {} bytes but only {} available. \
             Reduce model size or shorten context length.",
            decision.required_bytes, decision.host_headroom_bytes
        )));
    }

    let llama_bin = resolve_llama_server_bin(&state)?;
    kill_llama_server(&state.llama_process);

    let child = std::process::Command::new(&llama_bin)
        .arg("--model")
        .arg(&model_path)
        .arg("--port")
        .arg(LLAMA_SERVER_PORT.to_string())
        .arg("--ctx-size")
        .arg(req.context_tokens.unwrap_or(4_096).to_string())
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--no-webui")
        .spawn()
        .map_err(|e| AppError::internal(format!("failed to spawn llama-server: {e}")))?;

    *state.llama_process.lock().unwrap() = Some(child);

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

async fn api_stop(State(state): State<Arc<RuntimeState>>) -> Json<serde_json::Value> {
    kill_llama_server(&state.llama_process);
    Json(serde_json::json!({ "stopped": true }))
}

/// Transparent reverse-proxy: `/api/chat/**` → llama-server `/v1/chat/**`.
async fn api_chat_proxy(
    State(_state): State<Arc<RuntimeState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    let client = reqwest::Client::new();
    let path = uri.path().replacen("/api/chat", "/v1/chat", 1);
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let upstream = format!("http://127.0.0.1:{LLAMA_SERVER_PORT}{path}{query}");

    let body_bytes = axum::body::to_bytes(body, usize::MAX)
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
    let bytes = upstream_resp
        .bytes()
        .await
        .map_err(|e| AppError::bad_gateway(format!("error reading llama-server response: {e}")))?;

    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    *response.headers_mut() = resp_headers;
    Ok(response)
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
        // Allow llama-server on PATH for development/testing.
        PathBuf::from(bin_name),
    ];
    candidates
        .into_iter()
        .find(|p| p.exists() || p.components().count() == 1)
        .ok_or_else(|| {
            AppError::internal(format!(
                "llama-server binary not found for version {}",
                current.active
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
    llama_running: bool,
    llama_port: u16,
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
