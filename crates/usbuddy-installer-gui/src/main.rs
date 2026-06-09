//! `usbuddy-installer-gui` — an eframe/egui shell over `usbuddy-core`.
//!
//! Designed as a three-step flow:
//!   1. Pick a drive (native folder picker).
//!   2. Refresh / load the catalog (with bundled fallback so it works offline).
//!   3. Browse the model list, click Download on a row.
//!
//! Long-running operations (downloads, network catalog fetch) run on a worker
//! thread and stream log lines back via an mpsc channel.

use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use clap::Parser;
use eframe::egui;
use semver::Version;
use usbuddy_core::{
    catalog::{Catalog, ModelEntry, load_catalog},
    compiled_version,
    download::download_verified,
    engine::{
        DEFAULT_LLAMA_TAG, DEFAULT_RUNTIME_RELEASE_BASE, EngineSelection, EngineStatus,
        EngineTarget, install_engines, install_runtimes_from_release,
        report_status as engine_report_status,
    },
    layout::DriveLayout,
    license::{LicensePrefs, LicenseScope},
    platform::detect_platform,
    ram::{FitBand, MemorySnapshot, RamEstimateInput, assess_fit, detect_memory},
};

const DEFAULT_CATALOG_URL: &str =
    "https://github.com/skullzarmy/USBuddy/releases/latest/download/official.catalog.json";

/// Catalog bundled into the binary at build time. Used as a fallback when
/// the release URL 404s (which it does until a maintainer cuts a release)
/// or there's no network. The user can still browse + download models from
/// this exact snapshot.
const BUNDLED_CATALOG: &str = include_str!("../../../fixtures/catalog/official.catalog.json");

#[derive(Debug, Parser)]
#[command(
    name = "usbuddy-installer-gui",
    version = compiled_version(),
    about = "USBuddy installer GUI (eframe/egui)"
)]
struct Cli {
    /// Path to act as the USB drive root. May be supplied later via the UI.
    #[arg(long)]
    drive: Option<PathBuf>,
}

