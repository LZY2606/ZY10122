//! 离线声学定位审查工具。
//!
//! 用法：
//!   acoustic-review --listen 127.0.0.1:5322 [--db acoustic.db] [--seed|--no-seed]

use std::process::ExitCode;

use serde_json::json;

use acoustic_review::{db, server, synthetic};

struct Args {
    listen: String,
    db_path: String,
    seed: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut listen = "127.0.0.1:5322".to_string();
    let mut db_path = "acoustic-review.db".to_string();
    let mut seed = true;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => listen = it.next().ok_or("missing --listen value")?,
            "--db" => db_path = it.next().ok_or("missing --db value")?,
            "--seed" => seed = true,
            "--no-seed" => seed = false,
            "--help" | "-h" => {
                println!("用法: acoustic-review --listen 127.0.0.1:5322 [--db PATH] [--no-seed]");
                std::process::exit(0);
            }
            other => return Err(format!("未知参数: {other}")),
        }
    }
    Ok(Args { listen, db_path, seed })
}

fn is_empty_database(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM stations", [], |r| r.get(0))?;
    Ok(n == 0)
}

fn seed_database(conn: &mut rusqlite::Connection) -> rusqlite::Result<()> {
    let data = synthetic::build();
    for s in &data.stations {
        db::upsert_station(conn, s)?;
    }
    for c in &data.clocks {
        db::insert_clock_segment(conn, c)?;
    }
    for e in &data.events {
        db::create_event(conn, e)?;
    }
    for b in &data.batches {
        let raw = json!(b);
        db::ingest_batch(conn, b, &raw)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut conn = match db::open(&args.db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("无法打开数据库 {}: {e}", args.db_path);
            return ExitCode::FAILURE;
        }
    };
    if args.seed {
        match is_empty_database(&conn) {
            Ok(true) => {
                if let Err(e) = seed_database(&mut conn) {
                    eprintln!("写入合成数据失败: {e}");
                    return ExitCode::FAILURE;
                }
                eprintln!("已写入合成演示数据（--no-seed 可关闭）");
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!("初始化失败: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    drop(conn);
    if let Err(e) = server::serve(&args.listen, &args.db_path) {
        eprintln!("服务退出: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
