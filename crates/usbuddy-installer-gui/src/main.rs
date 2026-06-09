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
    memory: MemorySnapshot,
    show_settings: bool,

    // Log + worker
    output: Vec<String>,
    job_rx: Option<mpsc::Receiver<Job>>,
    job_running: bool,
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
            memory,
            show_settings: false,
            output: vec![format!(
                "USBuddy installer (GUI) {} — {}/{} • {:.1} GiB RAM available",
                compiled_version(),
                platform.os,
                platform.arch,
                memory.available_bytes as f64 / 1_073_741_824.0
            )],
            job_rx: None,
            job_running: false,
        };
        me.refresh_layout();
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
            Ok(()) => self.log(format!("✓ Drive initialised at version {version}")),
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
        egui::Panel::top("header").show_inside(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("USBuddy");
                ui.label(egui::RichText::new(format!("v{}", compiled_version())).weak());
                ui.separator();
                ui.label(
                    egui::RichText::new(format!(
                        "{:.1} GiB RAM available",
                        self.memory.available_bytes as f64 / 1_073_741_824.0
                    ))
                    .weak(),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("⚙ Settings").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    if self.job_running {
                        ui.add_space(8.0);
                        ui.colored_label(egui::Color32::YELLOW, "● Working…");
                    }
                });
            });
            ui.add_space(4.0);
        });
    }

    fn render_log_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(180.0)
            .show_inside(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Log").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Clear").clicked() {
                            self.output.clear();
                        }
                    });
                });
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

    fn render_main(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show_inside(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                self.render_drive_card(ui);
                ui.add_space(8.0);
                self.render_catalog_card(ui);
                ui.add_space(8.0);
                self.render_models_card(ui);
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
        card(ui, "2. Catalog", |ui| {
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
        card(ui, "3. Models", |ui| {
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
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(title).heading());
            ui.separator();
            body(ui)
        })
        .inner
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

fn main() -> eframe::Result<()> {
    let cli = Cli::parse();
    let app = App::new(cli.drive);
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([800.0, 500.0])
            .with_title("USBuddy installer"),
        ..Default::default()
    };
    eframe::run_native(
        "USBuddy installer",
        native_options,
        Box::new(|_cc| Ok(Box::new(app))),
    )
}
