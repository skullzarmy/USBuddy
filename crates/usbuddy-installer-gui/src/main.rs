//! `usbuddy-installer-gui` — an eframe/egui shell over `usbuddy-core`.
//!
//! Same wizard surface as the TUI, rendered with egui. Synchronous calls into
//! the core; long-running operations (downloads) run on a worker thread and
//! stream log lines back via an mpsc channel.

use std::{fs, path::PathBuf, sync::mpsc, thread};

use clap::Parser;
use eframe::egui;
use semver::Version;
use usbuddy_core::{
    catalog::load_catalog,
    compiled_version,
    download::download_verified,
    layout::DriveLayout,
    license::{LicensePrefs, LicenseScope},
    platform::detect_platform,
    ram::{RamEstimateInput, assess_fit, detect_memory},
};

const DEFAULT_CATALOG_URL: &str =
    "https://github.com/skullzarmy/USBuddy/releases/latest/download/official.catalog.json";

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

#[derive(Default)]
struct InputState {
    drive: String,
    init_version: String,
    catalog_url: String,
    download_id: String,
    download_url: String,
    remove_id: String,
    license_scope: String,
}

enum Job {
    Log(String),
}

struct App {
    inputs: InputState,
    output: Vec<String>,
    job_rx: Option<mpsc::Receiver<Job>>,
    job_running: bool,
}

impl App {
    fn new(initial_drive: Option<PathBuf>) -> Self {
        let mut inputs = InputState {
            init_version: "0.1.0".into(),
            catalog_url: DEFAULT_CATALOG_URL.into(),
            license_scope: "permissive-only".into(),
            ..Default::default()
        };
        if let Some(d) = initial_drive {
            inputs.drive = d.display().to_string();
        }
        let platform = detect_platform();
        let mem = detect_memory();
        let output = vec![format!(
            "USBuddy installer (GUI) {} — {}/{} • {:.1} GiB RAM available",
            compiled_version(),
            platform.os,
            platform.arch,
            mem.available_bytes as f64 / 1_073_741_824.0
        )];
        Self {
            inputs,
            output,
            job_rx: None,
            job_running: false,
        }
    }

    fn log(&mut self, line: impl Into<String>) {
        let s = line.into();
        for piece in s.split('\n') {
            self.output.push(piece.to_string());
        }
        if self.output.len() > 500 {
            let drop = self.output.len() - 500;
            self.output.drain(0..drop);
        }
    }

    fn drive_layout(&self) -> Option<DriveLayout> {
        let trimmed = self.inputs.drive.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(DriveLayout::new(PathBuf::from(trimmed)))
        }
    }

    fn require_drive(&mut self) -> Option<DriveLayout> {
        match self.drive_layout() {
            Some(l) => Some(l),
            None => {
                self.log("[error] drive path must be set");
                None
            }
        }
    }

    fn spawn_blocking<F>(&mut self, label: &str, work: F)
    where
        F: FnOnce(mpsc::Sender<Job>) + Send + 'static,
    {
        if self.job_running {
            self.log("[busy] previous job still running — wait for it to finish");
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
        let mut messages: Vec<String> = Vec::new();
        let mut done = false;
        loop {
            match rx.try_recv() {
                Ok(Job::Log(line)) => messages.push(line),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    done = true;
                    break;
                }
            }
        }
        for line in messages {
            self.log(line);
        }
        if done {
            self.job_running = false;
            self.job_rx = None;
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_jobs();
        if self.job_running {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(150));
        }

        egui::Panel::top("header").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("USBuddy installer");
                ui.label(format!("v{}", compiled_version()));
                ui.separator();
                ui.label("Drive root:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.inputs.drive)
                        .hint_text("/path/to/usb")
                        .desired_width(400.0),
                );
            });
        });

        egui::Panel::left("actions")
            .default_size(320.0)
            .show_inside(ui, |ui| {
                ui.heading("Actions");
                ui.separator();

                ui.collapsing("Drive", |ui| {
                    if ui.button("Inspect").clicked() {
                        self.action_inspect();
                    }
                    ui.horizontal(|ui| {
                        ui.label("Init version:");
                        ui.text_edit_singleline(&mut self.inputs.init_version);
                    });
                    if ui.button("Initialise drive").clicked() {
                        self.action_init();
                    }
                    if ui.button("Rollback to previous").clicked() {
                        self.action_rollback();
                    }
                });

                ui.collapsing("Catalog", |ui| {
                    ui.label("URL:");
                    ui.text_edit_singleline(&mut self.inputs.catalog_url);
                    if ui.button("Refresh catalog").clicked() {
                        self.action_refresh_catalog();
                    }
                    if ui.button("List models").clicked() {
                        self.action_list_catalog();
                    }
                });

                ui.collapsing("Models", |ui| {
                    if ui.button("Discover drop-ins").clicked() {
                        self.action_discover();
                    }
                    ui.label("Catalog model id:");
                    ui.text_edit_singleline(&mut self.inputs.download_id);
                    ui.label("Override URL (optional):");
                    ui.text_edit_singleline(&mut self.inputs.download_url);
                    if ui.button("Download").clicked() {
                        self.action_download();
                    }
                    ui.separator();
                    ui.label("Remove model id:");
                    ui.text_edit_singleline(&mut self.inputs.remove_id);
                    if ui.button("Remove").clicked() {
                        self.action_remove();
                    }
                });

                ui.collapsing("License", |ui| {
                    ui.label("Scope (all | permissive-only | none):");
                    ui.text_edit_singleline(&mut self.inputs.license_scope);
                    if ui.button("Write prefs").clicked() {
                        self.action_set_license();
                    }
                });

                ui.add_space(8.0);
                if self.job_running {
                    ui.colored_label(egui::Color32::YELLOW, "Working…");
                }
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading("Output");
            ui.separator();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in &self.output {
                        ui.monospace(line);
                    }
                });
        });
    }
}

