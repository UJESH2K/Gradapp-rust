//! GradApp 2 desktop shell: window, tray, plugins and the command surface.
//! The interface is plain HTML/CSS/JS in `app/ui`; it reaches this crate only
//! through `commands.rs` and the `gm://event` stream.

mod backend;
mod commands;
mod datasets;
mod mesh;
mod model;
mod peers;
mod runs;
mod tray;

use std::path::Path;
use std::time::Duration;

use tauri::{Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_notification::NotificationExt as _;

use backend::AppBackend;

/// First update check after start, then this often.
const UPDATE_FIRST: Duration = Duration::from_secs(10);
const UPDATE_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// Logs to stderr and to `<data>/logs/gradapp.<date>.log` (daily files, a
/// week kept). The returned guard flushes the file writer; keep it alive.
fn init_logging(data_dir: &Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let filter = || {
        tracing_subscriber::EnvFilter::try_from_env("GRADAPP_LOG").unwrap_or_else(|_| {
            "info,gradapp_lib=info,gm_net=info,gm_ports=info,gm_supervisor=info,gm_update=info,mdns_sd=warn,tao=warn,wry=warn".into()
        })
    };
    let _ = std::fs::create_dir_all(data_dir.join("logs"));
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("gradapp")
        .filename_suffix("log")
        .max_log_files(7)
        .build(data_dir.join("logs"));
    let (file_layer, guard) = match file {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            (
                Some(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(writer),
                ),
                Some(guard),
            )
        }
        Err(error) => {
            eprintln!("GradApp: file logging disabled: {error}");
            (None, None)
        }
    };
    let _ = tracing_subscriber::registry()
        .with(filter())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(file_layer)
        .try_init();
    guard
}

pub fn run() {
    let backend = AppBackend::load();
    let _log_guard = init_logging(&backend.data_dir());
    tracing::info!(
        "GradApp {} starting (engine {}, protocol {})",
        gm_types::APP_VERSION,
        gm_types::ENGINE_VERSION,
        gm_types::MESH_PROTOCOL
    );

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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(backend)
        .manage(commands::PendingUpdate::default())
        .manage(tray::TrayState::default())
        .manage(peers::Peers::default())
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
            peers::peers_state,
            peers::peer_invite,
            peers::invite_respond,
            peers::peers_unblock_all,
            datasets::datasets_overview,
            datasets::dataset_import,
            datasets::dataset_upload,
            datasets::dataset_set_default,
            datasets::dataset_delete,
            datasets::dataset_download,
            datasets::pick_dataset_zip,
            datasets::pick_folder,
            datasets::transfers,
            datasets::transfer_cancel,
            runs::run_options,
            runs::run_create,
            runs::run_stop,
            runs::run_delete,
            runs::run_live,
            runs::run_download_weights,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tray::build(&handle)?;
            // Announce this GradApp on the LAN and list the others (Discover).
            peers::start(&handle);

            let settings = app.state::<AppBackend>().settings();
            if !(started_hidden || settings.start_hidden) {
                tray::show_main(&handle);
            }

            // Probe now, so Home and Device show real hardware at once.
            let probe = handle.clone();
            tauri::async_runtime::spawn(async move {
                probe.state::<AppBackend>().facts().await;
            });

            // Rejoin the last mesh if this machine was contributing when GradApp last ran.
            if settings.auto_rejoin && settings.rejoin_on_start {
                if let Some(invite) = settings.last_invite.clone() {
                    let rejoin = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        match rejoin.state::<AppBackend>().join(&rejoin, &invite).await {
                            Ok(joined) => tracing::info!("auto-rejoined {}", joined.coordinator),
                            Err(error) => {
                                tracing::info!("auto-rejoin skipped: {error}");
                                backend::notice(
                                    &rejoin,
                                    "warn",
                                    "Could not rejoin the last mesh",
                                    &error,
                                );
                            }
                        }
                    });
                }
            }

            // Update check 10 s after start, then every 6 h; the window shows the dialog.
            let check = handle.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(UPDATE_FIRST).await;
                loop {
                    match commands::run_update_check(&check, false).await {
                        Ok(info) => {
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
                        Err(error) => tracing::info!("update check failed: {error}"),
                    }
                    tokio::time::sleep(UPDATE_EVERY).await;
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
        .build(tauri::generate_context!())
        .expect("GradApp failed to start")
        .run(|app, event| {
            // Quit (tray, window, OS): stop the worker, coordinator, join page
            // and beacon so no Python process outlives the app.
            if let RunEvent::Exit = event {
                let app = app.clone();
                tauri::async_runtime::block_on(async move {
                    peers::stop(&app).await;
                    let backend = app.state::<AppBackend>();
                    if tokio::time::timeout(Duration::from_secs(15), backend.shutdown(&app))
                        .await
                        .is_err()
                    {
                        tracing::warn!("shutdown took longer than 15 s");
                    }
                });
            }
        });
}
