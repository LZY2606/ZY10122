//! Fixed inputs: parallel solves must not reorder candidates or numbering.

use std::collections::HashSet;
use std::sync::Arc;
use std::thread;

use acoustic_review::*;

#[test]
fn parallel_runs_keep_identical_ranking() {
    let db = Arc::new(Db::open_memory().unwrap());
    synth::generate(&db);
    let event = "EVT-IMPACT";

    let mut handles = Vec::new();
    for _ in 0..6 {
        let db = Arc::clone(&db);
        let event = event.to_string();
        handles.push(thread::spawn(move || {
            let snap = jobs::build_snapshot(&db, &event, units::PhysicalModel::default()).unwrap();
            let cands = solver::solve(&snap);
            cands
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{}:{kind}:{key}", i, kind = c.kind, key = c.cand_key))
                .collect::<Vec<_>>()
        }));
    }
    let mut rankings: Vec<Vec<String>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let first = rankings.pop().unwrap();
    for r in &rankings {
        assert_eq!(r, &first);
    }

    // Evidence content ids are also deterministic for the same decision.
    let mut ids = HashSet::new();
    for _ in 0..4 {
        let id = evidence::add_evidence(
            &db,
            event,
            models::EvidenceReq {
                kind: "exclude_peak".into(),
                payload: serde_json::json!({"station_a": "A", "station_b": "D", "peak_index": 1}),
            },
        )
        .unwrap();
        ids.insert(id);
    }
    assert_eq!(ids.len(), 1);
}
