//! `usbuddy-installer-tui` — a ratatui front-end over `usbuddy-core`.
//!
//! This is a thin wizard surface that calls into the same core library the
//! CLI uses. It is intentionally focused: the keyboard-driven menu performs
//! the most common drive-management actions without ever shelling out to
//! `usbuddy-installer-cli`.

use std::{
    fs,
    io::{self, Stdout},
    path::PathBuf,
    time::Duration,
};

use anyhow::Context;
use clap::Parser;
use crossterm::{
    ExecutableCommand,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
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
    name = "usbuddy-installer-tui",
    version = compiled_version(),
    about = "Interactive USBuddy installer (ratatui)"
)]
struct Cli {
    /// Path to act as the USB drive root. Created on first init if absent.
    #[arg(long)]
    drive: PathBuf,
}

#[derive(Clone, Copy, Debug)]
enum MenuAction {
    Inspect,
    InitDrive,
    RefreshCatalog,
    ListCatalogModels,
    DiscoverDropIns,
    DownloadModel,
    RemoveModel,
    UpdateCheck,
    UpdateRollback,
    SetLicensePrefs,
    Quit,
}

impl MenuAction {
    fn label(self) -> &'static str {
        match self {
            MenuAction::Inspect => "Inspect drive",
            MenuAction::InitDrive => "Initialise drive layout…",
            MenuAction::RefreshCatalog => "Refresh catalog from URL…",
            MenuAction::ListCatalogModels => "List catalog models",
            MenuAction::DiscoverDropIns => "Discover drop-in models",
            MenuAction::DownloadModel => "Download catalog model…",
            MenuAction::RemoveModel => "Remove installed model…",
            MenuAction::UpdateCheck => "Check for runtime updates (offline catalog only)",
            MenuAction::UpdateRollback => "Rollback to previous runtime version",
            MenuAction::SetLicensePrefs => "Set license preferences…",
            MenuAction::Quit => "Quit",
        }
    }

    fn all() -> &'static [MenuAction] {
        &[
            MenuAction::Inspect,
            MenuAction::InitDrive,
            MenuAction::RefreshCatalog,
            MenuAction::ListCatalogModels,
            MenuAction::DiscoverDropIns,
            MenuAction::DownloadModel,
            MenuAction::RemoveModel,
            MenuAction::UpdateCheck,
            MenuAction::UpdateRollback,
            MenuAction::SetLicensePrefs,
            MenuAction::Quit,
        ]
    }
}

/// Modal prompts: ask the user for one piece of text. The pending action
/// records what to do with the answer.
#[derive(Clone, Debug)]
enum PendingPrompt {
    InitVersion,
    CatalogUrl,
    DownloadModelId,
    DownloadModelUrl { model_id: String },
    RemoveModelId,
    LicenseScope,
}

struct App {
    drive: PathBuf,
    menu_state: ListState,
    output: Vec<String>,
    prompt: Option<(PendingPrompt, String, String)>, // (action, label, input buffer)
    should_quit: bool,
}

impl App {
    fn new(drive: PathBuf) -> Self {
        let mut menu_state = ListState::default();
        menu_state.select(Some(0));
        let mut app = Self {
            drive,
            menu_state,
            output: Vec::new(),
            prompt: None,
            should_quit: false,
        };
        app.log(format!(
            "USBuddy installer (TUI) {}. Drive: {}",
            compiled_version(),
            app.drive.display()
        ));
        let platform = detect_platform();
        app.log(format!(
            "Platform: {}/{} | host RAM: {:.1} GiB available",
            platform.os,
            platform.arch,
            detect_memory().available_bytes as f64 / 1_073_741_824.0
        ));
        app.log("↑/↓ to move, Enter to run, q or Ctrl-C to quit.".into());
        app
    }

    fn layout(&self) -> DriveLayout {
        DriveLayout::new(self.drive.clone())
    }

    fn log(&mut self, line: String) {
        for piece in line.split('\n') {
            self.output.push(piece.to_string());
        }
        // Cap output so we don't grow without bound.
        let max = 500;
        if self.output.len() > max {
            let drop = self.output.len() - max;
            self.output.drain(0..drop);
        }
    }

    fn log_err(&mut self, prefix: &str, error: impl std::fmt::Display) {
        self.log(format!("[error] {prefix}: {error}"));
    }

    fn selected_action(&self) -> MenuAction {
        let idx = self.menu_state.selected().unwrap_or(0);
        MenuAction::all()
            .get(idx)
            .copied()
            .unwrap_or(MenuAction::Quit)
    }