impl App {
    fn action_inspect(&mut self) {
        let Some(layout) = self.require_drive() else {
            return;
        };
        self.log(format!("• Drive: {}", layout.root().display()));
        self.log(format!("  Initialised: {}", layout.is_initialized()));
        match layout.read_current() {
            Ok(current) => self.log(format!(
                "  Current: active={} previous={}",
                current.active,
                current.previous.as_deref().unwrap_or("(none)")
            )),
            Err(error) => self.log(format!("  current.json: {error}")),
        }
        if layout.catalog_path().exists() {
            match load_catalog(&layout.catalog_path()) {
                Ok(c) => self.log(format!(
                    "  Catalog: {} models, {} advisories",
                    c.models.len(),
                    c.advisories.len()
                )),
                Err(error) => self.log(format!("[error] catalog load: {error}")),
            }
        }
    }

    fn action_init(&mut self) {
        let Some(layout) = self.require_drive() else {
            return;
        };
        let version = self.inputs.init_version.trim().to_string();
        if Version::parse(&version).is_err() {
            self.log(format!("[error] init: '{version}' is not valid semver"));
            return;
        }
        match layout.initialize_structure(&version) {
            Ok(()) => self.log(format!("✓ Drive initialised at version {version}")),
            Err(error) => self.log(format!("[error] init: {error}")),
        }
    }

