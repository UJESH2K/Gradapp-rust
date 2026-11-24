//! The window's API. `app/ui/js/api.js` is the only caller; every name here
//! has a twin in its mock backend. All commands are async so a slow probe or
//! network call never blocks the window's main thread.

use std::sync::Mutex;

use gm_types::{AppEvent, UpdateInfo, APP_VERSION};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_opener::OpenerExt as _;
use tauri_plugin_updater::UpdaterExt as _;

use crate::backend::{emit, AppBackend};
use crate::model::*;

type Res<T> = Result<T, String>;

/// The update found by the last `check_update`, kept for `install_update`.
#[derive(Default)]
pub struct PendingUpdate(pub Mutex<Option<tauri_plugin_updater::Update>>);

const STABLE_FEED: &str = "https://github.com/UJESH2K/gradapp/releases/latest/download/latest.json";
const BETA_FEED: &str = "https://github.com/UJESH2K/gradapp/releases/download/beta/latest.json";

#[tauri::command]
pub async fn app_status(state: State<'_, AppBackend>) -> Res<AppStatus> {
    Ok(state.status())
}

#[tauri::command]
pub async fn logs(state: State<'_, AppBackend>) -> Res<Vec<LogEntry>> {
    Ok(state.logs())
}

#[tauri::command]
pub async fn get_device(state: State<'_, AppBackend>) -> Res<DeviceReport> {
    Ok(state.device().await)
}

#[tauri::command]
pub async fn get_ports(state: State<'_, AppBackend>) -> Res<Vec<PortEntry>> {
    Ok(state.ports().await)
}

#[tauri::command]
pub async fn install_port(
    app: AppHandle,
    state: State<'_, AppBackend>,
    port_id: String,
) -> Res<gm_types::PortStatus> {
    state.install_port(&app, port_id)
}

#[tauri::command]
pub async fn discover_start(app: AppHandle, state: State<'_, AppBackend>) -> Res<()> {
    state.discover_start(&app);
    Ok(())
}

#[tauri::command]
pub async fn discover_stop(state: State<'_, AppBackend>) -> Res<()> {
    state.discover_stop();
    Ok(())
}

#[tauri::command]
pub async fn join(
    app: AppHandle,
    state: State<'_, AppBackend>,
    invite: String,
) -> Res<gm_types::Invite> {
    state.join(&app, &invite).await
}

