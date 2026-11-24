//! Datasets screen: what the mesh can train on, and the host's controls for it
//! (import a standard set, upload a YOLO .zip, set the default, delete,
//! download). Also the file transfers (uploads and downloads with progress on
//! `gm://transfer`, cancellable) that `runs.rs` reuses for trained weights,
//! and the native file/folder pickers.
//!
//! Coordinator routes (`desktop/engine/coordinator/app.py`): `GET /datasets`,
//! `GET|POST /datasets/standard`, `POST /datasets` (multipart: `file`, `name`,
//! `make_default`), `POST /datasets/{id}/default`, `DELETE /datasets/{id}`,
//! `GET /datasets/{id}/download`.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use gm_net::CoordinatorClient;
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt as _;

use crate::backend::{now, AppBackend};

type Res<T> = Result<T, String>;

/// Window event channel for upload/download progress (payload: [`Transfer`]).
pub const TRANSFER_EVENT: &str = "gm://transfer";

/// The mesh this app is in, or a message for the window.
pub(crate) fn access(state: &AppBackend) -> Res<(CoordinatorClient, bool)> {
    state
        .mesh_access()
        .ok_or_else(|| "This machine is not in a mesh.".to_string())
}

/// The mesh this app hosts, or why the action is the host's.
pub(crate) fn host_access(state: &AppBackend) -> Res<CoordinatorClient> {
    match access(state)? {
        (client, true) => Ok(client),
        (_, false) => Err("Only the machine hosting the mesh can change this.".into()),
    }
}

