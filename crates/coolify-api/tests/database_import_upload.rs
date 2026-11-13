//! `POST /databases/{uuid}/imports/uploads`: Dropzone chunks are appended to `<upload_id>.part`
//! within the 10 GiB limit, counted while a chunk is written and before it is appended.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::Request;
use common::Harness;
use http_body_util::BodyExt;
use tower::ServiceExt;

const FIXTURE: &str = r#"
INSERT INTO projects (id, uuid, name, team_id, created_at, updated_at) VALUES (800, 'proj', 'Web', 1, now(), now());
INSERT INTO environments (id, uuid, name, project_id, created_at, updated_at) VALUES (810, 'env', 'production', 800, now(), now());
INSERT INTO standalone_postgresqls (id, uuid, name, postgres_password, environment_id, destination_type, destination_id)
  VALUES (840, 'pg-a', 'main-db', 'x', 810, 'App\Models\StandaloneDocker', 0);
"#;

const MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;
const BOUNDARY: &str = "XboundaryX";

/// Where the API keeps unfinished uploads (`import_data_dir()/upload-tmp`).
fn upload_tmp() -> std::path::PathBuf {
    std::env::var_os("COOLIFY_RS_DATA_DIR")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
        .join("upload-tmp")
}

fn multipart(fields: &[(&str, &str)], file: &[u8], file_first: bool) -> Vec<u8> {
    let mut text = Vec::new();
    for (k, v) in fields {
        text.extend_from_slice(
            format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    let mut part = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"dump.sql\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    part.extend_from_slice(file);
    part.extend_from_slice(b"\r\n");
    let mut body = Vec::new();
    if file_first {
        body.extend(part);
        body.extend(text);
    } else {
        body.extend(text);
        body.extend(part);
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn upload(h: &Harness, body: Vec<u8>) -> (u16, String) {
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/databases/pg-a/imports/uploads")
        .header(
            "Authorization",
            format!("Bearer {}", h.token("acme_owner_write")),
        )
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(Body::from(body))
        .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

/// A UUID no other run uses.
fn fresh_uuid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!(
        "{:08x}-{:04x}-4000-8000-{:012x}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed) & 0xffff,
        nanos & 0xffff_ffff_ffff
    )
}

const TOO_LARGE: &str = r#"{"message":"Validation failed.","errors":{"file":["The backup exceeds the 10 GiB limit."]}}"#;

#[tokio::test]
async fn chunks_are_appended_within_the_size_limit() {
    let h = Harness::with_extra_sql(FIXTURE).await;
    std::fs::create_dir_all(upload_tmp()).unwrap();

    // A chunk within the limit is appended.
    let id = fresh_uuid();
    let fields = [
        ("upload_id", id.as_str()),
        ("dzchunkindex", "0"),
        ("dztotalchunkcount", "2"),
    ];
    let (status, body) = upload(&h, multipart(&fields, b"select 1;\n", false)).await;
    assert_eq!(status, 200, "{body}");
    let part = upload_tmp().join(format!("{id}.part"));
    assert_eq!(std::fs::metadata(&part).unwrap().len(), 10);
    std::fs::remove_file(&part).unwrap();

    // An upload already near the limit (a sparse file) takes no chunk that would pass it, whether
    // the chunk fields come before the file or after it; the unfinished upload is dropped.
    for file_first in [false, true] {
        let id = fresh_uuid();
        let part = upload_tmp().join(format!("{id}.part"));
        std::fs::File::create(&part)
            .unwrap()
            .set_len(MAX_BYTES - 3)
            .unwrap();
        let fields = [
            ("upload_id", id.as_str()),
            ("dzchunkindex", "5"),
            ("dztotalchunkcount", "9"),
        ];
        let (status, body) = upload(&h, multipart(&fields, b"0123456789", file_first)).await;
        assert_eq!(status, 422, "file first: {file_first}: {body}");
        assert_eq!(body, TOO_LARGE);
        assert!(!part.exists(), "file first: {file_first}");
    }
    h.finish().await;
}
