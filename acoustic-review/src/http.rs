//! Tiny HTTP layer: JSON API plus the static single-page workbench.

use crate::db::Db;
use crate::models::{ClockSegmentReq, CreateEventReq, CreateStationReq, EvidenceReq, IngestReq};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tiny_http::{Header, Method, Request, Response, Server};

pub struct App {
    pub db: Db,
}

pub fn listen(addr: SocketAddr, db_path: &str, reseed: bool) {
    let db = Db::open(db_path).expect("open database");
    let recovered = db.recover_pending_jobs().expect("crash recovery");
    if recovered > 0 {
        eprintln!("recovered {recovered} interrupted job(s)");
    }
    if reseed {
        let any: i64 = db
            .conn
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM stations", [], |r| r.get(0))
            .unwrap_or(0);
        if any == 0 {
            crate::synth::generate(&db);
            for event in ["EVT-IMPACT", "EVT-MIRROR", "EVT-DRIFT"] {
                let _ = crate::service::solve_event(&db, event);
            }
            eprintln!("seeded synthetic demo data");
        }
    }
    let app = Arc::new(App { db });
    let server = Server::http(addr).expect("bind listen address");
    eprintln!("acoustic-review listening on http://{addr}");
    for request in server.incoming_requests() {
        let app = Arc::clone(&app);
        handle(app, request);
    }
}

fn json_response(status: u16, value: &Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec(value).unwrap_or_default();
    Response::from_data(body)
        .with_status_code(status)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
}

fn error(status: u16, msg: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(status, &serde_json::json!({"error": msg}))
}

fn read_json<T: serde::de::DeserializeOwned>(request: &mut Request) -> Result<T, String> {
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| format!("invalid JSON: {e}"))
}

fn handle(app: Arc<App>, mut request: Request) {
    let method = request.method().clone();
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let result = route(&app, &method, path, query, &mut request);
    let response = match result {
        Ok(resp) => resp,
        Err((status, msg)) => error(status, &msg),
    };
    let _ = request.respond(response);
}

type Resp = Response<std::io::Cursor<Vec<u8>>>;

fn route(
    app: &App,
    method: &Method,
    path: &str,
    _query: &str,
    request: &mut Request,
) -> Result<Resp, (u16, String)> {
    match (method, path) {
        (Method::Get, "/") | (Method::Get, "/index.html") => {
            static_file("index.html", "text/html; charset=utf-8")
        }
        (Method::Get, "/app.js") => static_file("app.js", "application/javascript; charset=utf-8"),
        (Method::Get, "/app.css") => static_file("app.css", "text/css; charset=utf-8"),

        (Method::Get, "/api/stations") => {
            let v = crate::service::list_stations(&app.db).map_err(se)?;
            Ok(json_response(200, &serde_json::json!(v)))
        }
        (Method::Post, "/api/stations") => {
            let req: CreateStationReq = read_json(request).map_err(se)?;
            crate::service::create_station(&app.db, req).map_err(se)?;
            Ok(json_response(201, &serde_json::json!({"ok": true})))
        }
        (Method::Get, "/api/events") => {
            let v = crate::service::list_events(&app.db).map_err(se)?;
            Ok(json_response(200, &serde_json::json!(v)))
        }
        (Method::Post, "/api/events") => {
            let req: CreateEventReq = read_json(request).map_err(se)?;
            crate::service::create_event(&app.db, req).map_err(se)?;
            Ok(json_response(201, &serde_json::json!({"ok": true})))
        }
        (Method::Post, "/api/batches") => {
            let req: IngestReq = read_json(request).map_err(se)?;
            let res = crate::ingest::ingest(&app.db, req).map_err(se)?;
            Ok(json_response(
                200,
                &serde_json::json!({
                    "fingerprint": res.fingerprint,
                    "already_present": res.already_present,
                    "observation_ids": res.observation_ids,
                }),
            ))
        }
        (Method::Post, p) if p.starts_with("/api/events/") && p.ends_with("/link") => {
            let event = p
                .trim_start_matches("/api/events/")
                .trim_end_matches("/link");
            let n = crate::service::link_window(&app.db, event).map_err(se)?;
            Ok(json_response(200, &serde_json::json!({"linked": n})))
        }
        (Method::Get, p) if p.starts_with("/api/events/") => {
            let event = p.trim_start_matches("/api/events/");
            let detail = crate::service::event_detail(&app.db, event).map_err(se)?;
            Ok(json_response(200, &detail))
        }
        (Method::Post, p) if p.starts_with("/api/events/") && p.ends_with("/solve") => {
            let event = p
                .trim_start_matches("/api/events/")
                .trim_end_matches("/solve");
            let fp = crate::service::solve_event(&app.db, event).map_err(se)?;
            Ok(json_response(200, &serde_json::json!({"job_fp": fp})))
        }
        (Method::Post, p) if p.starts_with("/api/events/") && p.ends_with("/evidence") => {
            let event = p
                .trim_start_matches("/api/events/")
                .trim_end_matches("/evidence");
            let req: EvidenceReq = read_json(request).map_err(se)?;
            let id = crate::evidence::add_evidence(&app.db, event, req).map_err(se)?;
            Ok(json_response(201, &serde_json::json!({"content_id": id})))
        }
        (Method::Post, "/api/clock-segments") => {
            let req: ClockSegmentReq = read_json(request).map_err(se)?;
            let id = crate::clock::add_segment(&app.db, &req).map_err(se)?;
            Ok(json_response(201, &serde_json::json!({"seg_id": id})))
        }
        (Method::Post, "/api/seed") => {
            let v = crate::synth::generate(&app.db);
            Ok(json_response(201, &v))
        }
        (Method::Post, "/api/recover") => {
            let n = app
                .db
                .recover_pending_jobs()
                .map_err(|e| (500, e.to_string()))?;
            Ok(json_response(200, &serde_json::json!({"recovered": n})))
        }
        _ => Err((404, format!("no route for {method} {path}"))),
    }
}

fn se(e: String) -> (u16, String) {
    if e.contains("FOREIGN KEY") || e.contains("UNIQUE") || e.contains("CHECK") {
        (409, e)
    } else {
        (400, e)
    }
}

fn static_file(name: &str, content_type: &str) -> Result<Resp, (u16, String)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("static")
        .join(name);
    let body = std::fs::read(&path).map_err(|e| (404, e.to_string()))?;
    Ok(Response::from_data(body)
        .with_header(Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).unwrap()))
}