fn s(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(t) if !t.is_empty() => Some(t.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn u(v: &Value, key: &str) -> u64 {
    v.get(key)
        .and_then(|x| x.as_u64().or_else(|| x.as_f64().map(|f| f.max(0.0) as u64)))
        .unwrap_or(0)
}

// ---------------------------------------------------------------- overview

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DatasetInfo {
    pub id: String,
    pub name: String,
    pub images: u64,
    pub val_images: u64,
    pub classes: u32,
    /// The first dozen class names, for a hint on the card.
    pub class_names: Vec<String>,
    pub is_default: bool,
    /// The uploaded archive's size; `None` for imports (not copied).
    pub size_mb: Option<f64>,
    /// The files are on the host (a moved or deleted folder makes this false).
    pub available: bool,
    /// "upload" | "standard" | "subset"
    pub source: String,
    pub standard_key: Option<String>,
    pub created_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CatalogueEntry {
    pub key: String,
    pub name: String,
    pub images: u64,
    pub classes: u64,
    pub download_mb: u64,
    pub blurb: String,
    pub good_for: String,
    /// Already imported into this mesh.
    pub on_mesh: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct ImportState {
    pub running: bool,
    pub key: Option<String>,
    pub message: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DatasetsOverview {
    pub datasets: Vec<DatasetInfo>,
    pub catalogue: Vec<CatalogueEntry>,
    pub import: ImportState,
    /// This app hosts the mesh, so it may import, upload, change and delete.
    pub can_manage: bool,
    /// The host has PyTorch: standard imports (Ultralytics) and runs need it.
    pub torch_ready: Option<bool>,
}

/// `GET /datasets` → cards.
pub fn dataset_infos(v: &Value) -> Vec<DatasetInfo> {
    let list = v
        .get("datasets")
        .and_then(Value::as_array)
        .or_else(|| v.as_array());
    list.map(|items| {
        items
            .iter()
            .map(|d| {
                let names: Vec<String> = d
                    .get("class_names")
                    .and_then(Value::as_array)
                    .map(|c| {
                        c.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let source = if d.get("is_subset").and_then(Value::as_bool) == Some(true)
                    || d.get("parent_id").is_some_and(|p| !p.is_null())
                {
                    "subset"
                } else if s(d, "source").as_deref() == Some("standard") {
                    "standard"
                } else {
                    "upload"
                };
                let bytes = u(d, "bytes");
                DatasetInfo {
                    id: s(d, "id").unwrap_or_default(),
                    name: s(d, "name").unwrap_or_else(|| "Dataset".into()),
                    images: u(d, "train_count"),
                    val_images: u(d, "val_count"),
                    classes: names.len() as u32,
                    class_names: names.into_iter().take(12).collect(),
                    is_default: d.get("is_default").and_then(Value::as_bool) == Some(true),
                    size_mb: (bytes > 0)
                        .then(|| (bytes as f64 / 1_048_576.0 * 10.0).round() / 10.0),
                    available: d.get("available").and_then(Value::as_bool) != Some(false),
                    source: source.into(),
                    standard_key: s(d, "standard_key"),
                    created_at: d.get("created_at").and_then(Value::as_f64),
                }
            })
            .collect()
    })
    .unwrap_or_default()
}

/// `GET /datasets/standard` → catalogue + import state.
pub fn catalogue(v: &Value, have: &[DatasetInfo]) -> (Vec<CatalogueEntry>, ImportState) {
    let entries = v
        .get("catalogue")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|e| {
                    let key = s(e, "key").unwrap_or_default();
                    CatalogueEntry {
                        on_mesh: have
                            .iter()
                            .any(|d| d.standard_key.as_deref() == Some(key.as_str())),
                        name: s(e, "name").unwrap_or_else(|| key.clone()),
                        images: u(e, "images"),
                        classes: u(e, "classes"),
                        download_mb: u(e, "download_mb"),
                        blurb: s(e, "blurb").unwrap_or_default(),
                        good_for: s(e, "good_for").unwrap_or_default(),
                        key,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let import = v
        .get("import")
        .map(|i| ImportState {
            running: i.get("running").and_then(Value::as_bool) == Some(true),
            key: s(i, "key"),
            message: s(i, "message"),
            error: s(i, "error"),
        })
        .unwrap_or_default();
    (entries, import)
}

#[tauri::command]
pub async fn datasets_overview(state: State<'_, AppBackend>) -> Res<DatasetsOverview> {
    let (client, can_manage) = access(&state)?;
    let (list, standard, health) = tokio::join!(
        client.datasets(),
        client.standard_datasets(),
        client.health()
    );
    let datasets = dataset_infos(&list.map_err(|e| format!("Could not read datasets: {e}"))?);
    // An older coordinator without the catalogue still lists its datasets.
    let (catalogue, import) = standard
        .map(|v| catalogue(&v, &datasets))
        .unwrap_or_default();
    Ok(DatasetsOverview {
        datasets,
        catalogue,
        import,
        can_manage,
        torch_ready: health.ok().map(|h| h.torch_ready),
    })
}

#[tauri::command]
pub async fn dataset_import(
    state: State<'_, AppBackend>,
    key: String,
    make_default: bool,
) -> Res<()> {
    let client = host_access(&state)?;
    client
        .import_standard(&key, make_default)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dataset_set_default(state: State<'_, AppBackend>, id: String) -> Res<()> {
    let client = host_access(&state)?;
    client
        .set_default_dataset(&id)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dataset_delete(state: State<'_, AppBackend>, id: String) -> Res<()> {
    let client = host_access(&state)?;
    client
        .delete_dataset(&id)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Upload a YOLO .zip from disk. Returns at once with a transfer id; progress,
/// "processing" (the host unpacks and checks it) and the result arrive on
/// `gm://transfer`. The file is streamed, never read whole.
#[tauri::command]
pub async fn dataset_upload(
    app: AppHandle,
    state: State<'_, AppBackend>,
    path: String,
    name: String,
    make_default: bool,
) -> Res<String> {
    let client = host_access(&state)?;
    let path = PathBuf::from(path);
    let meta =
        std::fs::metadata(&path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    if !meta.is_file()
        || !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
    {
        return Err("Choose a .zip file: images/ and labels/ folders, plus a data.yaml.".into());
    }
    let name = match name.trim() {
        "" => path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "dataset".into()),
        n => n.chars().take(80).collect(),
    };
    let label = format!("Uploading {name}");
    Ok(start_transfer(
        &app,
        "upload",
        label,
        Some(meta.len()),
        move |report| async move {
            let progress = report.clone();
            let record = client
                .upload_dataset(&path, &name, make_default, move |sent, total| {
                    progress.progress(sent, Some(total))
                })
                .await
                .map_err(|e| e.to_string())?;
            let images = record
                .get("train_count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            Ok(Some(format!("{name}: {images} training images")))
        },
    ))
}

/// Download a dataset as a .zip into `dir`. Returns a transfer id.
#[tauri::command]
pub async fn dataset_download(
    app: AppHandle,
    state: State<'_, AppBackend>,
    id: String,
    name: String,
    dir: String,
) -> Res<String> {
    let (client, _) = access(&state)?;
    let label = format!("Downloading {name}");
    let fallback = format!("{name}.zip");
    Ok(start_transfer(
        &app,
        "download",
        label,
        None,
        move |report| async move {
            let path = client
                .download_file(
                    &format!("/datasets/{id}/download"),
                    std::path::Path::new(&dir),
                    &fallback,
                    move |done, total| report.progress(done, total),
                )
                .await
                .map_err(|e| e.to_string())?;
            Ok(Some(path.to_string_lossy().into_owned()))
        },
    ))
}

// ---------------------------------------------------------------- pickers

#[derive(Debug, Clone, Serialize)]
pub struct PickedFile {
    pub path: String,
    /// File name without `.zip`, a sensible default dataset name.
    pub name: String,
    pub bytes: u64,
}

/// Native "open" dialog for a YOLO dataset .zip.
#[tauri::command]
pub async fn pick_dataset_zip(app: AppHandle) -> Res<Option<PickedFile>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose a YOLO dataset (.zip)")
        .add_filter("YOLO dataset", &["zip"])
        .pick_file(move |file| {
            let _ = tx.send(file);
        });
    let Some(file) = rx.await.ok().flatten() else {
        return Ok(None);
    };
    let path = file.into_path().map_err(|e| e.to_string())?;
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(Some(PickedFile {
        name: path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path: path.to_string_lossy().into_owned(),
        bytes,
    }))
}

/// Native folder dialog (where to save a download). Starts in Downloads.
#[tauri::command]
pub async fn pick_folder(app: AppHandle, title: Option<String>) -> Res<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app
        .dialog()
        .file()
        .set_title(title.unwrap_or_else(|| "Choose a folder".into()));
    if let Some(dir) = dirs::download_dir() {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_folder(move |folder| {
        let _ = tx.send(folder);
    });
    match rx.await.ok().flatten() {
        Some(folder) => Ok(Some(
            folder
                .into_path()
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned(),
        )),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------- transfers

/// One upload or download, as the window draws it.
#[derive(Debug, Clone, Serialize)]
pub struct Transfer {
    pub id: String,
    /// "upload" | "download"
    pub kind: String,
    pub label: String,
    pub done: u64,
    pub total: Option<u64>,
    /// "running" | "processing" (upload sent, host unpacking) | "done" | "failed" | "cancelled"
    pub state: String,
    pub error: Option<String>,
    /// Where a download was saved, or what an upload became.
    pub result: Option<String>,
    pub started_at: f64,
}

struct Slot {
    transfer: Transfer,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

static TRANSFERS: LazyLock<Mutex<HashMap<String, Slot>>> = LazyLock::new(Default::default);
const KEEP_FINISHED: usize = 12;

fn transfers_lock() -> std::sync::MutexGuard<'static, HashMap<String, Slot>> {
    TRANSFERS.lock().unwrap_or_else(|p| p.into_inner())
}

/// Progress reporting for one transfer: updates the slot and emits at most
/// ~8 events a second (always the last one).
#[derive(Clone)]
pub(crate) struct Reporter {
    app: AppHandle,
    id: String,
    last: Arc<Mutex<Option<Instant>>>,
}

impl Reporter {
    pub(crate) fn progress(&self, done: u64, total: Option<u64>) {
        let finished = total.is_some_and(|t| done >= t);
        {
            let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
            if !finished && last.is_some_and(|l| l.elapsed() < Duration::from_millis(120)) {
                return;
            }
            *last = Some(Instant::now());
        }
        let snapshot = {
            let mut all = transfers_lock();
            let Some(slot) = all.get_mut(&self.id) else {
                return;
            };
            let t = &mut slot.transfer;
            if !matches!(t.state.as_str(), "running" | "processing") {
                return;
            }
            t.done = done;
            t.total = total.or(t.total);
            if finished && t.kind == "upload" {
                t.state = "processing".into();
            }
            t.clone()
        };
        let _ = self.app.emit(TRANSFER_EVENT, snapshot);
    }
}

fn finish(app: &AppHandle, id: &str, state: &str, error: Option<String>, result: Option<String>) {
    let snapshot = {
        let mut all = transfers_lock();
        let Some(slot) = all.get_mut(id) else { return };
        if matches!(
            slot.transfer.state.as_str(),
            "cancelled" | "done" | "failed"
        ) {
            return;
        }
        let t = &mut slot.transfer;
        t.state = state.into();
        t.error = error;
        t.result = result;
        if state == "done" {
            if let Some(total) = t.total {
                t.done = total;
            }
        }
        slot.task = None;
        let snapshot = t.clone();
        // Forget old finished transfers.
        let mut finished: Vec<(String, f64)> = all
            .iter()
            .filter(|(_, s)| s.task.is_none())
            .map(|(k, s)| (k.clone(), s.transfer.started_at))
            .collect();
        if finished.len() > KEEP_FINISHED {
            finished.sort_by(|a, b| a.1.total_cmp(&b.1));
            for (k, _) in finished.iter().take(finished.len() - KEEP_FINISHED) {
                all.remove(k);
            }
        }
        snapshot
    };
    let _ = app.emit(TRANSFER_EVENT, snapshot);
}

/// Run `work` in the background as a tracked transfer and return its id.
/// `work` returns `Ok(Some(text))` for the result line (a saved path, a
/// dataset summary) or an error message.
pub(crate) fn start_transfer<F, Fut>(
    app: &AppHandle,
    kind: &str,
    label: String,
    total: Option<u64>,
    work: F,
) -> String
where
    F: FnOnce(Reporter) -> Fut + Send + 'static,
    Fut: Future<Output = Result<Option<String>, String>> + Send + 'static,
{
    let id = format!("t{:x}", (now() * 1000.0) as u64 ^ rand_bits());
    let transfer = Transfer {
        id: id.clone(),
        kind: kind.into(),
        label,
        done: 0,
        total,
        state: "running".into(),
        error: None,
        result: None,
        started_at: now(),
    };
    transfers_lock().insert(
        id.clone(),
        Slot {
            transfer: transfer.clone(),
            task: None,
        },
    );
    let _ = app.emit(TRANSFER_EVENT, transfer);
    let reporter = Reporter {
        app: app.clone(),
        id: id.clone(),
        last: Arc::default(),
    };
    let done_app = app.clone();
    let done_id = id.clone();
    let task = tauri::async_runtime::spawn(async move {
        match work(reporter).await {
            Ok(result) => finish(&done_app, &done_id, "done", None, result),
            Err(error) => {
                tracing::info!("transfer {done_id} failed: {error}");
                finish(&done_app, &done_id, "failed", Some(error), None)
            }
        }
    });
    if let Some(slot) = transfers_lock().get_mut(&id) {
        if slot.transfer.state == "running" || slot.transfer.state == "processing" {
            slot.task = Some(task);
        }
    }
    id
}

fn rand_bits() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(now().to_bits());
    h.finish() & 0xffff_ffff
}

/// Every transfer still known (running first), for a window that reopens a screen.
#[tauri::command]
pub async fn transfers() -> Res<Vec<Transfer>> {
    let mut list: Vec<Transfer> = transfers_lock()
        .values()
        .map(|s| s.transfer.clone())
        .collect();
    list.sort_by(|a, b| b.started_at.total_cmp(&a.started_at));
    Ok(list)
}

/// Cancel a running transfer: the request is dropped (a download's `.part`
/// file is removed; an upload stops sending and the host discards it).
#[tauri::command]
pub async fn transfer_cancel(app: AppHandle, id: String) -> Res<()> {
    let snapshot = {
        let mut all = transfers_lock();
        let Some(slot) = all.get_mut(&id) else {
            return Err("That transfer has already finished.".into());
        };
        if let Some(task) = slot.task.take() {
            task.abort();
        }
        if !matches!(slot.transfer.state.as_str(), "running" | "processing") {
            return Ok(());
        }
        slot.transfer.state = "cancelled".into();
        slot.transfer.clone()
    };
    let _ = app.emit(TRANSFER_EVENT, snapshot);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn datasets_and_catalogue_map() {
        // Shapes from the real coordinator (GET /datasets after an upload and a
        // standard import; GET /datasets/standard).
        let list = json!({"datasets": [
            {"id": "std-coco8", "name": "COCO8", "filename": "coco8.yaml", "bytes": 0, "train_count": 4, "val_count": 4,
             "class_names": (0..80).map(|i| format!("c{i}")).collect::<Vec<_>>(), "source": "standard", "standard_key": "coco8",
             "available": true, "is_default": true, "created_at": 1.0},
            {"id": "a1b2c3", "name": "parts", "bytes": 3_145_728, "train_count": 120, "val_count": 30, "class_names": ["bolt", "nut"],
             "available": false, "is_default": false}
        ]});
        let d = dataset_infos(&list);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].source, "standard");
        assert_eq!(d[0].classes, 80);
        assert_eq!(d[0].class_names.len(), 12);
        assert_eq!(d[0].size_mb, None);
        assert!(d[0].is_default && d[0].available);
        assert_eq!(d[1].source, "upload");
        assert_eq!(d[1].size_mb, Some(3.0));
        assert_eq!(d[1].val_images, 30);
        assert!(!d[1].available);

        let standard = json!({"catalogue": [
            {"key": "coco8", "name": "COCO8", "images": 8, "classes": 80, "download_mb": 1, "blurb": "Eight images.", "good_for": "smoke test", "already_downloaded": false},
            {"key": "coco128", "name": "COCO128", "images": 128, "classes": 80, "download_mb": 7, "blurb": "", "good_for": ""}
        ], "import": {"running": true, "key": "coco128", "message": "Resolving COCO128", "error": null}});
        let (cat, import) = catalogue(&standard, &d);
        assert!(cat[0].on_mesh && !cat[1].on_mesh);
        assert_eq!(cat[1].download_mb, 7);
        assert!(import.running);
        assert_eq!(import.message.as_deref(), Some("Resolving COCO128"));
        assert_eq!(import.error, None);
    }
}