enum Job {
    Log(String),
    CatalogLoaded(Box<Catalog>, &'static str),
    EngineStatus(Vec<EngineStatus>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CatalogSource {
    None,
    Drive,
    Bundled,
    Network,
}

impl CatalogSource {
    fn label(self) -> &'static str {
        match self {
            Self::None => "no catalog loaded",
            Self::Drive => "loaded from drive",
            Self::Bundled => "bundled snapshot",
            Self::Network => "fresh from network",
        }
    }
}

#[derive(Default)]
struct Readiness {
    drive_ok: bool,
    engine_ok: bool,
    runtime_ok: bool,
    models_ok: bool,
    model_count: usize,
    missing: Vec<String>,
    drive_root: Option<PathBuf>,
    runtime_path: Option<PathBuf>,
}

struct App {
    // Inputs
    drive: String,
    init_version: String,
    catalog_url: String,
    license_scope: LicenseScope,

    // State
    layout_cache: Option<DriveLayout>,
    catalog: Option<Catalog>,
    catalog_source: CatalogSource,
    engines: Vec<EngineStatus>,
    memory: MemorySnapshot,
    show_settings: bool,

    // Runtime process management
    runtime_child: Option<std::process::Child>,
    runtime_url: Option<String>,

    // Log + worker
    output: Vec<String>,
    job_rx: Option<mpsc::Receiver<Job>>,
    job_running: bool,

    // Decoded once at startup, used to render the mascot in the header.
    mascot: Option<egui::TextureHandle>,
}

impl App {
    fn new(initial_drive: Option<PathBuf>) -> Self {
        let platform = detect_platform();
        let memory = detect_memory();
        let mut me = Self {
            drive: initial_drive
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            init_version: "0.1.0".into(),
            catalog_url: DEFAULT_CATALOG_URL.into(),
            license_scope: LicenseScope::PermissiveOnly,
            layout_cache: None,
            catalog: None,
            catalog_source: CatalogSource::None,
            engines: Vec::new(),
            memory,
            show_settings: false,
            runtime_child: None,
            runtime_url: None,
            output: vec![format!(
                "USBuddy installer (GUI) {} — {}/{} • {:.1} GiB RAM available",
                compiled_version(),
                platform.os,
                platform.arch,
                memory.available_bytes as f64 / 1_073_741_824.0
            )],
            job_rx: None,
            job_running: false,
            mascot: None,
        };
        me.refresh_layout();
        me.refresh_engine_status();
        me.try_load_drive_catalog_silent();
        me
    }

    fn refresh_layout(&mut self) {
        let trimmed = self.drive.trim();
        self.layout_cache = if trimmed.is_empty() {
            None
        } else {
            Some(DriveLayout::new(PathBuf::from(trimmed)))
        };
    }

    fn layout(&self) -> Option<&DriveLayout> {
        self.layout_cache.as_ref()
    }

    fn log(&mut self, line: impl Into<String>) {
        for piece in line.into().split('\n') {
            self.output.push(piece.to_string());
        }
        if self.output.len() > 500 {
            let drop = self.output.len() - 500;
            self.output.drain(0..drop);
        }
    }

    fn try_load_drive_catalog_silent(&mut self) {
        let Some(layout) = self.layout() else { return };
        let path = layout.catalog_path();
        if !path.exists() {
            return;
        }
        match load_catalog(&path) {
            Ok(c) => {
                self.catalog = Some(c);
                self.catalog_source = CatalogSource::Drive;
            }
            Err(error) => {
                self.log(format!("[warn] drive catalog parse: {error}"));
            }
        }
    }

    fn spawn_blocking<F>(&mut self, label: &str, work: F)
    where
        F: FnOnce(mpsc::Sender<Job>) + Send + 'static,
    {
        if self.job_running {
            self.log("[busy] previous job still running");
            return;
        }
        self.log(format!("→ {label}"));
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        self.job_running = true;
        thread::spawn(move || work(tx));
    }

    fn drain_jobs(&mut self) {
        let Some(rx) = self.job_rx.as_ref() else {
            return;
        };
        let mut messages: Vec<Job> = Vec::new();
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok(job) => messages.push(job),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        for job in messages {
            match job {
                Job::Log(line) => self.log(line),
                Job::CatalogLoaded(catalog, source) => {
                    self.log(format!(
                        "✓ Catalog loaded ({}) — {} models, {} advisories",
                        source,
                        catalog.models.len(),
                        catalog.advisories.len()
                    ));
                    self.catalog = Some(*catalog);
                    self.catalog_source = match source {
                        "bundled" => CatalogSource::Bundled,
                        "network" => CatalogSource::Network,
                        _ => CatalogSource::Drive,
                    };
                }
                Job::EngineStatus(status) => {
                    self.engines = status;
                }
            }
        }
        if done {
            self.job_running = false;
            self.job_rx = None;
        }
    }
}

// --------------------------------------------------------------------
// Actions
// --------------------------------------------------------------------

impl App {
    fn pick_drive(&mut self) {
        let mut dialog = rfd::FileDialog::new().set_title("Select USB drive folder");
        if let Some(layout) = self.layout() {
            dialog = dialog.set_directory(layout.root());
        }
        if let Some(picked) = dialog.pick_folder() {
            self.drive = picked.display().to_string();
            self.log(format!("• Drive set to {}", picked.display()));
            self.refresh_layout();
            self.catalog = None;
            self.catalog_source = CatalogSource::None;
            self.try_load_drive_catalog_silent();
            self.refresh_engine_status();
        }
    }

    fn action_init(&mut self) {
        let Some(layout) = self.layout().cloned() else {
            self.log("[error] pick a drive first");
            return;
        };
        let version = self.init_version.trim().to_string();
        if Version::parse(&version).is_err() {
            self.log(format!("[error] init: '{version}' is not valid semver"));
            return;
        }
        match layout.initialize_structure(&version) {
            Ok(()) => {
                self.log(format!("✓ Drive initialised at version {version}"));
                self.refresh_engine_status();
            }
            Err(error) => self.log(format!("[error] init: {error}")),
        }
    }

    fn action_rollback(&mut self) {
        let Some(layout) = self.layout().cloned() else {
            self.log("[error] pick a drive first");
            return;
        };
        match layout.rollback() {
            Ok(next) => self.log(format!(
                "✓ Rolled back. Active: {} (previous: {})",
                next.active,
                next.previous.as_deref().unwrap_or("(none)")
            )),
            Err(error) => self.log(format!("[error] rollback: {error}")),
        }
    }

    fn action_use_bundled_catalog(&mut self) {
        match serde_json::from_str::<Catalog>(BUNDLED_CATALOG) {
            Ok(catalog) => {
                if let Some(layout) = self.layout()
                    && let Err(error) = std::fs::write(layout.catalog_path(), BUNDLED_CATALOG)
                {
                    self.log(format!("[warn] could not write catalog to drive: {error}"));
                }
                self.log(format!(
                    "✓ Loaded bundled catalog: {} models",
                    catalog.models.len()
                ));
                self.catalog = Some(catalog);
                self.catalog_source = CatalogSource::Bundled;
            }
            Err(error) => self.log(format!("[error] bundled catalog parse: {error}")),
        }
    }

    fn action_refresh_catalog_network(&mut self) {
        let Some(layout) = self.layout().cloned() else {
            self.log("[error] pick a drive first");
            return;
        };
        let url = self.catalog_url.trim().to_string();
        if url.is_empty() {
            self.log("[error] catalog URL is empty");
            return;
        }
        let dest = layout.catalog_path();
        let label = format!("Fetching catalog from {url}");
        self.spawn_blocking(&label, move |tx| {
            match download_verified(&url, &dest, None) {
                Ok(sha) => {
                    let _ = tx.send(Job::Log(format!("  sha256={sha}")));
                    match load_catalog(&dest) {
                        Ok(c) => {
                            let _ = tx.send(Job::CatalogLoaded(Box::new(c), "network"));
                        }
                        Err(error) => {
                            let _ = tx.send(Job::Log(format!(
                                "[error] catalog validate: {error} (using bundled instead)"
                            )));
                            if let Ok(c) = serde_json::from_str::<Catalog>(BUNDLED_CATALOG) {
                                let _ = tx.send(Job::CatalogLoaded(Box::new(c), "bundled"));
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Job::Log(format!(
                        "[warn] network fetch failed ({error}) — falling back to bundled catalog"
                    )));
                    if let Ok(c) = serde_json::from_str::<Catalog>(BUNDLED_CATALOG) {
                        let _ = tx.send(Job::CatalogLoaded(Box::new(c), "bundled"));
                    }
                }
            }
        });
    }

    fn action_download(&mut self, entry: ModelEntry) {
        let Some(layout) = self.layout().cloned() else {
            self.log("[error] pick a drive first");
            return;
        };
        let dest = layout.models_dir().join(&entry.file_name);
        let url = entry.source.url.clone();
        let expected = entry.sha256.clone();
        let size_gib = entry.size_bytes as f64 / 1_073_741_824.0;
        let label = format!(
            "Downloading {} ({:.1} GiB) from {}",
            entry.display_name, size_gib, url
        );
        self.spawn_blocking(&label, move |tx| {
            match download_verified(&url, &dest, Some(&expected)) {
                Ok(sha) => {
                    let _ = tx.send(Job::Log(format!(
                        "✓ Saved {} (sha256={sha})",
                        dest.display()
                    )));
                }
                Err(error) => {
                    let _ = tx.send(Job::Log(format!("[error] download: {error}")));
                }
            }
        });
    }

    fn action_remove(&mut self, file_name: &str) {
        let Some(layout) = self.layout().cloned() else {
            return;
        };
        let target = layout.models_dir().join(file_name);
        if target.exists() {
            match std::fs::remove_file(&target) {
                Ok(()) => self.log(format!("✓ Removed {}", target.display())),
                Err(error) => self.log(format!("[error] remove: {error}")),
            }
        } else {
            self.log(format!("[error] not on drive: {}", target.display()));
        }
    }

    fn action_write_license(&mut self) {
        let Some(layout) = self.layout().cloned() else {
            return;
        };
        let prefs = LicensePrefs {
            scope: self.license_scope,
        };
        match prefs.write_to(&layout.license_prefs_path()) {
            Ok(()) => self.log(format!("✓ License prefs: scope={:?}", prefs.scope)),
            Err(error) => self.log(format!("[error] license: {error}")),
        }
    }

    fn refresh_engine_status(&mut self) {
        let Some(layout) = self.layout() else {
            self.engines = Vec::new();
            return;
        };
        let Ok(current) = layout.read_current() else {
            self.engines = Vec::new();
            return;
        };
        self.engines = engine_report_status(layout, &current.active);
    }

    fn action_install_engine(&mut self, selection: EngineSelection) {
        let Some(layout) = self.layout().cloned() else {
            self.log("[error] pick a drive first");
            return;
        };
        let Ok(current) = layout.read_current() else {
            self.log("[error] drive is not initialised — click \"Initialise drive\" first");
            return;
        };
        let tag = DEFAULT_LLAMA_TAG.to_string();
        let label = match &selection {
            EngineSelection::AllPlatforms => "Installing engines for ALL platforms".to_string(),
            EngineSelection::CurrentHost => "Installing engine for current host".to_string(),
            EngineSelection::Named(t) => format!("Installing engine for {}", t.dir_name()),
        };
        let drive_root = layout.root().to_path_buf();
        let active = current.active.clone();
        self.spawn_blocking(&label, move |tx| {
            let layout = DriveLayout::new(drive_root);
            let log_tx = tx.clone();
            match install_engines(&layout, &active, &selection, &tag, move |line| {
                let _ = log_tx.send(Job::Log(line));
            }) {
                Ok(_installed) => {
                    let _ = tx.send(Job::EngineStatus(engine_report_status(&layout, &active)));
                    let _ = tx.send(Job::Log("✓ Engine install complete".into()));
                }
                Err(error) => {
                    let _ = tx.send(Job::Log(format!("[error] engine install: {error}")));
                }
            }
        });
    }

    fn action_install_runtime_host(&mut self) {
        let Some(layout) = self.layout().cloned() else {
            self.log("[error] pick a drive first");
            return;
        };
        let Ok(current) = layout.read_current() else {
            self.log("[error] drive is not initialised");
            return;
        };
        let platform = detect_platform();
        let arch = match platform.arch.as_str() {
            "x86_64" => "x64".to_string(),
            "aarch64" => "arm64".to_string(),
            other => other.to_string(),
        };
        let bin_name = if platform.os == "windows" {
            "usbuddy-runtime.exe"
        } else {
            "usbuddy-runtime"
        };
        let source = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(bin_name)));
        let Some(source) = source else {
            self.log("[error] cannot locate sibling usbuddy-runtime binary");
            return;
        };
        if !source.exists() {
            self.log(format!(
                "[error] runtime not found at {} — build it with `cargo build --release -p usbuddy-runtime` and relaunch this GUI",
                source.display()
            ));
            return;
        }
        let dest_dir = layout
            .version_dir(&current.active)
            .join("bin")
            .join(format!("{}-{arch}", platform.os));
        if let Err(error) = std::fs::create_dir_all(&dest_dir) {
            self.log(format!("[error] create {}: {error}", dest_dir.display()));
            return;
        }
        let dest = dest_dir.join(bin_name);
        if let Err(error) = std::fs::copy(&source, &dest) {
            self.log(format!("[error] copy runtime: {error}"));
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&dest) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&dest, perms);
            }
        }
        self.log(format!("✓ Installed runtime: {}", dest.display()));
    }

    fn action_install_runtimes_from_release(&mut self, selection: EngineSelection) {
        let Some(layout) = self.layout().cloned() else {
            self.log("[error] pick a drive first");
            return;
        };
        let Ok(current) = layout.read_current() else {
            self.log("[error] drive is not initialised — click \"Initialise drive\" first");
            return;
        };
        let base = DEFAULT_RUNTIME_RELEASE_BASE.to_string();
        let label = match &selection {
            EngineSelection::AllPlatforms => {
                "Fetching runtimes for ALL platforms from latest release".to_string()
            }
            EngineSelection::CurrentHost => {
                "Fetching runtime for current host from latest release".to_string()
            }
            EngineSelection::Named(t) => {
                format!("Fetching runtime for {} from latest release", t.dir_name())
            }
        };
        let drive_root = layout.root().to_path_buf();
        let active = current.active.clone();
        self.spawn_blocking(&label, move |tx| {
            let layout = DriveLayout::new(drive_root);
            let log_tx = tx.clone();
            match install_runtimes_from_release(&layout, &active, &selection, &base, move |line| {
                let _ = log_tx.send(Job::Log(line));
            }) {
                Ok(_) => {
                    let _ = tx.send(Job::EngineStatus(engine_report_status(&layout, &active)));
                    let _ = tx.send(Job::Log("✓ Runtime install (from release) complete".into()));
                }
                Err(error) => {
                    let _ = tx.send(Job::Log(format!(
                        "[error] runtime install from release: {error}"
                    )));
                }
            }
        });
    }
}