    fn move_selection(&mut self, delta: isize) {
        let actions = MenuAction::all();
        let len = actions.len() as isize;
        let cur = self.menu_state.selected().unwrap_or(0) as isize;
        let next = (cur + delta).rem_euclid(len);
        self.menu_state.select(Some(next as usize));
    }

    fn handle_menu_select(&mut self) {
        match self.selected_action() {
            MenuAction::Inspect => self.run_inspect(),
            MenuAction::InitDrive => {
                self.prompt = Some((
                    PendingPrompt::InitVersion,
                    "Runtime version to initialise (e.g. 0.1.0):".into(),
                    "0.1.0".into(),
                ));
            }
            MenuAction::RefreshCatalog => {
                self.prompt = Some((
                    PendingPrompt::CatalogUrl,
                    "Catalog URL:".into(),
                    DEFAULT_CATALOG_URL.into(),
                ));
            }
            MenuAction::ListCatalogModels => self.run_list_catalog(),
            MenuAction::DiscoverDropIns => self.run_discover_drop_ins(),
            MenuAction::DownloadModel => {
                self.prompt = Some((
                    PendingPrompt::DownloadModelId,
                    "Catalog model id (or alias):".into(),
                    String::new(),
                ));
            }
            MenuAction::RemoveModel => {
                self.prompt = Some((
                    PendingPrompt::RemoveModelId,
                    "Catalog model id to remove:".into(),
                    String::new(),
                ));
            }
            MenuAction::UpdateCheck => self.run_update_check_offline(),
            MenuAction::UpdateRollback => self.run_rollback(),
            MenuAction::SetLicensePrefs => {
                self.prompt = Some((
                    PendingPrompt::LicenseScope,
                    "License scope (all | permissive-only | none):".into(),
                    "permissive-only".into(),
                ));
            }
            MenuAction::Quit => self.should_quit = true,
        }
    }

    fn submit_prompt(&mut self) {
        let Some((action, _label, input)) = self.prompt.take() else {
            return;
        };
        match action {
            PendingPrompt::InitVersion => self.run_init(input),
            PendingPrompt::CatalogUrl => self.run_refresh_catalog(input),
            PendingPrompt::DownloadModelId => {
                self.prompt = Some((
                    PendingPrompt::DownloadModelUrl { model_id: input },
                    "Override download URL (blank = catalog default):".into(),
                    String::new(),
                ));
            }
            PendingPrompt::DownloadModelUrl { model_id } => {
                let url_override = if input.trim().is_empty() {
                    None
                } else {
                    Some(input)
                };
                self.run_download_model(model_id, url_override);
            }
            PendingPrompt::RemoveModelId => self.run_remove_model(input),
            PendingPrompt::LicenseScope => self.run_set_license_scope(input),
        }
    }

    // ------------------------------------------------------------------
    // Action runners — these all use usbuddy-core directly.
    // ------------------------------------------------------------------

    fn run_inspect(&mut self) {
        let layout = self.layout();
        self.log(format!("• Drive root: {}", layout.root().display()));
        self.log(format!("  Initialised: {}", layout.is_initialized()));
        match layout.read_current() {
            Ok(current) => self.log(format!(
                "  Current: active={} previous={}",
                current.active,
                current.previous.as_deref().unwrap_or("(none)")
            )),
            Err(error) => self.log(format!("  current.json: {error}")),
        }
        let catalog_path = layout.catalog_path();
        if catalog_path.exists() {
            match load_catalog(&catalog_path) {
                Ok(catalog) => self.log(format!(
                    "  Catalog: {} models, {} advisories",
                    catalog.models.len(),
                    catalog.advisories.len()
                )),
                Err(error) => self.log_err("catalog load", error),
            }
        } else {
            self.log("  Catalog: (none on drive)".into());
        }
    }

    fn run_init(&mut self, version: String) {
        let version = version.trim().to_string();
        if Version::parse(&version).is_err() {
            self.log_err("init", format!("'{version}' is not valid semver"));
            return;
        }
        let layout = self.layout();
        match layout.initialize_structure(&version) {
            Ok(()) => self.log(format!("✓ Drive initialised with active version {version}")),
            Err(error) => self.log_err("init", error),
        }
    }

    fn run_refresh_catalog(&mut self, url: String) {
        let url = url.trim().to_string();
        if url.is_empty() {
            self.log_err("catalog", "URL must not be empty");
            return;
        }
        let layout = self.layout();
        let dest = layout.catalog_path();
        self.log(format!("→ Fetching catalog from {url}"));
        match download_verified(&url, &dest, None) {
            Ok(sha) => {
                self.log(format!("  sha256={sha}"));
                match load_catalog(&dest) {
                    Ok(catalog) => self.log(format!(
                        "✓ Catalog written: {} models, {} advisories",
                        catalog.models.len(),
                        catalog.advisories.len()
                    )),
                    Err(error) => self.log_err("catalog validate", error),
                }
            }
            Err(error) => self.log_err("catalog download", error),
        }
    }