#[tauri::command]
pub async fn leave(app: AppHandle, state: State<'_, AppBackend>) -> Res<()> {
    state.leave(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn host_start(
    app: AppHandle,
    state: State<'_, AppBackend>,
    mesh_name: Option<String>,
) -> Res<HostInfo> {
    state.host_start(&app, mesh_name).await
}

#[tauri::command]
pub async fn host_stop(app: AppHandle, state: State<'_, AppBackend>) -> Res<()> {
    state.host_stop(&app).await;
    Ok(())
}

#[tauri::command]
pub async fn mesh_snapshot(state: State<'_, AppBackend>) -> Res<Option<MeshSnapshot>> {
    state.mesh_snapshot().await
}

#[tauri::command]
pub async fn runs(state: State<'_, AppBackend>) -> Res<Vec<RunSummary>> {
    state.runs().await
}

#[tauri::command]
pub async fn datasets(state: State<'_, AppBackend>) -> Res<Vec<Dataset>> {
    state.datasets().await
}

/// "Check now" in the window: always asks GitHub (no throttle).
#[tauri::command]
pub async fn check_update(app: AppHandle) -> Res<UpdateInfo> {
    run_update_check(&app, true).await
}

/// Ask GitHub Releases (gm-update: highest semver, stable or beta channel,
/// ETag cache) whether a newer GradApp exists. When the release carries a
/// signed `latest.json`, prepare the Tauri updater from that exact manifest
/// so "Update now" installs in place; otherwise the update is link-only
/// (`installable: false`) and the window offers the download page.
pub async fn run_update_check(app: &AppHandle, force: bool) -> Res<UpdateInfo> {
    let state = app.state::<AppBackend>();
    let beta = state.settings().update_channel == "beta";
    let channel = if beta {
        gm_update::Channel::Beta
    } else {
        gm_update::Channel::Stable
    };
    let checker = gm_update::Checker::new().with_cache(state.data_dir().join("update-check.json"));
    let checked = checker
        .check_detailed(APP_VERSION, gm_update::DEFAULT_REPO, channel, force)
        .await;
    let mut info = checked.info;
    info.reason = checked.reason;
    info.manifest_url = checked.manifest_url.clone();
    let mut found = None;
    if info.available && info.installable {
        let feed = checked
            .manifest_url
            .clone()
            .unwrap_or_else(|| (if beta { BETA_FEED } else { STABLE_FEED }).to_string());
        match signed_update(app, &feed).await {
            Ok(Some(update)) => found = Some(update),
            Ok(None) => {
                info.installable = false;
                info.reason = Some("The release has no signed build for this platform.".into());
            }
            Err(error) => {
                info.installable = false;
                info.reason = Some(format!("The signed update could not be prepared: {error}"));
            }
        }
    }
    if let Some(reason) = &info.reason {
        tracing::info!("update check: {reason}");
    }
    *app.state::<PendingUpdate>()
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = found;
    state.set_update(info.available.then(|| info.clone()));
    if info.available {
        emit(app, AppEvent::Update { info: info.clone() });
    }
    Ok(info)
}

async fn signed_update(
    app: &AppHandle,
    feed: &str,
) -> Result<Option<tauri_plugin_updater::Update>, String> {
    let url = tauri::Url::parse(feed).map_err(|e| e.to_string())?;
    let updater = app
        .updater_builder()
        .endpoints(vec![url])
        .map_err(|e| e.to_string())?
        .build()
        .map_err(|e| e.to_string())?;
    updater.check().await.map_err(|e| e.to_string())
}

/// Download and install the pending update, streaming UpdateProgress events.
/// The UI then offers `restart_app`.
#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    state: State<'_, AppBackend>,
    pending: State<'_, PendingUpdate>,
) -> Res<()> {
    let taken = pending.0.lock().unwrap_or_else(|p| p.into_inner()).take();
    let update = match taken {
        Some(update) => update,
        None => {
            let info = run_update_check(&app, true).await?;
            let taken = pending.0.lock().unwrap_or_else(|p| p.into_inner()).take();
            match taken {
                Some(update) => update,
                None if info.available => {
                    return Err(info.reason.unwrap_or_else(|| {
                        "This update must be downloaded from the release page.".into()
                    }))
                }
                None => return Err("GradApp is already up to date.".into()),
            }
        }
    };
    let mut downloaded: u64 = 0;
    let progress_app = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                emit(
                    &progress_app,
                    AppEvent::UpdateProgress { downloaded, total },
                );
            },
            || {},
        )
        .await
        .map_err(|e| format!("The update could not be installed: {e}"))?;
    state.set_update(None);
    Ok(())
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart();
}

#[tauri::command]
pub async fn get_settings(state: State<'_, AppBackend>) -> Res<Settings> {
    Ok(state.settings())
}

#[tauri::command]
pub async fn set_settings(
    app: AppHandle,
    state: State<'_, AppBackend>,
    settings: Settings,
) -> Res<Settings> {
    let before = state.settings();
    if before.start_at_login != settings.start_at_login {
        let launcher = app.autolaunch();
        let result = if settings.start_at_login {
            launcher.enable()
        } else {
            launcher.disable()
        };
        result.map_err(|e| format!("Could not change start at login: {e}"))?;
    }
    state.save_settings_from_ui(settings)
}

#[tauri::command]
pub async fn open_data_dir(app: AppHandle, state: State<'_, AppBackend>) -> Res<()> {
    let dir = state.data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn open_url(app: AppHandle, url: String) -> Res<()> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("Only web links can be opened.".into());
    }
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn hide_window(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    app.exit(0);
}
