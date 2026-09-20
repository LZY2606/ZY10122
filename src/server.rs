//! 极简 HTTP 服务：静态页面 + JSON API + 后台求解 worker。
//! 仅依赖 std::net，避免引入大型 Web 框架。

use rusqlite::Connection;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::db;
use crate::solver::{self, SolverEvent, SolverInput};
use crate::*;

struct App {
    db: Mutex<Connection>,
}

pub fn serve(listen: &str, db_path: &str) -> std::io::Result<()> {
    let conn = db::open(db_path).expect("open database");
    let app = Arc::new(App { db: Mutex::new(conn) });

    // 崩溃恢复 + 后台求解 worker。
    {
        let mut c = app.db.lock().unwrap();
        let _ = db::claim_next_job(&mut c);
    }
    spawn_worker(Arc::clone(&app));

    let listener = TcpListener::bind(listen)?;
    eprintln!("acoustic-review listening on http://{listen}");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let app = Arc::clone(&app);
                thread::spawn(move || {
                    let _ = handle_connection(app, stream);
                });
            }
            Err(e) => eprintln!("accept error: {e}"),
        }
    }
    Ok(())
}

fn spawn_worker(app: Arc<App>) {
    thread::spawn(move || loop {
        let claimed = {
            let mut c = app.db.lock().unwrap();
            db::claim_next_job(&mut c).expect("claim job")
        };
        if let Some(job_id) = claimed {
            run_job(&app, job_id);
        } else {
            thread::sleep(Duration::from_millis(250));
        }
    });
}

fn run_job(app: &Arc<App>, job_id: i64) {
    let input = {
        let c = app.db.lock().unwrap();
        build_solver_input(&c, job_id)
    };
    let input = match input {
        Ok(v) => v,
        Err(e) => {
            let mut c = app.db.lock().unwrap();
            let _ = db::fail_job(&mut c, job_id, &e);
            return;
        }
    };
    let out = solver::solve(input, job_id);
    let mut c = app.db.lock().unwrap();
    if let Err(e) = db::complete_job(&mut c, job_id, &out.candidates) {
        eprintln!("complete_job {job_id} failed: {e}");
        let _ = db::fail_job(&mut c, job_id, &e.to_string());
    }
}

fn build_solver_input(conn: &Connection, job_id: i64) -> Result<SolverInput, String> {
    let event_id: String = conn
        .query_row("SELECT event_id FROM solve_jobs WHERE job_id=?1", rusqlite::params![job_id], |r| {
            r.get(0)
        })
        .map_err(|e| e.to_string())?;
    let event = db::get_event(conn, &event_id)
        .map_err(|e| e.to_string())?
        .ok_or("event missing")?;
    Ok(SolverInput {
        event: SolverEvent {
            event_id: event.event_id.clone(),
            window_start: event.window_start,
            window_end: event.window_end,
            sound_speed_mps: event.sound_speed_mps,
            bounds: event.bounds,
        },
        stations: db::list_stations(conn).map_err(|e| e.to_string())?,
        clock_segments: db::list_clock_segments(conn).map_err(|e| e.to_string())?,
        observations: db::list_observations(conn, &event_id).map_err(|e| e.to_string())?,
        lags: db::list_lags(conn, &event_id).map_err(|e| e.to_string())?,
        evidence: db::list_evidence(conn, &event_id).map_err(|e| e.to_string())?,
    })
}

fn handle_connection(app: Arc<App>, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }
    let raw = String::from_utf8_lossy(&buf[..n]).to_string();
    let mut lines = raw.split("\r\n");
    let request = lines.next().unwrap_or("").to_string();
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    let mut content_length = 0usize;
    let mut headers_done = false;
    let mut header_bytes = 0usize;
    for line in raw.split("\r\n") {
        header_bytes += line.len() + 2;
        if line.is_empty() {
            headers_done = true;
            break;
        }
        if let Some(rest) = line.to_lowercase().strip_prefix("content-length:") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    let _ = headers_done;
    let mut body = String::new();
    if content_length > 0 {
        let already = n.saturating_sub(header_bytes);
        if let Some(idx) = raw.find("\r\n\r\n") {
            body.push_str(&raw[idx + 4..]);
        }
        while body.len() < content_length {
            let mut extra = vec![0u8; content_length - body.len()];
            let k = stream.read(&mut extra)?;
            if k == 0 {
                break;
            }
            body.push_str(&String::from_utf8_lossy(&extra[..k]));
        }
        let _ = already;
    }

    let resp = route(&app, method, path, query, &body);
    stream.write_all(&resp)?;
    stream.flush()?;
    Ok(())
}

