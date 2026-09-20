//! Batch idempotency, crash recovery, and evidence-driven recomputation.

use acoustic_review::*;
use models::*;

fn station(db: &Db, id: &str) {
    service::create_station(
        db,
        CreateStationReq {
            station_id: id.into(),
            name: id.into(),
            x: 0.0,
            y: 0.0,
        },
    )
    .unwrap();
}

fn batch(station_id: &str, onset: f64) -> IngestReq {
    IngestReq {
        fingerprint: Some("fixed-batch-fp".into()),
        received_at: Some(1.0),
        observations: vec![IngestObs {
            station_id: station_id.into(),
            event_ref: Some("EVT-X".into()),
            t_device: onset,
            local_onset_s: onset,
            peak_band_hz: 800.0,
            snr_db: 12.0,
            xcorr: vec![],
        }],
    }
}

#[test]
fn replayed_batch_does_not_duplicate_observations() {
    let db = Db::open_memory().unwrap();
    station(&db, "A");
    let first = ingest::ingest(&db, batch("A", 40.1)).unwrap();
    assert!(!first.already_present);
    let second = ingest::ingest(&db, batch("A", 40.1)).unwrap();
    assert!(second.already_present);
    assert_eq!(first.observation_ids, second.observation_ids);

    let count: i64 = db
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn crashed_job_is_never_auditable_and_is_recomputed() {
    let db = Db::open_memory().unwrap();
    synth::generate(&db);
    let event = "EVT-IMPACT";
    let model = units::PhysicalModel::default();
    let snap = jobs::build_snapshot(&db, event, model).unwrap();

    // Simulate a worker dying after writing a 'running' job with a partial row.
    let fp = jobs::crash_after_running(&db, &snap, 1).unwrap();
    let state: String = db
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT state FROM jobs WHERE fingerprint=?1",
            rusqlite::params![fp],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "running");

    // No candidates from a non-done job may surface as auditable.
    let detail = service::event_detail(&db, event).unwrap();
    assert!(detail["latest_done_job"].is_null());
    assert_eq!(detail["candidates"].as_array().unwrap().len(), 0);

    // Recovery clears half-written candidates and requeues; solving completes.
    let recovered = db.recover_pending_jobs().unwrap();
    assert_eq!(recovered, 1);
    let partial: i64 = db
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM candidates WHERE job_fp=?1",
            rusqlite::params![fp],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(partial, 0);

    let done_fp = service::solve_event(&db, event).unwrap();
    assert_eq!(done_fp, fp);
    let detail = service::event_detail(&db, event).unwrap();
    assert_eq!(detail["latest_done_job"], serde_json::json!(fp));
    assert!(!detail["candidates"].as_array().unwrap().is_empty());
}

#[test]
fn evidence_changes_new_run_without_touching_old_candidates() {
    let db = Db::open_memory().unwrap();
    synth::generate(&db);
    let event = "EVT-IMPACT";
    let fp1 = service::solve_event(&db, event).unwrap();
    let before: i64 = db
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM candidates WHERE job_fp=?1",
            rusqlite::params![fp1],
            |r| r.get(0),
        )
        .unwrap();

    // Lock a clock segment; a NEW job fingerprint is produced.
    evidence::add_evidence(
        &db,
        event,
        EvidenceReq {
            kind: "lock_clock".into(),
            payload: serde_json::json!({"station_id": "A", "offset_s": 0.0031, "sigma_s": 0.0002}),
        },
    )
    .unwrap();
    let fp2 = service::solve_event(&db, event).unwrap();
    assert_ne!(fp1, fp2);

    let after: i64 = db
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM candidates WHERE job_fp=?1",
            rusqlite::params![fp1],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, after, "old candidate set must remain intact");

    // Identical evidence is idempotent (same content id).
    let id1 = evidence::add_evidence(
        &db,
        event,
        EvidenceReq {
            kind: "lock_clock".into(),
            payload: serde_json::json!({"station_id": "A", "offset_s": 0.0031, "sigma_s": 0.0002}),
        },
    )
    .unwrap();
    let id2 = evidence::add_evidence(
        &db,
        event,
        EvidenceReq {
            kind: "lock_clock".into(),
            payload: serde_json::json!({"station_id": "A", "offset_s": 0.0031, "sigma_s": 0.0002}),
        },
    )
    .unwrap();
    assert_eq!(id1, id2);
}
