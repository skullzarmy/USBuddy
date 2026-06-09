//! Cross-platform tray/menu-bar control surface.
//!
//! Runs on the OS main thread (required by macOS Cocoa). Hosts a small
//! menu with Open chat / Stop model / Quit USBuddy and dispatches into
//! the shared `RuntimeState` that the HTTP server thread also holds.
//!
//! Tray init can fail on bare Linux desktops that lack a StatusNotifier
//! host (e.g. vanilla GNOME without the AppIndicator extension). In that
//! case we degrade gracefully: log a warning, then park the main thread
//! so the HTTP server keeps serving and the user can `kill` the process
//! or hit the web UI's Quit button.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIconBuilder};

use crate::{ICON_JPG, RuntimeState, kill_llama_server, open_browser_best_effort};

/// Per-event-loop user event so we can wake the loop on menu clicks.
enum UserEvent {
    MenuEvent(MenuEvent),
}

pub fn run_tray(state: Arc<RuntimeState>, url: String) -> Result<()> {
    // Decode the embedded JPG once into RGBA8 for the tray icon API.
    let icon = match decode_icon() {
        Ok(icon) => Some(icon),
        Err(e) => {
            eprintln!("USBuddy: failed to decode tray icon ({e}); using text-only menu");
            None
        }
    };

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Forward menu events into the tao loop so we can react on the main thread.
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::MenuEvent(event));
    }));

    let menu = Menu::new();
    let open_item = MenuItem::new("Open USBuddy chat", true, None);
    let stop_item = MenuItem::new("Stop model (free RAM)", true, None);
    let quit_item = MenuItem::new("Quit USBuddy", true, None);
    menu.append(&open_item)
        .context("append Open menu item")?;
    menu.append(&stop_item)
        .context("append Stop menu item")?;
    menu.append(&quit_item)
        .context("append Quit menu item")?;

    let open_id = open_item.id().clone();
    let stop_id = stop_item.id().clone();
    let quit_id = quit_item.id().clone();

    let tray_result = {
        let mut builder = TrayIconBuilder::new()
            .with_tooltip("USBuddy — portable offline LLM")
            .with_menu(Box::new(menu));
        if let Some(icon) = icon {
            builder = builder.with_icon(icon);
        }
        builder.build()
    };

    let _tray = match tray_result {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "USBuddy: tray icon unavailable ({e}). Server is still running at {url}."
            );
            eprintln!("        Use the web UI's Quit button or Ctrl-C in the terminal to stop.");
            // Park the main thread; the server lives on its background runtime.
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
    };

    eprintln!("USBuddy: tray icon active. Click it for Open / Stop / Quit.");

    event_loop.run(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::UserEvent(UserEvent::MenuEvent(menu_event)) = event {
            if menu_event.id == open_id {
                let _ = open_browser_best_effort(&url);
            } else if menu_event.id == stop_id {
                kill_llama_server(&state.llama_process);
            } else if menu_event.id == quit_id {
                kill_llama_server(&state.llama_process);
                state.shutdown.notify_waiters();
                // Give the HTTP server a beat to flush its graceful-shutdown.
                std::thread::sleep(Duration::from_millis(250));
                std::process::exit(0);
            }
        }
    });
}

fn decode_icon() -> Result<Icon> {
    let img = image::load_from_memory(ICON_JPG)
        .context("decode embedded usbuddy-icon.jpg")?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).context("build tray Icon from RGBA")
}