// --------------------------------------------------------------------
// UI
// --------------------------------------------------------------------

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_jobs();
        if self.job_running {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(150));
        }

        self.render_top_bar(ui);
        self.render_log_panel(ui);
        self.render_main(ui);

        if self.show_settings {
            self.render_settings_window(ui.ctx());
        }
    }
}

impl App {
    fn render_top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x16, 0x1b, 0x22))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(0x2a, 0x31, 0x3c),
                    ))
                    .inner_margin(egui::Margin::symmetric(20, 12)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(tex) = &self.mascot {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(36.0, 36.0)));
                        ui.add_space(10.0);
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("USBuddy")
                                .size(20.0)
                                .strong()
                                .color(egui::Color32::from_rgb(0x5b, 0x8c, 0xff)),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "Installer · v{}  ·  {:.1} GiB RAM available",
                                compiled_version(),
                                self.memory.available_bytes as f64 / 1_073_741_824.0
                            ))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(0x6b, 0x77, 0x85)),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("⚙  Settings").clicked() {
                            self.show_settings = !self.show_settings;
                        }
                        if self.job_running {
                            ui.add_space(10.0);
                            ui.colored_label(
                                egui::Color32::from_rgb(0xff, 0xc9, 0x4a),
                                "● Working…",
                            );
                        }
                    });
                });
            });
    }

    fn render_log_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(180.0)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x0a, 0x0d, 0x12))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(0x2a, 0x31, 0x3c),
                    ))
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("⌨  Log")
                            .strong()
                            .color(egui::Color32::from_rgb(0x9a, 0xa5, 0xb4)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Clear").clicked() {
                            self.output.clear();
                        }
                    });
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.output {
                            ui.label(
                                egui::RichText::new(line)
                                    .monospace()
                                    .color(egui::Color32::from_rgb(0xc8, 0xd0, 0xda)),
                            );
                        }
                    });
            });
    }

    fn render_main(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x0d, 0x11, 0x17))
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.render_launch_card(ui);
                    ui.add_space(12.0);
                    self.render_drive_card(ui);
                    ui.add_space(12.0);
                    self.render_engine_card(ui);
                    ui.add_space(12.0);
                    self.render_catalog_card(ui);
                    ui.add_space(12.0);
                    self.render_models_card(ui);
                });
            });
    }

    fn readiness(&self) -> Readiness {
        let mut r = Readiness::default();
        let Some(layout) = self.layout() else {
            r.missing.push("Drive not picked".into());
            return r;
        };
        let Ok(current) = layout.read_current() else {
            r.missing
                .push("Drive not initialised (click Initialise)".into());
            return r;
        };
        r.drive_ok = true;
        let host = EngineTarget::current_host();
        let host_engine = host
            .and_then(|h| self.engines.iter().find(|s| s.target == h))
            .map(|s| s.installed)
            .unwrap_or(false);
        if host_engine {
            r.engine_ok = true;
        } else {
            r.missing
                .push("Engine for this host not installed (Engine card)".into());
        }
        let host_arch_dir = host.map(|h| h.dir_name());
        let runtime_path = host_arch_dir.as_ref().map(|d| {
            layout
                .version_dir(&current.active)
                .join("bin")
                .join(d)
                .join(if cfg!(windows) {
                    "usbuddy-runtime.exe"
                } else {
                    "usbuddy-runtime"
                })
        });
        if let Some(p) = runtime_path.as_ref() {
            if p.exists() {
                r.runtime_ok = true;
                r.runtime_path = Some(p.clone());
            } else {
                r.missing
                    .push("USBuddy runtime for this host not installed (Engine card)".into());
            }
        }
        let models = std::fs::read_dir(layout.models_dir())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_name()
                            .to_str()
                            .map(|n| n.ends_with(".gguf"))
                            .unwrap_or(false)
                    })
                    .count()
            })
            .unwrap_or(0);
        if models > 0 {
            r.models_ok = true;
            r.model_count = models;
        } else {
            r.missing
                .push("No models downloaded yet (Models card)".into());
        }
        r.drive_root = Some(layout.root().to_path_buf());
        r
    }

    fn render_launch_card(&mut self, ui: &mut egui::Ui) {
        let ready = self.readiness();
        let running = self.runtime_alive();
        card(ui, "▶ Launch USBuddy", |ui| {
            if running {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
                        "● USBuddy is running",
                    );
                    ui.label(
                        egui::RichText::new("http://127.0.0.1:8765")
                            .monospace()
                            .weak(),
                    );
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("🌐  Open chat in browser")
                                    .size(15.0)
                                    .strong(),
                            )
                            .min_size(egui::vec2(220.0, 32.0)),
                        )
                        .clicked()
                    {
                        self.action_open_browser();
                    }
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("■  Stop USBuddy").size(15.0))
                                .min_size(egui::vec2(140.0, 32.0))
                                .fill(egui::Color32::from_rgb(0x6b, 0x1f, 0x1f)),
                        )
                        .clicked()
                    {
                        self.action_stop_runtime();
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Quitting this installer will also stop the USBuddy runtime.",
                    )
                    .small()
                    .weak(),
                );
                return;
            }

            let all_ready =
                ready.drive_ok && ready.engine_ok && ready.runtime_ok && ready.models_ok;
            if all_ready {
                ui.horizontal(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(0x4c, 0xaf, 0x50), "● Ready");
                    ui.label(format!(
                        "{} model(s) installed. Click below to start USBuddy and open the chat UI in your browser.",
                        ready.model_count
                    ));
                });
                ui.add_space(6.0);
                let launch_btn = egui::Button::new(
                    egui::RichText::new("🚀  Launch USBuddy (opens browser)")
                        .size(16.0)
                        .strong(),
                )
                .min_size(egui::vec2(280.0, 36.0));
                if ui.add(launch_btn).clicked() {
                    self.action_launch_runtime(
                        ready.runtime_path.unwrap(),
                        ready.drive_root.unwrap(),
                    );
                }
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Or eject the drive and double-click launch-macos.command / launch-linux.sh / launch-windows.bat on the host you want to use.",
                    )
                    .small()
                    .weak(),
                );
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(0xff, 0xb3, 0x00),
                    "● Not ready yet — finish the steps below:",
                );
                for m in &ready.missing {
                    ui.label(format!("  • {m}"));
                }
            }
        });
    }

    fn action_launch_runtime(&mut self, runtime_path: PathBuf, drive_root: PathBuf) {
        use std::process::Command;
        if self.runtime_alive() {
            self.log("• USBuddy is already running — opening browser");
            self.action_open_browser();
            return;
        }
        self.log(format!("▶ Launching: {}", runtime_path.display()));
        let mut cmd = Command::new(&runtime_path);
        cmd.arg("serve")
            .arg("--drive")
            .arg(&drive_root)
            .arg("--open-browser");
        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                self.runtime_child = Some(child);
                self.runtime_url = Some("http://127.0.0.1:8765".into());
                self.log(format!(
                    "✓ USBuddy runtime started (pid {pid}) — browser opening at http://127.0.0.1:8765. Quit this installer (or click Stop USBuddy) to shut it down."
                ));
            }
            Err(e) => {
                self.log(format!("[error] failed to launch runtime: {e}"));
            }
        }
    }

    fn runtime_alive(&mut self) -> bool {
        let Some(child) = self.runtime_child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.log(format!("• USBuddy runtime exited ({status})"));
                self.runtime_child = None;
                self.runtime_url = None;
                false
            }
            Ok(None) => true,
            Err(e) => {
                self.log(format!("[warn] runtime status check failed: {e}"));
                true
            }
        }
    }

    fn action_open_browser(&mut self) {
        let Some(url) = self.runtime_url.clone() else {
            self.log("[error] runtime is not running");
            return;
        };
        let result = if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg(&url).spawn()
        } else if cfg!(target_os = "windows") {
            std::process::Command::new("cmd")
                .args(["/C", "start", "", &url])
                .spawn()
        } else {
            std::process::Command::new("xdg-open").arg(&url).spawn()
        };
        if let Err(e) = result {
            self.log(format!("[error] could not open browser: {e}"));
        }
    }

    fn action_stop_runtime(&mut self) {
        let Some(mut child) = self.runtime_child.take() else {
            return;
        };
        self.runtime_url = None;
        self.log(format!("■ Stopping USBuddy runtime (pid {})…", child.id()));
        let _ = child.kill();
        let _ = child.wait();
        self.log("✓ USBuddy runtime stopped");
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(mut child) = self.runtime_child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl App {
    fn render_engine_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "2. Engine (llama.cpp)", |ui| {
            if self.layout().is_none() {
                ui.label(
                    egui::RichText::new("Pick a drive above first.")
                        .italics()
                        .weak(),
                );
                return;
            }
            let host = EngineTarget::current_host();
            let host_installed = host
                .and_then(|h| self.engines.iter().find(|s| s.target == h))
                .map(|s| s.installed)
                .unwrap_or(false);
            let any_installed = self.engines.iter().any(|s| s.installed);
            let all_installed =
                !self.engines.is_empty() && self.engines.iter().all(|s| s.installed);

            ui.horizontal(|ui| {
                let (color, msg) = if all_installed {
                    (
                        egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
                        "Fully portable — all 6 platforms provisioned".to_string(),
                    )
                } else if host_installed {
                    (
                        egui::Color32::from_rgb(0xff, 0xb3, 0x00),
                        "Works on this host only. Install all platforms to make the stick truly portable.".to_string(),
                    )
                } else if any_installed {
                    (
                        egui::Color32::from_rgb(0xff, 0xb3, 0x00),
                        "Partially provisioned — but NOT this host. Chat will not work until you install for the current platform.".to_string(),
                    )
                } else {
                    (
                        egui::Color32::from_rgb(0xe5, 0x73, 0x73),
                        "No engine installed. Chat will fail. Install at least the current host.".to_string(),
                    )
                };
                ui.colored_label(color, "●");
                ui.label(msg);
            });

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let busy = self.job_running;
                let host_setup = egui::Button::new(
                    egui::RichText::new("⚙  Set up THIS host (engine + runtime)")
                        .strong(),
                )
                .min_size(egui::vec2(280.0, 32.0));
                if ui
                    .add_enabled(!busy && host.is_some(), host_setup)
                    .on_hover_text(format!(
                        "One click: downloads llama.cpp {} for this host AND copies the USBuddy runtime onto the drive. This is the minimum to make chat work right now.",
                        DEFAULT_LLAMA_TAG
                    ))
                    .clicked()
                {
                    self.action_install_engine(EngineSelection::CurrentHost);
                    self.action_install_runtime_host();
                    self.refresh_engine_status();
                }
                let portable_setup = egui::Button::new(
                    "🌍  Make it fully portable (ALL 6 platforms)",
                );
                if ui
                    .add_enabled(!busy, portable_setup)
                    .on_hover_text("Downloads llama.cpp for all 6 platforms (~60–90 MB) AND attempts to fetch per-platform USBuddy runtimes from the latest GitHub release. Runtime fetch will 404 until a release is published — fall back to running \"Set up THIS host\" on each machine you plug the stick into.")
                    .clicked()
                {
                    self.action_install_engine(EngineSelection::AllPlatforms);
                    self.action_install_runtime_host();
                    self.action_install_runtimes_from_release(EngineSelection::AllPlatforms);
                }
            });

            ui.add_space(6.0);
            ui.collapsing("Advanced (per-action buttons)", |ui| {
                ui.horizontal_wrapped(|ui| {
                    let busy = self.job_running;
                    if ui
                        .add_enabled(
                            !busy && host.is_some(),
                            egui::Button::new("Engine: this host only"),
                        )
                        .clicked()
                    {
                        self.action_install_engine(EngineSelection::CurrentHost);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Engine: ALL platforms"))
                        .clicked()
                    {
                        self.action_install_engine(EngineSelection::AllPlatforms);
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Runtime: copy local build"))
                        .clicked()
                    {
                        self.action_install_runtime_host();
                        self.refresh_engine_status();
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("Runtime: ALL from release"))
                        .clicked()
                    {
                        self.action_install_runtimes_from_release(EngineSelection::AllPlatforms);
                    }
                });
            });

            ui.add_space(6.0);
            egui::Grid::new("engines-grid")
                .num_columns(3)
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Platform").strong());
                    ui.label(egui::RichText::new("Status").strong());
                    ui.label(egui::RichText::new("Path").strong());
                    ui.end_row();
                    let host_dir = host.map(|h| h.dir_name());
                    for status in &self.engines {
                        let is_host = host_dir.as_deref() == Some(&status.target.dir_name());
                        let label = if is_host {
                            format!("{} (this host)", status.target.dir_name())
                        } else {
                            status.target.dir_name()
                        };
                        ui.label(label);
                        if status.installed {
                            ui.colored_label(
                                egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
                                "Installed",
                            );
                        } else {
                            ui.colored_label(egui::Color32::GRAY, "—");
                        }
                        ui.label(
                            egui::RichText::new(format!("{}", status.server_path.display()))
                                .small()
                                .weak(),
                        );
                        ui.end_row();
                    }
                });
        });
    }

    fn render_drive_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "1. Drive", |ui| {
            ui.horizontal(|ui| {
                ui.label("Drive folder:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.drive)
                        .hint_text("/Volumes/USBUDDY or /tmp/usbuddy-dev")
                        .desired_width(420.0),
                );
                if ui.button("📂 Browse…").clicked() {
                    self.pick_drive();
                }
            });

            let (status_color, status_text) = drive_status(self.layout());
            ui.horizontal(|ui| {
                ui.colored_label(status_color, "●");
                ui.label(status_text);
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Init version:");
                ui.add(egui::TextEdit::singleline(&mut self.init_version).desired_width(80.0));
                if ui
                    .button("Initialise drive")
                    .on_hover_text(
                        "Creates the shadow-tree layout, current.json, models/, .usbuddy/ — \
                     non-destructive to existing files outside this scope.",
                    )
                    .clicked()
                {
                    self.action_init();
                }
                ui.separator();
                if ui
                    .button("Rollback")
                    .on_hover_text(
                        "Atomically swap current.json back to the previously-installed version.",
                    )
                    .clicked()
                {
                    self.action_rollback();
                }
            });
        });
    }

    fn render_catalog_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "3. Catalog", |ui| {
            let n = self.catalog.as_ref().map(|c| c.models.len()).unwrap_or(0);
            let src = self.catalog_source.label();
            ui.horizontal(|ui| {
                let color = if self.catalog.is_some() {
                    egui::Color32::from_rgb(0x4c, 0xaf, 0x50)
                } else {
                    egui::Color32::GRAY
                };
                ui.colored_label(color, "●");
                ui.label(format!("{n} model(s) — {src}"));
            });

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Use bundled catalog")
                    .on_hover_text(
                        "Load the snapshot baked into this binary at build time. \
                     Works offline. Recommended until a real release exists.",
                    )
                    .clicked()
                {
                    self.action_use_bundled_catalog();
                }
                if ui
                    .button("Fetch latest from network")
                    .on_hover_text(
                        "Try the catalog URL set in Settings. Falls back to bundled on failure.",
                    )
                    .clicked()
                {
                    self.action_refresh_catalog_network();
                }
            });
        });
    }

    fn render_models_card(&mut self, ui: &mut egui::Ui) {
        card(ui, "4. Models", |ui| {
            let Some(catalog) = self.catalog.clone() else {
                ui.label(
                    egui::RichText::new("Load a catalog above to see available models.")
                        .italics()
                        .weak(),
                );
                return;
            };

            let installed_files: Vec<String> = self
                .layout()
                .map(|l| {
                    std::fs::read_dir(l.models_dir())
                        .map(|rd| {
                            rd.filter_map(|e| e.ok())
                                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_default();

            let mut to_download: Option<ModelEntry> = None;
            let mut to_remove: Option<String> = None;
            let memory = self.memory;

            egui::Grid::new("models-grid")
                .num_columns(5)
                .striped(true)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Model").strong());
                    ui.label(egui::RichText::new("Profile").strong());
                    ui.label(egui::RichText::new("Size").strong());
                    ui.label(egui::RichText::new("Fit").strong());
                    ui.label(egui::RichText::new("Action").strong());
                    ui.end_row();

                    for model in &catalog.models {
                        ui.label(&model.display_name).on_hover_text(format!(
                            "id: {}\nfile: {}\nlicense: {}\nsource: {}",
                            model.id, model.file_name, model.license.spdx, model.source.url
                        ));
                        ui.label(&model.profile);
                        ui.label(format!(
                            "{:.1} GiB",
                            model.size_bytes as f64 / 1_073_741_824.0
                        ));

                        let decision = assess_fit(
                            memory,
                            RamEstimateInput {
                                model_bytes: model.size_bytes,
                                context_tokens: 4_096,
                                kv_bytes_per_token: 131_072,
                                runtime_overhead_bytes: 512 * 1024 * 1024,
                            },
                        );
                        let (color, txt) = match decision.band {
                            FitBand::Green => {
                                (egui::Color32::from_rgb(0x4c, 0xaf, 0x50), "Comfortable")
                            }
                            FitBand::Yellow => (egui::Color32::from_rgb(0xff, 0xb3, 0x00), "Tight"),
                            FitBand::Red => (
                                egui::Color32::from_rgb(0xe5, 0x73, 0x73),
                                "Risky / won't fit",
                            ),
                        };
                        ui.colored_label(color, txt);

                        let installed = installed_files.contains(&model.file_name);
                        ui.horizontal(|ui| {
                            if installed {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
                                    "Installed",
                                );
                                if ui.small_button("Remove").clicked() {
                                    to_remove = Some(model.file_name.clone());
                                }
                            } else {
                                let enabled = self.layout().is_some() && !self.job_running;
                                if ui
                                    .add_enabled(enabled, egui::Button::new("⬇ Download"))
                                    .clicked()
                                {
                                    to_download = Some(model.clone());
                                }
                            }
                        });
                        ui.end_row();
                    }
                });

            if let Some(entry) = to_download {
                self.action_download(entry);
            }
            if let Some(file_name) = to_remove {
                self.action_remove(&file_name);
            }
        });
    }

    fn render_settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_settings;
        egui::Window::new("Settings")
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Catalog source").strong());
                ui.add(egui::TextEdit::singleline(&mut self.catalog_url).desired_width(420.0));
                ui.small("URL used by \"Fetch latest from network\".");

                ui.separator();
                ui.label(egui::RichText::new("License acceptance scope").strong());
                ui.radio_value(
                    &mut self.license_scope,
                    LicenseScope::All,
                    "All — accept any license up front",
                );
                ui.radio_value(
                    &mut self.license_scope,
                    LicenseScope::PermissiveOnly,
                    "Permissive only — auto-accept Apache/MIT/BSD; prompt for others",
                );
                ui.radio_value(
                    &mut self.license_scope,
                    LicenseScope::None,
                    "None — prompt for every model",
                );
                if ui.button("Save license prefs to drive").clicked() {
                    self.action_write_license();
                }
            });
        self.show_settings = open;
    }
}

