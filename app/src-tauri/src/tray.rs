//! The tray icon: Open, Start/Stop contributing, Quit. Its icon turns to the
//! "active" variant while this machine is contributing or hosting.

use std::sync::Mutex;

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::backend::AppBackend;

const ICON_IDLE: &[u8] = include_bytes!("../icons/tray@2x.png");
const ICON_ACTIVE: &[u8] = include_bytes!("../icons/tray-active@2x.png");

#[derive(Default)]
pub struct TrayState {
    toggle: Mutex<Option<MenuItem<Wry>>>,
    icon: Mutex<Option<TrayIcon<Wry>>>,
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open GradApp", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Start contributing", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit GradApp", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &toggle, &PredefinedMenuItem::separator(app)?, &quit])?;

    let tray = TrayIconBuilder::with_id("main")
        .icon(Image::from_bytes(ICON_IDLE)?)
        .tooltip("GradApp")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "quit" => app.exit(0),
            "toggle" => {
                let backend = app.state::<AppBackend>();
                if backend.contributing() {
                    backend.leave(app);
                } else if let Some(invite) = backend.settings().last_invite {
                    if let Err(error) = backend.join(app, &invite) {
                        tracing::warn!("rejoin from tray failed: {error}");
                        show_main(app);
                        let _ = app.emit("gm://navigate", "discover");
                    }
                } else {
                    show_main(app);
                    let _ = app.emit("gm://navigate", "discover");
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    let state = app.state::<TrayState>();
    *state.toggle.lock().unwrap() = Some(toggle);
    *state.icon.lock().unwrap() = Some(tray);
    Ok(())
}

/// Bring the menu text and icon in line with the worker/coordinator state.
pub fn refresh(app: &AppHandle) {
    let Some(state) = app.try_state::<TrayState>() else { return };
    let backend = app.state::<AppBackend>();
    let contributing = backend.contributing();
    let active = contributing || backend.status().hosting.is_some();
    if let Some(item) = state.toggle.lock().unwrap().as_ref() {
        let _ = item.set_text(if contributing { "Stop contributing" } else { "Start contributing" });
    };
    if let Some(tray) = state.icon.lock().unwrap().as_ref() {
        if let Ok(image) = Image::from_bytes(if active { ICON_ACTIVE } else { ICON_IDLE }) {
            let _ = tray.set_icon(Some(image));
        }
        let _ = tray.set_tooltip(Some(if contributing { "GradApp · contributing" } else if active { "GradApp · hosting" } else { "GradApp" }));
    };
}