    fn action_rollback(&mut self) {
        let Some(layout) = self.require_drive() else {
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

    fn action_refresh_catalog(&mut self) {
        let Some(layout) = self.require_drive() else {
            return;
        };
        let url = self.inputs.catalog_url.trim().to_string();
        if url.is_empty() {
            self.log("[error] catalog URL must not be empty");
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
                            let _ = tx.send(Job::Log(format!(
                                "✓ Catalog: {} models, {} advisories",
                                c.models.len(),
                                c.advisories.len()
                            )));
                        }
                        Err(error) => {
                            let _ = tx.send(Job::Log(format!("[error] catalog validate: {error}")));
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Job::Log(format!("[error] catalog download: {error}")));
                }
            }
        });
    }

    fn action_list_catalog(&mut self) {
        let Some(layout) = self.require_drive() else {
            return;
        };
        if !layout.catalog_path().exists() {
            self.log("(no catalog on drive — run Refresh first)");
            return;
        }
        match load_catalog(&layout.catalog_path()) {
            Ok(catalog) => {
                let memory = detect_memory();
                self.log(format!("Catalog ({} models):", catalog.models.len()));
                for model in &catalog.models {
                    let decision = assess_fit(
                        memory,
                        RamEstimateInput {
                            model_bytes: model.size_bytes,
                            context_tokens: 4_096,
                            kv_bytes_per_token: 131_072,
                            runtime_overhead_bytes: 512 * 1024 * 1024,
                        },
                    );
                    self.log(format!(
                        "  • {} [{}] {:.1} GiB — band={:?}",
                        model.id,
                        model.profile,
                        model.size_bytes as f64 / 1_073_741_824.0,
                        decision.band
                    ));
                }
            }
            Err(error) => self.log(format!("[error] catalog load: {error}")),
        }
    }

    fn action_discover(&mut self) {
        let Some(layout) = self.require_drive() else {
            return;
        };
        match layout.discover_drop_in_models() {
            Ok(drops) if drops.is_empty() => self.log("No drop-in .gguf models on drive."),
            Ok(drops) => {
                self.log(format!("Found {} drop-in model(s):", drops.len()));
                for d in drops {
                    self.log(format!(
                        "  • {} ({}) — {}",
                        d.display_name,
                        d.profile,
                        d.path.display()
                    ));
                }
            }
            Err(error) => self.log(format!("[error] discover: {error}")),
        }
    }

    fn action_download(&mut self) {
        let Some(layout) = self.require_drive() else {
            return;
        };
        let model_id = self.inputs.download_id.trim().to_string();
        if model_id.is_empty() {
            self.log("[error] download: model id must not be empty");
            return;
        }
        let catalog_path = layout.catalog_path();
        if !catalog_path.exists() {
            self.log("[error] download: no catalog on drive — refresh first");
            return;
        }
        let catalog = match load_catalog(&catalog_path) {
            Ok(c) => c,
            Err(error) => {
                self.log(format!("[error] catalog load: {error}"));
                return;
            }
        };
        let entry = match catalog
            .models
            .iter()
            .find(|m| m.id == model_id || m.aliases.contains(&model_id))
        {
            Some(e) => e.clone(),
            None => {
                self.log(format!("[error] download: '{model_id}' not in catalog"));
                return;
            }
        };
        let url_override = self.inputs.download_url.trim().to_string();
        let url = if url_override.is_empty() {
            entry.source.url.clone()
        } else {
            url_override
        };
        let dest = layout.models_dir().join(&entry.file_name);
        let display = entry.display_name.clone();
        let size_gib = entry.size_bytes as f64 / 1_073_741_824.0;
        let expected = entry.sha256.clone();
        let label = format!("Downloading {display} ({size_gib:.1} GiB) from {url}");
        self.spawn_blocking(&label, move |tx| {
            match download_verified(&url, &dest, Some(&expected)) {
                Ok(sha) => {
                    let _ = tx.send(Job::Log(format!("✓ Saved {} sha256={sha}", dest.display())));
                }
                Err(error) => {
                    let _ = tx.send(Job::Log(format!("[error] download: {error}")));
                }
            }
        });
    }

    fn action_remove(&mut self) {
        let Some(layout) = self.require_drive() else {
            return;
        };
        let model_id = self.inputs.remove_id.trim().to_string();
        let catalog_path = layout.catalog_path();
        let file_name = if catalog_path.exists() {
            match load_catalog(&catalog_path) {
                Ok(catalog) => catalog
                    .models
                    .iter()
                    .find(|m| m.id == model_id || m.aliases.contains(&model_id))
                    .map(|e| e.file_name.clone()),
                Err(_) => None,
            }
        } else {
            None
        };
        let file_name = file_name.unwrap_or_else(|| format!("{model_id}.gguf"));
        let target = layout.models_dir().join(&file_name);
        if target.exists() {
            match fs::remove_file(&target) {
                Ok(()) => self.log(format!("✓ Removed {}", target.display())),
                Err(error) => self.log(format!("[error] remove: {error}")),
            }
        } else {
            self.log(format!(
                "[error] remove: file not found: {}",
                target.display()
            ));
        }
    }

    fn action_set_license(&mut self) {
        let Some(layout) = self.require_drive() else {
            return;
        };
        let scope = match self
            .inputs
            .license_scope
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "all" => LicenseScope::All,
            "permissive-only" | "permissive_only" | "permissive" => LicenseScope::PermissiveOnly,
            "none" => LicenseScope::None,
            other => {
                self.log(format!(
                    "[error] license scope: '{other}' is not one of all/permissive-only/none"
                ));
                return;
            }
        };
        let prefs = LicensePrefs { scope };
        match prefs.write_to(&layout.license_prefs_path()) {
            Ok(()) => self.log(format!("✓ License prefs written: scope={:?}", prefs.scope)),
            Err(error) => self.log(format!("[error] license: {error}")),
        }
    }
}

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();
    let app = App::new(cli.drive);
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1024.0, 700.0]),
        ..Default::default()
    };
    eframe::run_native(
        "USBuddy installer",
        native_options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
}