fn response(status: u16, reason: &str, content_type: &str, body: Vec<u8>) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(&body);
    out
}

fn json_200<T: serde::Serialize>(v: &T) -> Vec<u8> {
    let body = serde_json::to_vec_pretty(v).unwrap_or_default();
    response(200, "OK", "application/json; charset=utf-8", body)
}

fn json_err(status: u16, reason: &str, msg: &str) -> Vec<u8> {
    let body = serde_json::json!({"error": msg}).to_string().into_bytes();
    response(status, reason, "application/json; charset=utf-8", body)
}

fn query_params(q: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for pair in q.split('&').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            m.insert(url_decode(k), url_decode(v));
        }
    }
    m
}

fn url_decode(s: &str) -> String {
    let bytes = s.replace('+', " ");
    let mut out = Vec::new();
    let mut chars = bytes.bytes();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h = chars.next().unwrap_or(b'0');
            let l = chars.next().unwrap_or(b'0');
            let hex = |c: u8| match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => 0,
            };
            out.push(hex(h) * 16 + hex(l));
        } else {
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn route(app: &Arc<App>, method: &str, path: &str, query: &str, body: &str) -> Vec<u8> {
    let q = query_params(query);
    match (method, path) {
        ("GET", "/") => serve_static("text/html; charset=utf-8", include_str!("../static/index.html")),
        ("GET", "/static/app.js") => serve_static("application/javascript; charset=utf-8", include_str!("../static/app.js")),
        ("GET", "/api/health") => json_200(&serde_json::json!({"ok": true})),

        ("GET", "/api/stations") => with_db(app, |c| Ok(json_200(&db::list_stations(c)?))),
        ("GET", "/api/events") => with_db(app, |c| {
            let events = db::list_events(c)?;
            Ok(json_200(&serde_json::json!({"events": events})))
        }),
        ("GET", "/api/event") => with_db(app, |c| {
            let id = q.get("event_id").cloned().unwrap_or_default();
            let event = db::get_event(c, &id)?;
            let stations = db::list_stations(c)?;
            let segments = db::list_clock_segments(c)?
                .into_iter().map(|(s, id)| serde_json::json!({"segment_id": id,
                    "station_id": s.station_id, "t_start": s.t_start, "t_end": s.t_end,
                    "offset_sec": s.offset_sec, "source": s.source})).collect::<Vec<_>>();
            let observations = db::list_observations(c, &id)?;
            let lags = db::list_lags(c, &id)?;
            let evidence = db::list_evidence(c, &id)?;
            let jobs = db::list_jobs(c, &id)?;
            let candidates = match db::latest_completed_job(c, &id)? {
                Some(jid) => db::list_candidates(c, jid)?,
                None => vec![],
            };
            Ok(json_200(&serde_json::json!({
                "event": event, "stations": stations, "clock_segments": segments,
                "observations": observations, "lags": lags, "evidence": evidence,
                "jobs": jobs, "candidates": candidates
            })))
        }),

        ("POST", "/api/stations") => write_json(body, |v: model::Station| {
            let c = app.db.lock().unwrap();
            db::upsert_station(&c, &v)?;
            Ok(json_200(&serde_json::json!({"ok": true})))
        }),
        ("POST", "/api/events") => write_json(body, |v: model::EventInput| {
            if !(v.window_end > v.window_start) || v.sound_speed_mps <= 0.0 {
                return Ok(json_err(400, "Bad Request", "window_end>window_start and sound_speed>0 required"));
            }
            let c = app.db.lock().unwrap();
            db::create_event(&c, &v)?;
            Ok(json_200(&serde_json::json!({"ok": true})))
        }),
        ("POST", "/api/clock-segments") => write_json(body, |v: model::ClockSegment| {
            let mut c = app.db.lock().unwrap();
            let mut segs = db::list_clock_segments(&c)?;
            segs.push((v.clone(), -1));
            if let Err(e) = db::validate_clock_partition(&segs.into_iter().map(|(s, _)| s).collect::<Vec<_>>()) {
                return Ok(json_err(400, "Bad Request", &e));
            }
            let id = db::insert_clock_segment(&mut c, &v)?;
            db::apply_clock_corrections(&mut c)?;
            Ok(json_200(&serde_json::json!({"ok": true, "segment_id": id})))
        }),
        ("POST", "/api/batches") => write_json(body, |v: serde_json::Value| {
            let batch: model::BatchInput = match serde_json::from_value(v.clone()) {
                Ok(b) => b,
                Err(e) => return Ok(json_err(400, "Bad Request", &e.to_string())),
            };
            let mut c = app.db.lock().unwrap();
            let res = db::ingest_batch(&mut c, &batch, &v)?;
            let added = matches!(res, db::IngestResult::Inserted { .. });
            Ok(json_200(&serde_json::json!({"inserted": added, "result": format!("{res:?}")})))
        }),

        ("POST", "/api/evidence") => write_json(body, |v: serde_json::Value| {
            let event_id = v.get("event_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let basis = v.get("basis").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let payload = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
            if event_id.is_empty() || !matches!(kind.as_str(), "exclude_reflection" | "lock_clock" | "keep_ambiguous") {
                return Ok(json_err(400, "Bad Request", "event_id and valid kind required"));
            }
            let mut c = app.db.lock().unwrap();
            let id = db::append_evidence(&mut c, &event_id, &kind, &payload, &basis)?;
            let _ = enqueue_for_event(&mut c, &event_id);
            Ok(json_200(&serde_json::json!({"evidence_id": id})))
        }),

        ("POST", "/api/solve") => write_json(body, |v: serde_json::Value| {
            let event_id = v.get("event_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let mut c = app.db.lock().unwrap();
            let job_id = enqueue_for_event(&mut c, &event_id)?;
            Ok(json_200(&serde_json::json!({"job_id": job_id})))
        }),

        _ => json_err(404, "Not Found", "no such route"),
    }
}

fn enqueue_for_event(c: &mut rusqlite::Connection, event_id: &str) -> rusqlite::Result<i64> {
    // 计算当前输入指纹并入队（已存在等价作业则复用）。
    let event = match db::get_event(c, event_id)? {
        Some(e) => e,
        None => return Err(rusqlite::Error::InvalidQuery),
    };
    let input = SolverInput {
        event: SolverEvent {
            event_id: event.event_id,
            window_start: event.window_start,
            window_end: event.window_end,
            sound_speed_mps: event.sound_speed_mps,
            bounds: event.bounds,
        },
        stations: db::list_stations(c)?,
        clock_segments: db::list_clock_segments(c)?,
        observations: db::list_observations(c, event_id)?,
        lags: db::list_lags(c, event_id)?,
        evidence: db::list_evidence(c, event_id)?,
    };
    let fp = solver::input_fingerprint(&input);
    db::enqueue_job(c, event_id, &fp)
}

fn serve_static(content_type: &str, body: &str) -> Vec<u8> {
    response(200, "OK", content_type, body.as_bytes().to_vec())
}

fn with_db<F>(app: &Arc<App>, f: F) -> Vec<u8>
where
    F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<Vec<u8>>,
{
    let c = app.db.lock().unwrap();
    match f(&c) {
        Ok(v) => v,
        Err(e) => json_err(500, "Internal Server Error", &e.to_string()),
    }
}

fn write_json<T, F>(body: &str, f: F) -> Vec<u8>
where
    T: serde::de::DeserializeOwned,
    F: FnOnce(T) -> rusqlite::Result<Vec<u8>>,
{
    match serde_json::from_str::<T>(body) {
        Ok(v) => match f(v) {
            Ok(bytes) => bytes,
            Err(e) => json_err(500, "Internal Server Error", &e.to_string()),
        },
        Err(e) => json_err(400, "Bad Request", &e.to_string()),
    }
}
