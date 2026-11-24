//! The coordinator client's dataset and run management calls, the streamed
//! multipart upload and the authenticated downloads, against an axum mock
//! shaped like `desktop/engine/coordinator/app.py`.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use gm_net::client::CoordinatorClient;
use gm_net::NetError;
use serde_json::{json, Value};

const TOKEN: &str = "t0k3n";

#[derive(Default)]
struct Seen {
    upload: Option<(String, Vec<u8>)>,
    calls: Vec<String>,
}

type Shared = Arc<Mutex<Seen>>;

fn ok(headers: &HeaderMap) -> bool {
    headers.get("x-mesh-token").and_then(|v| v.to_str().ok()) == Some(TOKEN)
}

fn denied() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"detail": "Invalid or missing mesh token"})),
    )
        .into_response()
}

async fn upload(State(seen): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    if !ok(&headers) {
        return denied();
    }
    let ctype = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !ctype.starts_with("multipart/form-data; boundary=") {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"detail": [{"msg": "Field required"}]})),
        )
            .into_response();
    }
    seen.lock().unwrap().upload = Some((ctype, body.to_vec()));
    Json(json!({"id": "d9", "name": "parts", "train_count": 3, "class_names": ["a"], "bytes": 1}))
        .into_response()
}

async fn mock() -> (SocketAddr, Shared) {
    let seen: Shared = Arc::default();
    let log = |seen: &Shared, what: &str| seen.lock().unwrap().calls.push(what.to_string());
    let router = Router::new()
        .route(
            "/datasets",
            post(upload).get(|headers: HeaderMap| async move {
                if !ok(&headers) {
                    return denied();
                }
                Json(json!({"datasets": []})).into_response()
            }),
        )
        .route(
            "/datasets/standard",
            get(|| async {
                Json(json!({"catalogue": [{"key": "coco8", "name": "COCO8", "images": 8}], "import": {"running": false, "key": null, "message": null, "error": null}}))
            })
            .post({
                let seen = seen.clone();
                move |headers: HeaderMap, Json(body): Json<Value>| async move {
                    if !ok(&headers) {
                        return denied();
                    }
                    log(&seen, &format!("import {}", body["key"].as_str().unwrap_or("")));
                    if body["key"] != "coco8" {
                        return (StatusCode::NOT_FOUND, Json(json!({"detail": "Unknown dataset 'x'"}))).into_response();
                    }
                    Json(json!({"status": "importing", "key": "coco8"})).into_response()
                }
            }),
        )
        .route(
            "/datasets/:id/default",
            post({
                let seen = seen.clone();
                move |Path(id): Path<String>| async move {
                    log(&seen, &format!("default {id}"));
                    Json(json!({"status": "ok"}))
                }
            }),
        )
        .route(
            "/datasets/:id",
            delete({
                let seen = seen.clone();
                move |Path(id): Path<String>| async move {
                    log(&seen, &format!("delete {id}"));
                    if id == "busy" {
                        return (StatusCode::CONFLICT, Json(json!({"detail": "This dataset is in use by a running job"}))).into_response();
                    }
                    Json(json!({"status": "deleted"})).into_response()
                }
            }),
        )
        .route(
            "/datasets/:id/download",
            get(|headers: HeaderMap, Path(id): Path<String>| async move {
                if !ok(&headers) {
                    return denied();
                }
                if id == "gone" {
                    return (StatusCode::GONE, Json(json!({"detail": "This dataset's files are not on the host any more"}))).into_response();
                }
                (
                    [
                        (header::CONTENT_TYPE, "application/zip"),
                        (header::CONTENT_DISPOSITION, "attachment; filename=\"COCO8.zip\""),
                    ],
                    vec![7u8; 300_000],
                )
                    .into_response()
            }),
        )
        .route(
            "/runs/:id/artifact",
            get(|headers: HeaderMap| async move {
                if !ok(&headers) {
                    return denied();
                }
                // The caller's name wins over the server's.
                ([(header::CONTENT_TYPE, "application/octet-stream")], b"weights".to_vec()).into_response()
            }),
        )
        .route(
            "/runs/:id/weights.bin",
            get(|| async { (StatusCode::NO_CONTENT, [("x-round", "0")]).into_response() }),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (addr, seen)
}

#[tokio::test]
async fn dataset_management_calls() {
    let (addr, seen) = mock().await;
    let c = CoordinatorClient::new(format!("http://{addr}"), Some(TOKEN.into()));
    let catalogue = c.standard_datasets().await.unwrap();
    assert_eq!(catalogue["catalogue"][0]["key"], "coco8");
    assert_eq!(catalogue["import"]["running"], false);
    assert_eq!(
        c.import_standard("coco8", true).await.unwrap()["status"],
        "importing"
    );
    let err = c.import_standard("x", false).await.unwrap_err();
    assert_eq!(err.status(), Some(404));
    assert_eq!(err.to_string(), "Unknown dataset 'x'");
    c.set_default_dataset("d1").await.unwrap();
    c.delete_dataset("d1").await.unwrap();
    let busy = c.delete_dataset("busy").await.unwrap_err();
    assert_eq!(busy.status(), Some(409));
    // Ids are checked by the allowlist before anything is sent.
    assert!(matches!(
        c.delete_dataset("../x").await.unwrap_err(),
        NetError::Blocked(_)
    ));
    assert_eq!(
        seen.lock().unwrap().calls,
        vec![
            "import coco8",
            "import x",
            "default d1",
            "delete d1",
            "delete busy"
        ]
    );
}

#[tokio::test]
async fn upload_streams_multipart_with_progress() {
    let (addr, seen) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let zip = dir.path().join("parts.zip");
    // Bigger than one 256 KB chunk, so progress is reported more than once.
    let payload: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&zip, &payload).unwrap();

    let c = CoordinatorClient::new(format!("http://{addr}"), Some(TOKEN.into()));
    let ticks = Arc::new(Mutex::new(Vec::new()));
    let record = ticks.clone();
    let reply = c
        .upload_dataset(&zip, "parts", true, move |sent, total| {
            record.lock().unwrap().push((sent, total))
        })
        .await
        .unwrap();
    assert_eq!(reply["id"], "d9");

    let ticks = ticks.lock().unwrap().clone();
    assert!(ticks.len() >= 3, "{ticks:?}");
    assert!(ticks.windows(2).all(|w| w[0].0 < w[1].0));
    assert_eq!(ticks.last().unwrap(), &(700_000, 700_000));

    let (ctype, body) = seen.lock().unwrap().upload.clone().unwrap();
    let boundary = ctype.split("boundary=").nth(1).unwrap().to_string();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("name=\"name\"\r\n\r\nparts\r\n"));
    assert!(text.contains("name=\"make_default\"\r\n\r\ntrue\r\n"));
    assert!(text.contains("name=\"file\"; filename=\"parts.zip\""));
    assert!(text.contains("Content-Type: application/zip"));
    // The file's bytes arrive intact inside the part.
    let start = body
        .windows(4)
        .enumerate()
        .filter(|(_, w)| *w == b"\r\n\r\n")
        .map(|(i, _)| i + 4)
        .find(|i| body[*i..].starts_with(&payload[..64]))
        .unwrap();
    assert_eq!(&body[start..start + payload.len()], &payload[..]);
    assert!(text.trim_end().ends_with(&format!("--{boundary}--")));

    // A wrong token is FastAPI's detail, not a generic error.
    let bad = CoordinatorClient::new(format!("http://{addr}"), Some("nope".into()));
    let err = bad
        .upload_dataset(&zip, "parts", false, |_, _| {})
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(401));
    assert_eq!(err.to_string(), "Invalid or missing mesh token");
    // A missing file fails before any request.
    assert!(matches!(
        c.upload_dataset(&dir.path().join("none.zip"), "x", false, |_, _| {})
            .await,
        Err(NetError::Io(_))
    ));
}