// --------------------------------------------------------------------
// Small helpers
// --------------------------------------------------------------------

fn card<R>(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(0x1a, 0x1f, 0x28))
        .stroke(egui::Stroke::new(
            1.0,
            egui::Color32::from_rgb(0x2a, 0x31, 0x3c),
        ))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(title)
                    .size(15.0)
                    .strong()
                    .color(egui::Color32::from_rgb(0xe6, 0xed, 0xf3)),
            );
            ui.add_space(8.0);
            body(ui)
        })
        .inner
}

/// Centralised dark theme so cards, headers, buttons, and input fields all
/// look like they belong to the same product. Called once at app startup.
fn apply_theme(ctx: &egui::Context) {
    use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, TextStyle, Visuals};

    let mut visuals = Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(0x0d, 0x11, 0x17);
    visuals.window_fill = Color32::from_rgb(0x16, 0x1b, 0x22);
    visuals.extreme_bg_color = Color32::from_rgb(0x0a, 0x0d, 0x12);
    visuals.faint_bg_color = Color32::from_rgb(0x1a, 0x1f, 0x28);
    visuals.window_stroke = Stroke::new(1.0, Color32::from_rgb(0x2a, 0x31, 0x3c));
    visuals.widgets.noninteractive.bg_stroke =
        Stroke::new(1.0, Color32::from_rgb(0x2a, 0x31, 0x3c));
    visuals.widgets.noninteractive.fg_stroke =
        Stroke::new(1.0, Color32::from_rgb(0xe6, 0xed, 0xf3));
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(0x1f, 0x26, 0x30);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x1a, 0x1f, 0x28);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x2a, 0x31, 0x3c));
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(0xe6, 0xed, 0xf3));
    visuals.widgets.inactive.corner_radius = CornerRadius::same(8);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(0x29, 0x32, 0x3f);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x23, 0x2b, 0x36);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x3a, 0x44, 0x52));
    visuals.widgets.hovered.corner_radius = CornerRadius::same(8);
    visuals.widgets.active.bg_fill = Color32::from_rgb(0x4f, 0x7a, 0xef);
    visuals.widgets.active.weak_bg_fill = Color32::from_rgb(0x5b, 0x8c, 0xff);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x5b, 0x8c, 0xff));
    visuals.widgets.active.corner_radius = CornerRadius::same(8);
    visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(0x5b, 0x8c, 0xff, 60);
    visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(0x5b, 0x8c, 0xff));
    visuals.hyperlink_color = Color32::from_rgb(0x5b, 0x8c, 0xff);
    visuals.override_text_color = Some(Color32::from_rgb(0xe6, 0xed, 0xf3));
    ctx.set_visuals(visuals);

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.window_margin = egui::Margin::same(0);
    style.spacing.interact_size.y = 28.0;

    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(20.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(14.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(13.0, FontFamily::Monospace),
    );
    ctx.set_global_style(style);
}