    fn run_list_catalog(&mut self) {
        let layout = self.layout();
        let path = layout.catalog_path();
        if !path.exists() {
            self.log("(no catalog on drive — run Refresh first)".into());
            return;
        }
        match load_catalog(&path) {
            Ok(catalog) => {
                let memory = detect_memory();
                self.log(format!(
                    "Catalog ({} models, schema={}):",
                    catalog.models.len(),
                    catalog.schema
                ));
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
                        "  • {} [{}] {:.1} GiB — RAM band: {:?}",
                        model.id,
                        model.profile,
                        model.size_bytes as f64 / 1_073_741_824.0,
                        decision.band
                    ));
                }
            }
            Err(error) => self.log_err("catalog load", error),
        }
    }

    fn run_discover_drop_ins(&mut self) {
        let layout = self.layout();
        match layout.discover_drop_in_models() {
            Ok(drops) if drops.is_empty() => self.log("No drop-in .gguf models on drive.".into()),
            Ok(drops) => {
                self.log(format!("Found {} drop-in model(s):", drops.len()));
                for drop in drops {
                    self.log(format!(
                        "  • {} (profile: {}) — {}",
                        drop.display_name,
                        drop.profile,
                        drop.path.display()
                    ));
                }
            }
            Err(error) => self.log_err("discover", error),
        }
    }

    fn run_download_model(&mut self, model_id: String, override_url: Option<String>) {
        let model_id = model_id.trim().to_string();
        if model_id.is_empty() {
            self.log_err("download", "model id must not be empty");
            return;
        }
        let layout = self.layout();
        let catalog_path = layout.catalog_path();
        if !catalog_path.exists() {
            self.log_err("download", "no catalog on drive — refresh it first");
            return;
        }
        let catalog = match load_catalog(&catalog_path) {
            Ok(c) => c,
            Err(error) => {
                self.log_err("catalog load", error);
                return;
            }
        };
        let entry = catalog
            .models
            .iter()
            .find(|m| m.id == model_id || m.aliases.iter().any(|a| a == &model_id));
        let entry = match entry {
            Some(e) => e.clone(),
            None => {
                self.log_err("download", format!("'{model_id}' not in catalog"));
                return;
            }
        };
        let url = override_url.unwrap_or_else(|| entry.source.url.clone());
        let dest = layout.models_dir().join(&entry.file_name);
        self.log(format!(
            "→ Downloading {} ({:.1} GiB) from {url}",
            entry.display_name,
            entry.size_bytes as f64 / 1_073_741_824.0
        ));
        match download_verified(&url, &dest, Some(&entry.sha256)) {
            Ok(sha) => self.log(format!("✓ Saved {} (sha256={sha})", dest.display())),
            Err(error) => self.log_err("download", error),
        }
    }

    fn run_remove_model(&mut self, model_id: String) {
        let model_id = model_id.trim().to_string();
        let layout = self.layout();
        let catalog_path = layout.catalog_path();
        let file_name = if catalog_path.exists() {
            match load_catalog(&catalog_path) {
                Ok(catalog) => catalog
                    .models
                    .iter()
                    .find(|m| m.id == model_id || m.aliases.contains(&model_id))
                    .map(|e| e.file_name.clone()),
                Err(error) => {
                    self.log_err("catalog load", error);
                    None
                }
            }
        } else {
            None
        };
        let file_name = file_name.unwrap_or_else(|| format!("{model_id}.gguf"));
        let target = layout.models_dir().join(&file_name);
        if target.exists() {
            match fs::remove_file(&target) {
                Ok(()) => self.log(format!("✓ Removed {}", target.display())),
                Err(error) => self.log_err("remove", error),
            }
        } else {
            self.log_err("remove", format!("file not found: {}", target.display()));
        }
    }

    fn run_update_check_offline(&mut self) {
        let layout = self.layout();
        match layout.read_current() {
            Ok(current) => self.log(format!(
                "Current active runtime: {} (previous: {})",
                current.active,
                current.previous.as_deref().unwrap_or("(none)")
            )),
            Err(error) => self.log_err("update check", error),
        }
        self.log(
            "Network-based update check is intentionally not performed in the TUI. \
             Use the CLI: `usbuddy-installer-cli update check --drive <path>`."
                .into(),
        );
    }

    fn run_rollback(&mut self) {
        let layout = self.layout();
        match layout.rollback() {
            Ok(next) => self.log(format!(
                "✓ Rolled back. Active: {} (previous: {})",
                next.active,
                next.previous.as_deref().unwrap_or("(none)")
            )),
            Err(error) => self.log_err("rollback", error),
        }
    }

    fn run_set_license_scope(&mut self, scope: String) {
        let scope = match scope.trim().to_ascii_lowercase().as_str() {
            "all" => LicenseScope::All,
            "permissive-only" | "permissive_only" | "permissive" => LicenseScope::PermissiveOnly,
            "none" => LicenseScope::None,
            other => {
                self.log_err(
                    "license scope",
                    format!("'{other}' is not one of all/permissive-only/none"),
                );
                return;
            }
        };
        let layout = self.layout();
        let prefs = LicensePrefs { scope };
        match prefs.write_to(&layout.license_prefs_path()) {
            Ok(()) => self.log(format!("✓ License prefs written: scope={:?}", prefs.scope)),
            Err(error) => self.log_err("license", error),
        }
    }
}

