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
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, mpsc},
    thread,
    time::Instant,
};

use clap::Parser;
use eframe::egui;
use semver::Version;
use usbuddy_core::{
    catalog::{Catalog, ModelEntry, load_catalog},
    compiled_version,
    download::{DownloadProgress, download_verified_with_progress},
    engine::{
        AssetProgress, DEFAULT_LLAMA_TAG, DEFAULT_RUNTIME_RELEASE_BASE, EngineSelection,
        EngineStatus, EngineTarget, install_engines_with_asset_progress,
        install_runtimes_from_release_with_asset_progress, report_status as engine_report_status,
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

// --------------------------------------------------------------------
// Download queue
// --------------------------------------------------------------------

/// Status of one item in the model-download queue.
#[derive(Debug, Clone, PartialEq, Eq)]
enum QueueStatus {
    Pending,
    Running,
    Done,
    Failed(String),
}

/// One model queued for download. The queue is shared between the UI thread
/// (which reads it every frame to render bars) and the worker thread (which
/// mutates `status` / `bytes_done` / `started` / `finished` as it works
/// through items). All access goes through the parent `Mutex`.
#[derive(Debug, Clone)]
struct QueueItem {
    id: u64,
    label: String,
    file_name: String,
    url: String,
    expected_sha256: String,
    dest: PathBuf,
    bytes_total: Option<u64>,
    bytes_done: u64,
    status: QueueStatus,
    started: Option<Instant>,
    finished: Option<Instant>,
}

impl QueueItem {
    fn percent(&self) -> Option<f32> {
        match self.bytes_total {
            Some(total) if total > 0 => {
                Some((self.bytes_done as f32 / total as f32).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

    /// Bytes per second since the download started; `None` until enough time
    /// has elapsed to compute a meaningful rate.
    fn bytes_per_sec(&self) -> Option<f64> {
        let started = self.started?;
        let elapsed = started.elapsed().as_secs_f64();
        if elapsed < 0.25 {
            return None;
        }
        Some(self.bytes_done as f64 / elapsed)
    }

    fn eta_seconds(&self) -> Option<f64> {
        let total = self.bytes_total?;
        let rate = self.bytes_per_sec()?;
        if rate < 1.0 {
            return None;
        }
        let remaining = total.saturating_sub(self.bytes_done);
        Some(remaining as f64 / rate)
    }
}

/// Shared queue state. Workers wait on `cv` for new items; the UI thread
/// only ever takes the mutex briefly to read snapshots or push new items.
struct QueueState {
    items: Mutex<VecDeque<QueueItem>>,
    cv: Condvar,
    shutdown: Mutex<bool>,
}

impl QueueState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            items: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            shutdown: Mutex::new(false),
        })
    }

    fn snapshot(&self) -> Vec<QueueItem> {
        self.items.lock().unwrap().iter().cloned().collect()
    }

    fn push(&self, item: QueueItem) {
        self.items.lock().unwrap().push_back(item);
        self.cv.notify_all();
    }

    fn remove_pending(&self, id: u64) -> bool {
        let mut items = self.items.lock().unwrap();
        if let Some(pos) = items
            .iter()
            .position(|i| i.id == id && matches!(i.status, QueueStatus::Pending))
        {
            items.remove(pos);
            true
        } else {
            false
        }
    }

    fn clear_finished(&self) {
        self.items
            .lock()
            .unwrap()
            .retain(|i| !matches!(i.status, QueueStatus::Done | QueueStatus::Failed(_)));
    }

    /// Take the next `Pending` item, marking it `Running` and stamping
    /// `started`. Returns `None` when the queue has no work; the worker
    /// then waits on the condvar.
    fn take_next(&self) -> Option<QueueItem> {
        let mut items = self.items.lock().unwrap();
        let pos = items
            .iter()
            .position(|i| matches!(i.status, QueueStatus::Pending))?;
        let item = items.get_mut(pos)?;
        item.status = QueueStatus::Running;
        item.started = Some(Instant::now());
        Some(item.clone())
    }

    fn update_progress(&self, id: u64, bytes_done: u64, bytes_total: Option<u64>) {
        let mut items = self.items.lock().unwrap();
        if let Some(it) = items.iter_mut().find(|i| i.id == id) {
            it.bytes_done = bytes_done;
            if bytes_total.is_some() {
                it.bytes_total = bytes_total;
            }
        }
    }

    fn finish(&self, id: u64, result: std::result::Result<(), String>) {
        let mut items = self.items.lock().unwrap();
        if let Some(it) = items.iter_mut().find(|i| i.id == id) {
            it.finished = Some(Instant::now());
            it.status = match result {
                Ok(()) => {
                    if let Some(total) = it.bytes_total {
                        it.bytes_done = total;
                    }
                    QueueStatus::Done
                }
                Err(e) => QueueStatus::Failed(e),
            };
        }
    }
}

/// Worker thread body: drains the queue serially. Sleeps on the condvar
/// when idle. Exits when `shutdown` is set.
fn queue_worker(state: Arc<QueueState>, log_tx: mpsc::Sender<Job>) {
    loop {
        if *state.shutdown.lock().unwrap() {
            return;
        }
        let next = state.take_next();
        let Some(item) = next else {
            // Wait for new work or shutdown.
            let items = state.items.lock().unwrap();
            let _guard = state
                .cv
                .wait_timeout(items, std::time::Duration::from_millis(500))
                .unwrap();
            continue;
        };
        let _ = log_tx.send(Job::Log(format!("⏬ start: {}", item.label)));
        let state_for_cb = state.clone();
        let id = item.id;
        let result = download_verified_with_progress(
            &item.url,
            &item.dest,
            Some(&item.expected_sha256),
            move |DownloadProgress {
                      bytes_done,
                      bytes_total,
                  }| {
                state_for_cb.update_progress(id, bytes_done, bytes_total);
            },
        );
        match result {
            Ok(sha) => {
                state.finish(item.id, Ok(()));
                let _ = log_tx.send(Job::Log(format!("✓ {} (sha256={sha})", item.label)));
            }
            Err(e) => {
                let msg = e.to_string();
                state.finish(item.id, Err(msg.clone()));
                let _ = log_tx.send(Job::Log(format!("[error] {}: {msg}", item.label)));
            }
        }
    }
}

// --------------------------------------------------------------------
// Engine install progress (single multi-asset job)
// --------------------------------------------------------------------

/// Snapshot of an in-flight engine/runtime install, rendered as a
/// determinate progress bar on the Setup page.
#[derive(Debug, Clone, Default)]
struct EngineJobProgress {
    /// Human-readable job description (e.g. "Installing engines for ALL platforms").
    label: String,
    /// Current asset name (e.g. "llama-b9570-bin-macos-arm64.tar.gz") or
    /// empty when no asset has reported progress yet.
    current_asset: String,
    /// 1-based index of the current asset.
    asset_idx: usize,
    /// Total assets in this job.
    asset_total: usize,
    bytes_done: u64,
    bytes_total: Option<u64>,
}

impl EngineJobProgress {
    fn percent(&self) -> Option<f32> {
        match self.bytes_total {
            Some(total) if total > 0 => {
                Some((self.bytes_done as f32 / total as f32).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }
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
}

/// Top-level navigation page. The whole UI is one page at a time — no
/// more 5-section scroll-of-cards. Pages map to "what does the user
/// actually want to do right now?".
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,   // launch + readiness summary
    Setup,  // drive picker + engine provisioning
    Models, // catalog + downloads
    Settings,
}

impl Page {
    fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Setup => "Setup",
            Self::Models => "Models",
            Self::Settings => "Settings",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Home => "🏠",
            Self::Setup => "🔧",
            Self::Models => "📦",
            Self::Settings => "⚙",
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
    engines: Vec<EngineStatus>,
    memory: MemorySnapshot,
    show_settings: bool,
    current_page: Page,

    // Log + worker
    output: Vec<String>,
    job_rx: Option<mpsc::Receiver<Job>>,
    job_running: bool,

    // Long-lived download queue (model downloads). Workers stream progress
    // into `queue`; engine/runtime install reports its own determinate
    // progress via `engine_progress`. Both are read each frame to render.
    queue: Arc<QueueState>,
    queue_log_rx: Option<mpsc::Receiver<Job>>,
    next_queue_id: u64,
    engine_progress: Arc<Mutex<Option<EngineJobProgress>>>,

    // Decoded once at startup, used to render the mascot in the header.
    mascot: Option<egui::TextureHandle>,
}

impl App {
    fn new(initial_drive: Option<PathBuf>) -> Self {
        let platform = detect_platform();
        let memory = detect_memory();
        let queue = QueueState::new();
        let (queue_log_tx, queue_log_rx) = mpsc::channel();
        // Worker outlives `App`; on Drop we flip `shutdown` and notify.
        // The worker owns the only surviving Sender, so when it exits the
        // channel closes naturally — no need for `App` to retain one.
        {
            let state = queue.clone();
            thread::spawn(move || queue_worker(state, queue_log_tx));
        }
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
            current_page: Page::Home,
            output: vec![format!(
                "USBuddy installer (GUI) {} — {}/{} • {:.1} GiB RAM available",
                compiled_version(),
                platform.os,
                platform.arch,
                memory.available_bytes as f64 / 1_073_741_824.0
            )],
            job_rx: None,
            job_running: false,
            queue,
            queue_log_rx: Some(queue_log_rx),
            next_queue_id: 1,
            engine_progress: Arc::new(Mutex::new(None)),
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
        // Drain the one-shot job channel (catalog refresh, engine install, etc.)
        if let Some(rx) = self.job_rx.as_ref() {
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
                self.dispatch_job(job);
            }
            if done {
                self.job_running = false;
                self.job_rx = None;
            }
        }
        // Drain the long-lived queue worker's log channel — it only ever
        // emits Job::Log lines (progress lives on QueueState directly).
        if self.queue_log_rx.is_some() {
            let mut messages: Vec<Job> = Vec::new();
            let mut disconnected = false;
            if let Some(rx) = self.queue_log_rx.as_ref() {
                loop {
                    match rx.try_recv() {
                        Ok(job) => messages.push(job),
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
            }
            if disconnected {
                self.queue_log_rx = None;
            }
            for job in messages {
                self.dispatch_job(job);
            }
        }
    }

    fn dispatch_job(&mut self, job: Job) {
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
            match download_verified_with_progress(&url, &dest, None, |_| {}) {
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

    /// Add a model to the download queue. Returns immediately; the
    /// background worker will pick it up as soon as it finishes whatever
    /// is currently in flight. Multiple calls in a row enqueue multiple
    /// models — they download serially.
    fn action_download(&mut self, entry: ModelEntry) {
        let Some(layout) = self.layout().cloned() else {
            self.log("[error] pick a drive first");
            return;
        };
        // Reject duplicate enqueues: if this file is already pending or
        // running we don't want to start a second download for the same
        // bytes.
        let already_queued = self.queue.snapshot().into_iter().any(|i| {
            i.file_name == entry.file_name
                && matches!(i.status, QueueStatus::Pending | QueueStatus::Running)
        });
        if already_queued {
            self.log(format!("• {} already in queue", entry.display_name));
            return;
        }
        let dest = layout.models_dir().join(&entry.file_name);
        let id = self.next_queue_id;
        self.next_queue_id += 1;
        let item = QueueItem {
            id,
            label: format!(
                "{} ({:.1} GiB)",
                entry.display_name,
                entry.size_bytes as f64 / 1_073_741_824.0
            ),
            file_name: entry.file_name.clone(),
            url: entry.source.url.clone(),
            expected_sha256: entry.sha256.clone(),
            dest,
            bytes_total: Some(entry.size_bytes),
            bytes_done: 0,
            status: QueueStatus::Pending,
            started: None,
            finished: None,
        };
        self.log(format!("➕ queued: {}", item.label));
        self.queue.push(item);
    }

    fn action_cancel_queue_item(&mut self, id: u64) {
        if self.queue.remove_pending(id) {
            self.log(format!("✕ removed pending queue item #{id}"));
        } else {
            self.log("[info] cannot cancel — item already downloading or finished");
        }
    }

    fn action_clear_finished_queue(&mut self) {
        self.queue.clear_finished();
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
        let prog = self.engine_progress.clone();
        *prog.lock().unwrap() = Some(EngineJobProgress {
            label: label.clone(),
            ..Default::default()
        });
        let prog_for_cb = prog.clone();
        self.spawn_blocking(&label, move |tx| {
            let layout = DriveLayout::new(drive_root);
            let log_tx = tx.clone();
            let result = install_engines_with_asset_progress(
                &layout,
                &active,
                &selection,
                &tag,
                move |line| {
                    let _ = log_tx.send(Job::Log(line));
                },
                move |AssetProgress {
                          name,
                          idx,
                          total,
                          bytes_done,
                          bytes_total,
                      }| {
                    let mut guard = prog_for_cb.lock().unwrap();
                    if let Some(ep) = guard.as_mut() {
                        ep.current_asset = name;
                        ep.asset_idx = idx;
                        ep.asset_total = total;
                        ep.bytes_done = bytes_done;
                        ep.bytes_total = bytes_total;
                    }
                },
            );
            *prog.lock().unwrap() = None;
            match result {
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
        if let Err(error) = layout.write_launchers() {
            self.log(format!(
                "[warn] runtime installed, but writing launchers failed: {error}"
            ));
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
        let prog = self.engine_progress.clone();
        *prog.lock().unwrap() = Some(EngineJobProgress {
            label: label.clone(),
            ..Default::default()
        });
        let prog_for_cb = prog.clone();
        self.spawn_blocking(&label, move |tx| {
            let layout = DriveLayout::new(drive_root);
            let log_tx = tx.clone();
            let result = install_runtimes_from_release_with_asset_progress(
                &layout,
                &active,
                &selection,
                &base,
                move |line| {
                    let _ = log_tx.send(Job::Log(line));
                },
                move |AssetProgress {
                          name,
                          idx,
                          total,
                          bytes_done,
                          bytes_total,
                      }| {
                    let mut guard = prog_for_cb.lock().unwrap();
                    if let Some(ep) = guard.as_mut() {
                        ep.current_asset = name;
                        ep.asset_idx = idx;
                        ep.asset_total = total;
                        ep.bytes_done = bytes_done;
                        ep.bytes_total = bytes_total;
                    }
                },
            );
            *prog.lock().unwrap() = None;
            match result {
                Ok(_) => {
                    if let Err(error) = layout.write_launchers() {
                        let _ = tx.send(Job::Log(format!(
                            "[warn] runtimes installed, but writing launchers failed: {error}"
                        )));
                    }
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

        self.render_nav_rail(ui);
        self.render_log_panel(ui);
        self.render_main(ui);

        if self.show_settings {
            self.render_settings_window(ui.ctx());
        }
    }
}

impl App {
    /// Left navigation rail: mascot at the top, page buttons with status
    /// dots so the user always knows whether each step is complete, then
    /// version footer at the bottom.
    fn render_nav_rail(&mut self, ui: &mut egui::Ui) {
        let r = self.readiness();
        let setup_done = r.drive_ok && r.engine_ok && r.runtime_ok;
        let models_done = r.models_ok;
        // Home tab is "good" when the drive is fully provisioned and ready to
        // launch from the stick. We no longer track a runtime process inside
        // the installer — the launcher on the stick owns that lifecycle.
        let home_ready = setup_done && models_done;

        egui::Panel::left("nav-rail")
            .resizable(false)
            .default_size(220.0)
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x12, 0x16, 0x1d))
                    .stroke(egui::Stroke::new(
                        1.0,
                        egui::Color32::from_rgb(0x2a, 0x31, 0x3c),
                    ))
                    .inner_margin(egui::Margin::same(16)),
            )
            .show_inside(ui, |ui| {
                // Brand block — mascot + name
                ui.horizontal(|ui| {
                    if let Some(tex) = &self.mascot {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(48.0, 48.0)));
                        ui.add_space(8.0);
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("USBuddy")
                                .size(20.0)
                                .strong()
                                .color(egui::Color32::from_rgb(0x2d, 0xa3, 0xf6)),
                        );
                        ui.label(
                            egui::RichText::new("offline · portable")
                                .size(10.0)
                                .color(egui::Color32::from_rgb(0x6b, 0x77, 0x85)),
                        );
                    });
                });
                ui.add_space(18.0);

                // Nav buttons
                self.nav_button(ui, Page::Home, Some(home_ready));
                self.nav_button(ui, Page::Setup, Some(setup_done));
                self.nav_button(ui, Page::Models, Some(models_done));
                self.nav_button(ui, Page::Settings, None);

                // Footer at the bottom
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    if self.job_running {
                        ui.colored_label(egui::Color32::from_rgb(0xff, 0xd2, 0x3f), "● Working…");
                        ui.add_space(6.0);
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "{:.1} GiB RAM free",
                            self.memory.available_bytes as f64 / 1_073_741_824.0
                        ))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(0x6b, 0x77, 0x85)),
                    );
                    ui.label(
                        egui::RichText::new(format!("v{}", compiled_version()))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(0x6b, 0x77, 0x85)),
                    );
                });
            });
    }

    fn nav_button(&mut self, ui: &mut egui::Ui, page: Page, status: Option<bool>) {
        let selected = self.current_page == page;
        let height = 36.0;
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), height),
            egui::Sense::click(),
        );
        let painter = ui.painter();
        let bg = if selected {
            egui::Color32::from_rgba_unmultiplied(0x2d, 0xa3, 0xf6, 38)
        } else if resp.hovered() {
            egui::Color32::from_rgb(0x1c, 0x22, 0x2b)
        } else {
            egui::Color32::TRANSPARENT
        };
        painter.rect_filled(rect, egui::CornerRadius::same(8), bg);
        if selected {
            // Left accent bar in USB blue.
            let bar = egui::Rect::from_min_size(
                rect.min + egui::vec2(0.0, 6.0),
                egui::vec2(3.0, rect.height() - 12.0),
            );
            painter.rect_filled(
                bar,
                egui::CornerRadius::same(2),
                egui::Color32::from_rgb(0x2d, 0xa3, 0xf6),
            );
        }
        let text_color = if selected {
            egui::Color32::from_rgb(0xe6, 0xed, 0xf3)
        } else {
            egui::Color32::from_rgb(0x9a, 0xa5, 0xb4)
        };
        painter.text(
            rect.left_center() + egui::vec2(14.0, 0.0),
            egui::Align2::LEFT_CENTER,
            page.icon(),
            egui::FontId::proportional(15.0),
            text_color,
        );
        painter.text(
            rect.left_center() + egui::vec2(38.0, 0.0),
            egui::Align2::LEFT_CENTER,
            page.label(),
            egui::FontId::proportional(14.0),
            text_color,
        );
        if let Some(ok) = status {
            let color = if ok {
                egui::Color32::from_rgb(0x4c, 0xaf, 0x50)
            } else {
                egui::Color32::from_rgb(0xff, 0xd2, 0x3f)
            };
            painter.circle_filled(rect.right_center() - egui::vec2(14.0, 0.0), 4.0, color);
        }
        if resp.clicked() {
            self.current_page = page;
        }
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
                    .inner_margin(egui::Margin::symmetric(28, 22)),
            )
            .show_inside(ui, |ui| {
                // Page header
                let (title, subtitle): (&str, String) = match self.current_page {
                    Page::Home => ("Home", "Launch USBuddy and see what's ready.".into()),
                    Page::Setup => (
                        "Setup",
                        "Pick your USB drive and install the engine.".into(),
                    ),
                    Page::Models => (
                        "Models",
                        "Browse the catalog and download what you want.".into(),
                    ),
                    Page::Settings => {
                        ("Settings", "Catalog source and license preferences.".into())
                    }
                };
                ui.label(
                    egui::RichText::new(title)
                        .size(24.0)
                        .strong()
                        .color(egui::Color32::from_rgb(0xe6, 0xed, 0xf3)),
                );
                ui.label(
                    egui::RichText::new(subtitle)
                        .size(13.0)
                        .color(egui::Color32::from_rgb(0x6b, 0x77, 0x85)),
                );
                ui.add_space(20.0);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.current_page {
                        Page::Home => self.render_page_home(ui),
                        Page::Setup => self.render_page_setup(ui),
                        Page::Models => self.render_page_models(ui),
                        Page::Settings => self.render_page_settings(ui),
                    });
            });
    }

    fn render_page_home(&mut self, ui: &mut egui::Ui) {
        self.render_launch_card(ui);
    }

    fn render_page_setup(&mut self, ui: &mut egui::Ui) {
        self.render_drive_card(ui);
        ui.add_space(14.0);
        self.render_engine_progress_card(ui);
        self.render_engine_card(ui);
    }

    fn render_page_models(&mut self, ui: &mut egui::Ui) {
        self.render_catalog_card(ui);
        ui.add_space(14.0);
        self.render_queue_card(ui);
        self.render_models_card(ui);
    }

    /// Engine/runtime install progress bar. Hidden when no engine job is
    /// in flight so it doesn't take screen real estate at rest.
    fn render_engine_progress_card(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.engine_progress.lock().unwrap().clone();
        let Some(prog) = snapshot else {
            return;
        };
        card(ui, &prog.label, |ui| {
            if prog.asset_total > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "Asset {}/{}: {}",
                        prog.asset_idx, prog.asset_total, prog.current_asset
                    ))
                    .color(egui::Color32::from_rgb(0xc8, 0xd0, 0xda)),
                );
            } else {
                ui.label(
                    egui::RichText::new("Preparing…")
                        .italics()
                        .color(egui::Color32::from_rgb(0x9a, 0xa5, 0xb4)),
                );
            }
            let bar = match prog.percent() {
                Some(p) => egui::ProgressBar::new(p).show_percentage().animate(true),
                None => egui::ProgressBar::new(0.0)
                    .text("downloading…")
                    .animate(true),
            };
            ui.add(bar.desired_width(ui.available_width()));
            let line = match prog.bytes_total {
                Some(t) => format!("{} / {}", format_bytes(prog.bytes_done), format_bytes(t)),
                None => format_bytes(prog.bytes_done),
            };
            ui.small(line);
        });
        ui.add_space(14.0);
    }

    /// Download queue panel. Always shown on the Models page so the user
    /// can see what's queued, what's downloading, and what failed. Hidden
    /// (collapsed to nothing) when the queue is empty.
    fn render_queue_card(&mut self, ui: &mut egui::Ui) {
        let items = self.queue.snapshot();
        if items.is_empty() {
            return;
        }
        let active_count = items
            .iter()
            .filter(|i| matches!(i.status, QueueStatus::Running | QueueStatus::Pending))
            .count();
        let title = if active_count > 0 {
            format!(
                "Download queue ({} active / {} total)",
                active_count,
                items.len()
            )
        } else {
            format!("Download queue ({} finished)", items.len())
        };
        card(ui, &title, |ui| {
            let mut to_cancel: Option<u64> = None;
            let mut clear_finished = false;

            ui.horizontal(|ui| {
                if ui.small_button("Clear finished").clicked() {
                    clear_finished = true;
                }
            });
            ui.add_space(6.0);

            for item in &items {
                let (badge, badge_color) = match &item.status {
                    QueueStatus::Pending => ("Queued", egui::Color32::from_rgb(0x9a, 0xa5, 0xb4)),
                    QueueStatus::Running => {
                        ("Downloading", egui::Color32::from_rgb(0x2d, 0xa3, 0xf6))
                    }
                    QueueStatus::Done => ("Done", egui::Color32::from_rgb(0x4c, 0xaf, 0x50)),
                    QueueStatus::Failed(_) => ("Failed", egui::Color32::from_rgb(0xe5, 0x73, 0x73)),
                };
                ui.horizontal(|ui| {
                    ui.colored_label(badge_color, egui::RichText::new(badge).strong());
                    ui.label(
                        egui::RichText::new(&item.label)
                            .color(egui::Color32::from_rgb(0xe6, 0xed, 0xf3)),
                    );
                    if matches!(item.status, QueueStatus::Pending)
                        && ui.small_button("✕ cancel").clicked()
                    {
                        to_cancel = Some(item.id);
                    }
                });
                let bar = match item.percent() {
                    Some(p) => egui::ProgressBar::new(p).show_percentage(),
                    None if matches!(item.status, QueueStatus::Running) => {
                        egui::ProgressBar::new(0.0).text("starting…").animate(true)
                    }
                    None => egui::ProgressBar::new(0.0),
                };
                ui.add(bar.desired_width(ui.available_width()));
                let detail = match (&item.status, item.bytes_total) {
                    (QueueStatus::Failed(e), _) => e.clone(),
                    (QueueStatus::Done, Some(t)) => format!("✓ {}", format_bytes(t)),
                    (QueueStatus::Done, None) => format!("✓ {}", format_bytes(item.bytes_done)),
                    (_, Some(total)) => {
                        let mut s = format!(
                            "{} / {}",
                            format_bytes(item.bytes_done),
                            format_bytes(total)
                        );
                        if let Some(rate) = item.bytes_per_sec() {
                            s.push_str(&format!("  ·  {}/s", format_bytes(rate as u64)));
                        }
                        if let Some(eta) = item.eta_seconds() {
                            s.push_str(&format!("  ·  ETA {}", format_duration_secs(eta)));
                        }
                        s
                    }
                    _ => format_bytes(item.bytes_done),
                };
                ui.small(detail);
                ui.add_space(8.0);
            }

            if let Some(id) = to_cancel {
                self.action_cancel_queue_item(id);
            }
            if clear_finished {
                self.action_clear_finished_queue();
            }
        });
        ui.add_space(14.0);
    }

    fn render_page_settings(&mut self, ui: &mut egui::Ui) {
        // Inline settings (mirrors the modal). Keep the modal alive for
        // people who hit the cog from the top, but this is the canonical
        // home now.
        card(ui, "Catalog source", |ui| {
            ui.label("URL used by \"Fetch latest from network\".");
            ui.add(egui::TextEdit::singleline(&mut self.catalog_url).desired_width(f32::INFINITY));
        });
        ui.add_space(14.0);
        card(ui, "License acceptance scope", |ui| {
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
            ui.add_space(6.0);
            if ui.button("Save license prefs to drive").clicked() {
                self.action_write_license();
            }
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
        let all_ready = ready.drive_ok && ready.engine_ok && ready.runtime_ok && ready.models_ok;

        // Centered hero panel — single focus per state. The installer never
        // parents the runtime; it just tells the user how to launch the stick.
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(0x1a, 0x1f, 0x28))
            .stroke(egui::Stroke::new(
                1.0,
                egui::Color32::from_rgb(0x2a, 0x31, 0x3c),
            ))
            .corner_radius(egui::CornerRadius::same(14))
            .inner_margin(egui::Margin::same(32))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    if let Some(tex) = &self.mascot {
                        ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(120.0, 120.0)));
                        ui.add_space(8.0);
                    }
                    if all_ready {
                        ui.label(
                            egui::RichText::new("Ready to go")
                                .size(22.0)
                                .strong()
                                .color(egui::Color32::from_rgb(0xe6, 0xed, 0xf3)),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} model(s) installed on {}",
                                ready.model_count,
                                ready
                                    .drive_root
                                    .as_ref()
                                    .map(|p| format_path(p))
                                    .unwrap_or_default()
                            ))
                            .size(13.0)
                            .color(egui::Color32::from_rgb(0x9a, 0xa5, 0xb4)),
                        );
                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new(launcher_filename_for_host())
                                .monospace()
                                .size(16.0)
                                .color(egui::Color32::from_rgb(0xe6, 0xed, 0xf3)),
                        );
                        ui.label(
                            egui::RichText::new(
                                "Double-click the launcher at the root of your USB drive to start USBuddy.",
                            )
                            .size(13.0)
                            .color(egui::Color32::from_rgb(0x9a, 0xa5, 0xb4)),
                        );
                        ui.add_space(16.0);
                        if let Some(root) = ready.drive_root.as_ref()
                            && ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("📂  Reveal launcher in file manager")
                                            .size(14.0)
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(egui::Color32::from_rgb(0x2d, 0xa3, 0xf6))
                                    .min_size(egui::vec2(300.0, 40.0))
                                    .corner_radius(egui::CornerRadius::same(10)),
                                )
                                .clicked()
                        {
                            self.action_reveal_drive(root.clone());
                        }
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(
                                "The runtime lives on the drive — eject the stick and plug it into any \
                                 host. Double-clicking the launcher there starts USBuddy without this installer.",
                            )
                            .size(11.0)
                            .color(egui::Color32::from_rgb(0x6b, 0x77, 0x85)),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("Almost there")
                                .size(22.0)
                                .strong()
                                .color(egui::Color32::from_rgb(0xff, 0xd2, 0x3f)),
                        );
                        ui.label(
                            egui::RichText::new("Finish the setup steps and you're done.")
                                .size(13.0)
                                .color(egui::Color32::from_rgb(0x9a, 0xa5, 0xb4)),
                        );
                        ui.add_space(16.0);

                        // Step checklist
                        let steps = [
                            ("USB drive picked & initialised", ready.drive_ok),
                            ("Engine installed on drive", ready.engine_ok),
                            ("USBuddy runtime installed", ready.runtime_ok),
                            ("At least one model present", ready.models_ok),
                        ];
                        for (label, ok) in steps {
                            ui.horizontal(|ui| {
                                let (mark, color) = if ok {
                                    ("✓", egui::Color32::from_rgb(0x4c, 0xaf, 0x50))
                                } else {
                                    ("○", egui::Color32::from_rgb(0x6b, 0x77, 0x85))
                                };
                                ui.colored_label(color, egui::RichText::new(mark).size(16.0));
                                ui.label(
                                    egui::RichText::new(label)
                                        .size(13.0)
                                        .color(if ok {
                                            egui::Color32::from_rgb(0xc8, 0xd0, 0xda)
                                        } else {
                                            egui::Color32::from_rgb(0x9a, 0xa5, 0xb4)
                                        }),
                                );
                            });
                        }
                        ui.add_space(20.0);

                        let next_page = if !(ready.drive_ok && ready.engine_ok && ready.runtime_ok)
                        {
                            Page::Setup
                        } else {
                            Page::Models
                        };
                        let cta = if next_page == Page::Setup {
                            "Go to Setup  →"
                        } else {
                            "Browse Models  →"
                        };
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(cta)
                                        .size(15.0)
                                        .strong()
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(0x2d, 0xa3, 0xf6))
                                .min_size(egui::vec2(240.0, 42.0))
                                .corner_radius(egui::CornerRadius::same(10)),
                            )
                            .clicked()
                        {
                            self.current_page = next_page;
                        }
                    }
                });
            });
    }

    /// Open the drive root in the host file manager so the user can see and
    /// double-click the platform launcher. Pure read-only — does not touch
    /// the runtime process.
    fn action_reveal_drive(&mut self, root: PathBuf) {
        let result = if cfg!(target_os = "macos") {
            std::process::Command::new("open").arg(&root).spawn()
        } else if cfg!(target_os = "windows") {
            std::process::Command::new("explorer").arg(&root).spawn()
        } else {
            std::process::Command::new("xdg-open").arg(&root).spawn()
        };
        if let Err(e) = result {
            self.log(format!("[error] could not open file manager: {e}"));
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
            // Snapshot the queue once per frame so we can tag each model
            // row with its queue state without re-locking inside the loop.
            let queued_files: Vec<(String, QueueStatus)> = self
                .queue
                .snapshot()
                .into_iter()
                .filter(|i| matches!(i.status, QueueStatus::Pending | QueueStatus::Running))
                .map(|i| (i.file_name, i.status))
                .collect();

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
                        // Reflect queue state in the row so the user knows
                        // a model is already in flight without having to
                        // scroll up to the queue panel.
                        let in_queue = queued_files
                            .iter()
                            .any(|(name, _)| name == &model.file_name);
                        ui.horizontal(|ui| {
                            if installed {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0x4c, 0xaf, 0x50),
                                    "Installed",
                                );
                                if ui.small_button("Remove").clicked() {
                                    to_remove = Some(model.file_name.clone());
                                }
                            } else if in_queue {
                                ui.colored_label(
                                    egui::Color32::from_rgb(0x2d, 0xa3, 0xf6),
                                    "In queue",
                                );
                            } else {
                                let enabled = self.layout().is_some();
                                if ui
                                    .add_enabled(enabled, egui::Button::new("⬇ Queue download"))
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

/// Filename of the launcher the user should double-click on the current host.
/// Matches what `DriveLayout::write_launchers` writes to the drive root.
fn launcher_filename_for_host() -> &'static str {
    if cfg!(target_os = "macos") {
        "USBuddy.command"
    } else if cfg!(target_os = "windows") {
        "USBuddy.bat"
    } else {
        "USBuddy.sh"
    }
}

/// Format a byte count as a short human-readable string ("1.42 GiB",
/// "812 KiB"). Used by the queue panel's progress detail line.
fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} GiB", b / GIB)
    } else if b >= MIB {
        format!("{:.1} MiB", b / MIB)
    } else if b >= KIB {
        format!("{:.0} KiB", b / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// Format an ETA expressed in seconds as `Hh Mm Ss` (or `Mm Ss`, or `Ss`).
fn format_duration_secs(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

impl Drop for App {
    /// Tell the long-lived queue worker to exit when the GUI window closes
    /// so the process can shut down cleanly. The worker checks
    /// `state.shutdown` every iteration and wakes on the condvar.
    fn drop(&mut self) {
        *self.queue.shutdown.lock().unwrap() = true;
        self.queue.cv.notify_all();
    }
}

/// Embedded mascot icon — reused as the window icon. Shared with the
/// runtime's tray/web assets so we only ship one canonical artwork.
const APP_ICON_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../usbuddy-runtime/assets/usbuddy-icon.png"
));

/// DejaVu Sans bundled to guarantee glyph coverage for the arrows, check
/// marks, gear, and download symbols used throughout the UI. egui's default
/// font pack (Ubuntu-Light + NotoEmoji subset) renders many of these as tofu
/// boxes, which the user reported. License: DejaVu Fonts License (in assets).
const DEJAVU_SANS_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/DejaVuSans.ttf"
));

/// Install DejaVu Sans as a fallback for both Proportional and Monospace
/// families so any glyph missing from the egui defaults still resolves.
fn install_fallback_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "dejavu-sans".to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(DEJAVU_SANS_TTF)),
    );
    // Insert as a low-priority fallback (push to the end) for both families.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("dejavu-sans".to_owned());
    }
    ctx.set_fonts(fonts);
}

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
            install_fallback_fonts(&cc.egui_ctx);
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