fn drive_status(layout: Option<&DriveLayout>) -> (egui::Color32, String) {
    match layout {
        None => (egui::Color32::GRAY, "No drive selected".into()),
        Some(l) => {
            let root = l.root();
            if !root.exists() {
                return (
                    egui::Color32::from_rgb(0xe5, 0x73, 0x73),
                    format!("{} does not exist", root.display()),
                );
            }
            if !l.is_initialized() {
                return (
                    egui::Color32::from_rgb(0xff, 0xb3, 0x00),
                    format!(
                        "{} exists but is not initialised — click \"Initialise drive\".",
                        root.display()
                    ),
                );
            }
            match l.read_current() {
                Ok(c) => (
                    egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
                    format!(
                        "Initialised • active={} • previous={} • {}",
                        c.active,
                        c.previous.as_deref().unwrap_or("(none)"),
                        format_path(root)
                    ),
                ),
                Err(error) => (
                    egui::Color32::from_rgb(0xe5, 0x73, 0x73),
                    format!("current.json error: {error}"),
                ),
            }
        }
    }
}

fn format_path(p: &Path) -> String {
    p.display().to_string()
}

/// Embedded mascot icon — reused as the window icon. Shared with the
/// runtime's tray/web assets so we only ship one canonical artwork.
const APP_ICON_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../usbuddy-runtime/assets/usbuddy-icon.png"
));

fn load_window_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory(APP_ICON_PNG).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();
    let app = App::new(cli.drive);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 760.0])
        .with_min_inner_size([800.0, 500.0])
        .with_title("USBuddy installer");
    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "USBuddy installer",
        native_options,
        Box::new(|cc| {
            apply_theme(&cc.egui_ctx);
            // Decode the mascot into an egui texture once at startup so the
            // top bar can render it cheaply every frame.
            if let Ok(img) = image::load_from_memory(APP_ICON_PNG) {
                let img = img.into_rgba8();
                let (w, h) = img.dimensions();
                let tex = cc.egui_ctx.load_texture(
                    "usbuddy-mascot",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        &img.into_raw(),
                    ),
                    egui::TextureOptions::LINEAR,
                );
                let mut a = app;
                a.mascot = Some(tex);
                return Ok(Box::new(a));
            }
            Ok(Box::new(app))
        }),
    )
}
