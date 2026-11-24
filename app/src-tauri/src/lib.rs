//! GradApp 2 desktop shell: window, tray, plugins and the command surface.
//! The interface is plain HTML/CSS/JS in `app/ui`; it reaches this crate only
//! through `commands.rs` and the `gm://event` stream.

mod backend;
mod commands;
mod model;
mod tray;

use std::time::Duration;

use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_notification::NotificationExt as _;

use backend::AppBackend;

pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "gradapp=info".into()),
        )
        .try_init();

    let started_hidden = std::env::args().any(|a| a == "--hidden");

    tauri::Builder::default()
        // Must be first: a second launch focuses this one and hands over any invite link.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            tray::show_main(app);
            if let Some(link) = argv.iter().find(|a| a.starts_with("gradapp://")) {
                let _ = app.emit("gm://invite", link.clone());
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(AppBackend::load())
        .manage(commands::PendingUpdate::default())
        .manage(tray::TrayState::default())
        .invoke_handler(tauri::generate_handler![
            commands::app_status,
            commands::logs,
            commands::get_device,
            commands::get_ports,
            commands::install_port,
            commands::discover_start,
            commands::discover_stop,
            commands::join,
            commands::leave,
            commands::host_start,
            commands::host_stop,
            commands::mesh_snapshot,
            commands::runs,
            commands::datasets,
            commands::check_update,
            commands::install_update,
            commands::restart_app,
            commands::get_settings,
            commands::set_settings,
            commands::open_data_dir,
            commands::open_url,
            commands::hide_window,
            commands::quit_app,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tray::build(&handle)?;

            let settings = app.state::<AppBackend>().settings();
            if !(started_hidden || settings.start_hidden) {
                tray::show_main(&handle);
            }

            // Rejoin the last mesh, if asked to.
            if settings.auto_rejoin {
                if let Some(invite) = settings.last_invite.clone() {
                    if let Err(error) = app.state::<AppBackend>().join(&handle, &invite) {
                        tracing::info!("auto-rejoin skipped: {error}");
                    }
                }
            }

            // One quiet update check shortly after start; the window shows the dialog.
            let check = handle.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_secs(6)).await;
                let info =
                    commands::check_update(check.clone(), check.state(), check.state()).await;
                if let Ok(info) = info {
                    let hidden = check
                        .get_webview_window("main")
                        .map(|w| !w.is_visible().unwrap_or(false))
                        .unwrap_or(true);
                    if info.available
                        && hidden
                        && check.state::<AppBackend>().settings().notifications
                    {
                        let _ = check
                            .notification()
                            .builder()
                            .title(format!(
                                "GradApp {} is available",
                                info.latest.clone().unwrap_or_default()
                            ))
                            .body("Open GradApp to update.")
                            .show();
                    }
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Close to tray: the window goes, contributing carries on.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window
                    .app_handle()
                    .state::<AppBackend>()
                    .settings()
                    .close_to_tray
                {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("GradApp failed to start");
}
