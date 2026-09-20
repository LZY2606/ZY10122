//! 库入口：供集成测试与二进制复用。

pub mod db;
pub mod geom;
pub mod model;
pub mod server;
pub mod solver;
pub mod synthetic;

pub use model::*;

use serde::{Deserialize, Serialize};

/// 测试夹具：一个半开边界相接的时钟分段案例（S1，边界 t=100.0）。
#[must_use]
pub fn clock_boundary_case() -> Vec<(ClockSegment, i64)> {
    vec![
        (ClockSegment {
            station_id: "S1".into(),
            t_start: 0.0,
            t_end: Some(100.0),
            offset_sec: 0.010,
            source: "fixture".into(),
        }, 1),
        (ClockSegment {
            station_id: "S1".into(),
            t_start: 100.0,
            t_end: None,
            offset_sec: 0.020,
            source: "fixture".into(),
        }, 2),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedDataLike {
    pub stations: Vec<Station>,
    pub events: Vec<EventInput>,
    pub clocks: Vec<ClockSegment>,
    pub batches: Vec<BatchInput>,
}

/// 三台站最小可定位案例（源 (14,10)），时钟段存在但默认无锁定证据。
#[must_use]
pub fn synthetic_three_station_case() -> (Vec<ClockSegment>, SeedDataLike) {
    let c = 343.0_f64;
    let stations = vec![
        Station { station_id: "S1".into(), x: 0.0, y: 0.0, label: "a".into() },
        Station { station_id: "S2".into(), x: 30.0, y: 0.0, label: "b".into() },
        Station { station_id: "S3".into(), x: 15.0, y: 25.0, label: "c".into() },
    ];
    let pos = |id: &str| -> (f64, f64) {
        let s = stations.iter().find(|s| s.station_id == id).unwrap();
        (s.x, s.y)
    };
    let d = |id: &str| -> f64 {
        let (x, y) = pos(id);
        (x - 14.0).hypot(y - 10.0)
    };
    let offsets = [("S1", 0.011), ("S2", -0.006), ("S3", 0.002)];
    let t0 = 500.0;
    // 两段在 t0+0.2 处半开相接（覆盖观测时刻 t0+0.0x）。
    let clocks: Vec<ClockSegment> = offsets
        .iter()
        .flat_map(|(sid, off)| {
            vec![
                ClockSegment {
                    station_id: (*sid).into(),
                    t_start: 0.0,
                    t_end: Some(t0 + 0.2),
                    offset_sec: *off,
                    source: "fixture".into(),
                },
                ClockSegment {
                    station_id: (*sid).into(),
                    t_start: t0 + 0.2,
                    t_end: None,
                    offset_sec: *off,
                    source: "fixture".into(),
                },
            ]
        })
        .collect();
    let obs: Vec<ObservationInput> = offsets
        .iter()
        .map(|(sid, off)| ObservationInput {
            obs_uid: format!("obs:{sid}"),
            station_id: (*sid).into(),
            local_onset_sec: t0 + d(sid) / c + off,
            peak_band_hz: 1200.0,
            snr_db: 14.0,
            lags: vec![],
        })
        .collect();
    let event = EventInput {
        event_id: "EV".into(),
        label: "fixture".into(),
        window_start: t0,
        window_end: t0 + 0.6,
        sound_speed_mps: c,
        site_bounds: SiteBounds { min_x: -5.0, min_y: -5.0, max_x: 35.0, max_y: 30.0 },
    };
    let batch = BatchInput {
        batch_uid: "B".into(),
        event_id: "EV".into(),
        observations: obs,
    };
    let data = SeedDataLike {
        stations,
        events: vec![event],
        clocks: clocks.clone(),
        batches: vec![batch],
    };
    (clocks, data)
}

/// 最小幂等批次夹具（单台站单观测）。
#[must_use]
pub fn synthetic_build_minimal() -> SeedDataLike {
    let stations = vec![Station {
        station_id: "S1".into(),
        x: 0.0,
        y: 0.0,
        label: "a".into(),
    }];
    let clocks = vec![ClockSegment {
        station_id: "S1".into(),
        t_start: 0.0,
        t_end: None,
        offset_sec: 0.0,
        source: "fixture".into(),
    }];
    let event = EventInput {
        event_id: "EV".into(),
        label: "min".into(),
        window_start: 0.0,
        window_end: 10.0,
        sound_speed_mps: 343.0,
        site_bounds: SiteBounds { min_x: -5.0, min_y: -5.0, max_x: 35.0, max_y: 30.0 },
    };
    let batch = BatchInput {
        batch_uid: "BM".into(),
        event_id: "EV".into(),
        observations: vec![ObservationInput {
            obs_uid: "obs:min".into(),
            station_id: "S1".into(),
            local_onset_sec: 1.0,
            peak_band_hz: 1000.0,
            snr_db: 10.0,
            lags: vec![],
        }],
    };
    SeedDataLike {
        stations,
        events: vec![event],
        clocks,
        batches: vec![batch],
    }
}
