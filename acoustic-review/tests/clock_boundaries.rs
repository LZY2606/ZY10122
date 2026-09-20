//! Half-open clock segment semantics.

use acoustic_review::*;
use models::{ClockSegmentReq, CreateStationReq};

fn db_with_station() -> Db {
    let db = Db::open_memory().unwrap();
    service::create_station(
        &db,
        CreateStationReq {
            station_id: "A".into(),
            name: "A".into(),
            x: 0.0,
            y: 0.0,
        },
    )
    .unwrap();
    db
}

#[test]
fn touching_segments_are_allowed_and_select_the_later_one_at_join() {
    let db = db_with_station();
    clock::add_segment(
        &db,
        &ClockSegmentReq {
            station_id: "A".into(),
            start_s: 0.0,
            end_s: Some(60.0),
            offset_s: 0.010,
            sigma_s: 0.001,
            note: "first".into(),
        },
    )
    .unwrap();
    clock::add_segment(
        &db,
        &ClockSegmentReq {
            station_id: "A".into(),
            start_s: 60.0,
            end_s: None,
            offset_s: -0.020,
            sigma_s: 0.001,
            note: "second".into(),
        },
    )
    .unwrap();

    let segs = clock::list_segments(&db, "A").unwrap();

    let just_before = clock::segment_at(&segs, 59.9999).unwrap();
    assert_eq!(just_before.offset_s, 0.010);

    // At the join, [0,60) has ENDED: only [60, inf) is in effect.
    let at_join = clock::segment_at(&segs, 60.0).unwrap();
    assert_eq!(at_join.offset_s, -0.020);
    assert_ne!(just_before.seg_id, at_join.seg_id);
}

#[test]
fn genuinely_overlapping_segments_are_rejected() {
    let db = db_with_station();
    clock::add_segment(
        &db,
        &ClockSegmentReq {
            station_id: "A".into(),
            start_s: 0.0,
            end_s: Some(60.0),
            offset_s: 0.0,
            sigma_s: 0.001,
            note: String::new(),
        },
    )
    .unwrap();
    // [59.999, ...) overlaps [0,60) on [59.999,60)
    let err = clock::add_segment(
        &db,
        &ClockSegmentReq {
            station_id: "A".into(),
            start_s: 59.999,
            end_s: Some(120.0),
            offset_s: 0.0,
            sigma_s: 0.001,
            note: String::new(),
        },
    );
    assert!(err.is_err(), "overlap must be rejected: {err:?}");
}

#[test]
fn no_segment_means_free_clock() {
    let db = db_with_station();
    let segs = clock::list_segments(&db, "A").unwrap();
    let eff = clock::effective_offset(&segs, None, 40.0);
    assert!(eff.unknown);
    assert!(eff.sigma_s >= clock::FREE_CLOCK_SIGMA);

    let locked = clock::effective_offset(&segs, Some((0.005, 0.0)), 40.0);
    assert!(locked.locked);
    assert_eq!(locked.offset_s, 0.005);
}