#[tokio::test]
async fn downloads_name_files_and_never_overwrite() {
    let (addr, _) = mock().await;
    let dir = tempfile::tempdir().unwrap();
    let c = CoordinatorClient::new(format!("http://{addr}"), Some(TOKEN.into()));

    let mut last = (0, None);
    let first = c
        .download_file("/datasets/d1/download", dir.path(), "", |n, t| {
            last = (n, t)
        })
        .await
        .unwrap();
    assert_eq!(first, dir.path().join("COCO8.zip"));
    assert_eq!(std::fs::metadata(&first).unwrap().len(), 300_000);
    assert_eq!(last, (300_000, Some(300_000)));
    let second = c
        .download_file("/datasets/d1/download", dir.path(), "", |_, _| {})
        .await
        .unwrap();
    assert_eq!(second, dir.path().join("COCO8 (1).zip"));

    let weights = c
        .download_file(
            "/runs/r1/artifact",
            &dir.path().join("out"),
            "smoke-r1.pt",
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(weights, dir.path().join("out").join("smoke-r1.pt"));
    let named = c
        .download_file(
            "/datasets/d1/download",
            dir.path(),
            "my copy.zip",
            |_, _| {},
        )
        .await
        .unwrap();
    assert_eq!(named, dir.path().join("my copy.zip"));
    assert_eq!(std::fs::read(&weights).unwrap(), b"weights");

    let gone = c
        .download_file("/datasets/gone/download", dir.path(), "x.zip", |_, _| {})
        .await
        .unwrap_err();
    assert_eq!(gone.status(), Some(410));
    assert!(gone.to_string().contains("not on the host"));
    let empty = c
        .download_file("/runs/r1/weights.bin", dir.path(), "w.bin", |_, _| {})
        .await
        .unwrap_err();
    assert_eq!(empty.status(), Some(204));
    assert!(matches!(
        c.download_file("/runs/r1/shards/0.zip", dir.path(), "x", |_, _| {})
            .await,
        Err(NetError::Blocked(_))
    ));
    // No partial files are left behind by the failures.
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.ends_with(".part")), "{names:?}");
}