// ----------------------------------------------------------------------
// Rendering
// ----------------------------------------------------------------------

fn render(frame: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(frame.area());

    // Header
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "USBuddy installer",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  v{}  ", compiled_version())),
        Span::styled(
            format!("drive={}", app.drive.display()),
            Style::default().fg(Color::Gray),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, chunks[0]);

    // Body: menu + output
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(48), Constraint::Min(20)])
        .split(chunks[1]);

    let items: Vec<ListItem> = MenuAction::all()
        .iter()
        .map(|a| ListItem::new(a.label()))
        .collect();
    let menu = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Actions "))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("❯ ");
    frame.render_stateful_widget(menu, body[0], &mut app.menu_state);

    let output_lines: Vec<Line> = app.output.iter().map(|s| Line::from(s.clone())).collect();
    let output = Paragraph::new(output_lines)
        .block(Block::default().borders(Borders::ALL).title(" Output "))
        .wrap(Wrap { trim: false });
    frame.render_widget(output, body[1]);

    // Footer
    let footer_text = if app.prompt.is_some() {
        "Enter: submit | Esc: cancel"
    } else {
        "↑/↓ select • Enter run • q quit"
    };
    let footer = Paragraph::new(footer_text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(footer, chunks[2]);

    // Prompt modal
    if let Some((_, label, buffer)) = app.prompt.as_ref() {
        let area = centered_rect(60, 20, frame.area());
        frame.render_widget(Clear, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Input required ")
            .style(Style::default().bg(Color::Black).fg(Color::White));
        let inner_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(1)])
            .split(area);
        frame.render_widget(block, area);
        let label_para = Paragraph::new(label.clone())
            .style(Style::default().fg(Color::Cyan))
            .wrap(Wrap { trim: true });
        let input_para =
            Paragraph::new(format!("> {buffer}_")).style(Style::default().fg(Color::Yellow));
        frame.render_widget(label_para, inset(inner_layout[0], 2));
        frame.render_widget(input_para, inset(inner_layout[1], 2));
    }
}

fn inset(area: Rect, padding: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(padding),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(padding * 2),
        height: area.height.saturating_sub(1),
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

// ----------------------------------------------------------------------
// Event loop
// ----------------------------------------------------------------------

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind != KeyEventKind::Press {
        return;
    }
    if app.prompt.is_some() {
        match key.code {
            KeyCode::Esc => {
                app.prompt = None;
            }
            KeyCode::Enter => app.submit_prompt(),
            KeyCode::Backspace => {
                if let Some((_, _, buf)) = app.prompt.as_mut() {
                    buf.pop();
                }
            }
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) && c == 'c' {
                    app.should_quit = true;
                } else if let Some((_, _, buf)) = app.prompt.as_mut() {
                    buf.push(c);
                }
            }
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        KeyCode::Up | KeyCode::Char('k') => app.move_selection(-1),
        KeyCode::Down | KeyCode::Char('j') => app.move_selection(1),
        KeyCode::Enter => app.handle_menu_select(),
        _ => {}
    }
}

fn run(mut terminal: Terminal<CrosstermBackend<Stdout>>, mut app: App) -> anyhow::Result<()> {
    while !app.should_quit {
        terminal.draw(|frame| render(frame, &mut app))?;
        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
        {
            handle_key(&mut app, key);
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let drive = cli.drive;
    let app = App::new(drive);

    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    let result = run(terminal, app);

    disable_raw_mode().ok();
    io::stdout().execute(LeaveAlternateScreen).ok();
    result
}
