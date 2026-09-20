//! Underdetermined geometry: hyperbola from one edge and explicit reason.

use acoustic_review::solver::*;
use std::collections::{BTreeMap, BTreeSet};

fn edge(a: &str, b: &str, d: f64) -> EdgeObs {
    EdgeObs {
        station_a: a.into(),
        station_b: b.into(),
        delay_s: d,
        sigma_s: 0.001,
        multipath: false,
        xc_id: None,
        peak_index: 0,
        origin: "x".into(),
    }
}

fn two_station_snapshot() -> Snapshot {
    Snapshot {
        event_id: "G".into(),
        t_mid: 40.0,
        sound_speed: 343.0,
        observable_radius: 500.0,
        stations: vec![
            StationInfo {
                station_id: "A".into(),
                x: 0.0,
                y: 0.0,
            },
            StationInfo {
                station_id: "B".into(),
                x: 100.0,
                y: 0.0,
            },
        ],
        segments: {
            let mut m = BTreeMap::new();
            for sid in ["A", "B"] {
                m.insert(
                    sid.to_string(),
                    vec![acoustic_review::models::ClockSegment {
                        seg_id: 1,
                        station_id: sid.into(),
                        start_s: 0.0,
                        end_s: None,
                        offset_s: 0.0,
                        sigma_s: 0.001,
                        note: String::new(),
                    }],
                );
            }
            m
        },
        edges: vec![edge("A", "B", -0.05)],
        exclude_edges: BTreeSet::new(),
        locks: BTreeMap::new(),
        kept_pairs: vec![],
    }
}

#[test]
fn single_edge_yields_hyperbola_region() {
    let s = two_station_snapshot();
    let out = solve(&s);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].kind, "region");
    assert_eq!(out[0].status, "underdetermined");
    assert_eq!(out[0].reason, "sensor_geometry");
    let region = out[0].region.as_ref().unwrap();
    assert_eq!(region["shape"], "hyperbola");
    assert!(region["coordinates"].as_array().unwrap().len() > 10);
}

#[test]
fn impossible_delay_is_not_clamped_into_domain() {
    let mut s = two_station_snapshot();
    // |d| * c = 120 m > baseline 100 m: no hyperbola exists.
    s.edges[0].delay_s = 0.35;
    let out = solve(&s);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].kind, "region");
    let region = out[0].region.as_ref().unwrap();
    assert_eq!(region["shape"], "circle");
    assert!(region["source"]
        .as_str()
        .unwrap()
        .contains("no hyperbolic locus"));
}

#[test]
fn candidate_keys_and_ranks_are_stable_across_runs() {
    let s = two_station_snapshot();
    let a = solve(&s);
    let b = solve(&s);
    assert_eq!(a.len(), b.len());
    for (x, y) in a.iter().zip(b.iter()) {
        assert_eq!(x.cand_key, y.cand_key);
        assert_eq!(x.reason, y.reason);
    }
}

#[test]
fn contaminated_direct_peak_is_attributed_to_multipath() {
    // Source at (90, 20); stations with one edge A-D contaminated by a
    // reflected arrival so the all-direct set is inconsistent.
    let stations = vec![
        StationInfo {
            station_id: "A".into(),
            x: 0.0,
            y: 40.0,
        },
        StationInfo {
            station_id: "B".into(),
            x: 120.0,
            y: 40.0,
        },
        StationInfo {
            station_id: "D".into(),
            x: 120.0,
            y: -40.0,
        },
        StationInfo {
            station_id: "E".into(),
            x: 60.0,
            y: 0.0,
        },
    ];
    let c = 343.0f64;
    let dist = |sx: f64, sy: f64| ((90.0 - sx).powi(2) + (20.0 - sy).powi(2)).sqrt() / c;
    let d = |sx1: f64, sy1: f64, sx2: f64, sy2: f64| dist(sx2, sy2) - dist(sx1, sy1);
    let mut segments = BTreeMap::new();
    for sid in ["A", "B", "D", "E"] {
        segments.insert(
            sid.to_string(),
            vec![acoustic_review::models::ClockSegment {
                seg_id: 1,
                station_id: sid.into(),
                start_s: 0.0,
                end_s: None,
                offset_s: 0.0,
                sigma_s: 0.001,
                note: String::new(),
            }],
        );
    }
    let s = Snapshot {
        event_id: "MP".into(),
        t_mid: 40.0,
        sound_speed: c,
        observable_radius: 2000.0,
        stations,
        segments,
        edges: vec![
            edge("A", "B", d(0.0, 40.0, 120.0, 40.0)),
            EdgeObs {
                station_a: "A".into(),
                station_b: "D".into(),
                delay_s: d(0.0, 40.0, 120.0, -40.0) + 0.015, // reflected contamination
                sigma_s: 0.001,
                multipath: true,
                xc_id: None,
                peak_index: 0,
                origin: "x".into(),
            },
            edge("A", "E", d(0.0, 40.0, 60.0, 0.0)),
            edge("B", "D", d(120.0, 40.0, 120.0, -40.0)),
            edge("B", "E", d(120.0, 40.0, 60.0, 0.0)),
            edge("D", "E", d(120.0, -40.0, 60.0, 0.0)),
        ],
        exclude_edges: BTreeSet::new(),
        locks: BTreeMap::new(),
        kept_pairs: vec![],
    };

    let out = solve(&s);
    let near_truth: Vec<_> = out
        .iter()
        .filter(|c| {
            c.kind == "point"
                && c.x.is_some()
                && ((c.x.unwrap() - 90.0).powi(2) + (c.y.unwrap() - 20.0).powi(2)).sqrt() < 5.0
        })
        .collect();
    assert!(
        !near_truth.is_empty(),
        "expected a near-truth candidate: {out:?}"
    );
    assert!(
        near_truth
            .iter()
            .any(|c| c.status == "underdetermined" && c.reason.starts_with("multipath")),
        "near-truth candidate must be attributed to multipath ambiguity: {near_truth:?}"
    );
}
