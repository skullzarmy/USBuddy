use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use axum::{
    Json, Router,
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
};
use clap::{Parser, Subcommand};
use serde::Serialize;
use tokio::net::TcpListener;
use usbuddy_core::{
    catalog::{Advisory, Catalog, ModelEntry, load_catalog},
    compiled_version,
    layout::DriveLayout,
    platform::detect_platform,
    ram::{RamDecision, RamEstimateInput, assess_fit, detect_memory},
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

#[derive(Debug, Parser)]
#[command(name = "usbuddy-runtime", version = compiled_version(), about = "USBuddy portable runtime wrapper")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        drive: PathBuf,
        #[arg(long, default_value_t = 8765)]
        port: u16,
        #[arg(long, default_value_t = false)]
        open_browser: bool,
    },
    Inspect {
        #[arg(long)]
        drive: PathBuf,
    },
}

#[derive(Debug, Clone)]
struct RuntimeState {
    layout: DriveLayout,
    catalog: Option<Catalog>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Inspect { drive } => {
            let state = load_state(drive)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&status_payload(&state, "Inspection only"))?
            );
        }
        Command::Serve {
            drive,
            port,
            open_browser,
        } => {
            let state = Arc::new(load_state(drive)?);
            let app = Router::new()
                .route("/", get(index))
                .route("/assets/app.js", get(app_js))
                .route("/assets/styles.css", get(styles_css))
                .route("/api/status", get(api_status))
                .with_state(state.clone());
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            let listener = TcpListener::bind(addr).await?;
            let url = format!("http://{}", addr);
            eprintln!("USBuddy runtime serving on {url}");
            if open_browser {
                let _ = open_browser_best_effort(&url);
            }
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await
                .context("runtime HTTP server exited unexpectedly")?;
        }
    }
    Ok(())
}

fn load_state(drive: PathBuf) -> anyhow::Result<RuntimeState> {
    let layout = DriveLayout::new(drive);
    let catalog = if layout.catalog_path().exists() {
        Some(load_catalog(&layout.catalog_path())?)
    } else {
        None
    };
    Ok(RuntimeState { layout, catalog })
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn app_js() -> impl IntoResponse {
    [("content-type", "application/javascript; charset=utf-8")].into_response_body(APP_JS)
}

async fn styles_css() -> impl IntoResponse {
    [("content-type", "text/css; charset=utf-8")].into_response_body(STYLES_CSS)
}

async fn api_status(State(state): State<Arc<RuntimeState>>) -> Json<RuntimeStatus> {
    Json(status_payload(&state, "Runtime ready on localhost"))
}

fn status_payload(state: &RuntimeState, message: &str) -> RuntimeStatus {
    let current = state.layout.read_current().ok();
    let catalog_models = state
        .catalog
        .as_ref()
        .map(|catalog| catalog.models.clone())
        .unwrap_or_default();
    let advisories = state
        .catalog
        .as_ref()
        .map(|catalog| catalog.advisories.clone())
        .unwrap_or_default();
    let memory = detect_memory();
    let ram_decision = catalog_models.first().map(|model| {
        assess_fit(
            memory,
            RamEstimateInput {
                model_bytes: model.size_bytes,
                context_tokens: 4_096,
                kv_bytes_per_token: 131_072,
                runtime_overhead_bytes: 512 * 1024 * 1024,
            },
        )
    });

    RuntimeStatus {
        message: message.into(),
        version: compiled_version().into(),
        platform: detect_platform(),
        current,
        models: catalog_models,
        drop_in_models: state.layout.discover_drop_in_models().unwrap_or_default(),
        advisories,
        ram_decision,
    }
}

trait IntoResponseBody {
    fn into_response_body(self, body: &'static str) -> axum::response::Response;
}

impl IntoResponseBody for [(&'static str, &'static str); 1] {
    fn into_response_body(self, body: &'static str) -> axum::response::Response {
        (self, body).into_response()
    }
}

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

#[derive(Debug, Serialize)]
struct RuntimeStatus {
    message: String,
    version: String,
    platform: usbuddy_core::platform::PlatformInfo,
    current: Option<usbuddy_core::layout::CurrentVersionPointer>,
    models: Vec<ModelEntry>,
    drop_in_models: Vec<usbuddy_core::layout::DropInModel>,
    advisories: Vec<Advisory>,
    ram_decision: Option<RamDecision>,
}
