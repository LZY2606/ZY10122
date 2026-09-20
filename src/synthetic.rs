//! 合成数据生成器：构造可复现的演示站点、时钟分段、事件与观测。
//!
//! 单位：坐标 m、时间 s、声速 m/s。所有数值由固定公式计算，无随机数。

use crate::model::*;

pub const SOUND_SPEED: f64 = 343.0;

fn dist(ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    (ax - bx).hypot(ay - by)
}

pub struct SeedData {
    pub stations: Vec<Station>,
    pub events: Vec<EventInput>,
    pub clocks: Vec<ClockSegment>,
    pub batches: Vec<BatchInput>,
}

struct Ctx {
    stations: Vec<Station>,
    clocks: Vec<ClockSegment>,
    events: Vec<EventInput>,
    batches: Vec<BatchInput>,
}

pub fn build() -> SeedData {
    let mut ctx = Ctx {
        stations: vec![
            Station { station_id: "S1".into(), x: 0.0, y: 0.0, label: "阵列-西北".into() },
            Station { station_id: "S2".into(), x: 40.0, y: 0.0, label: "阵列-东北".into() },
            Station { station_id: "S3".into(), x: 0.0, y: 30.0, label: "阵列-西南".into() },
            Station { station_id: "S4".into(), x: 40.0, y: 30.0, label: "阵列-东南".into() },
        ],
        clocks: Vec::new(),
        events: Vec::new(),
        batches: Vec::new(),
    };
    seed_event_a(&mut ctx);
    seed_event_b(&mut ctx);
    seed_event_c(&mut ctx);
    SeedData {
        stations: ctx.stations,
        events: ctx.events,
        clocks: ctx.clocks,
        batches: ctx.batches,
    }
}

fn bounds() -> SiteBounds {
    SiteBounds { min_x: -10.0, min_y: -10.0, max_x: 50.0, max_y: 40.0 }
}

fn pos(ctx: &Ctx, id: &str) -> (f64, f64) {
    let s = ctx.stations.iter().find(|s| s.station_id == id).unwrap();
    (s.x, s.y)
}

/// 在每台站上构造“直达 + 可选反射”CCF 延迟（相对 ref_station）。
fn lag(ctx: &Ctx, station: &str, ref_station: &str, sx: f64, sy: f64,
       refl_extra_m: f64, idx: usize, batch: &str, obs_idx: usize) -> LagInput {
    let (ax, ay) = pos(ctx, station);
    let (bx, by) = pos(ctx, ref_station);
    let d_station = dist(ax, ay, sx, sy) + refl_extra_m;
    let d_ref = dist(bx, by, sx, sy);
    let lag_sec = (d_station - d_ref) / SOUND_SPEED;
    LagInput {
        peer_id: ref_station.into(),
        lag_sec,
        peak_band_hz: 1800.0,
        snr_db: if refl_extra_m > 0.0 { 9.0 } else { 16.0 },
        hint: if refl_extra_m > 0.0 { "reflection".into() } else { "direct".into() },
        lag_uid: Some(format!("lag:{}:{}:{}", batch, obs_idx, idx)),
    }
}

// ---- 事件 A：四阵列 + 多径 + 分段时钟（完整可定位）----
fn seed_event_a(ctx: &mut Ctx) {
    let (sx, sy) = (24.0, 12.0);
    let t0 = 1000.0;
    // 本地时钟偏移（秒）：S1 快 0.02、S2 慢 0.01、S3 无偏移、S4 快 0.005。
    // 每台两段，恰在事件窗内相接，演示半开边界。
    let offsets = [("S1", 0.020), ("S2", -0.010), ("S3", 0.000), ("S4", 0.005)];
    for (sid, off) in offsets {
        ctx.clocks.push(ClockSegment {
            station_id: sid.into(), t_start: 0.0, t_end: Some(t0 + 0.3),
            offset_sec: off, source: "synthetic".into(),
        });
        ctx.clocks.push(ClockSegment {
            station_id: sid.into(), t_start: t0 + 0.3, t_end: Some(1500.0),
            offset_sec: off + 0.05, source: "synthetic".into(),
        });
    }
    ctx.events.push(EventInput {
        event_id: "EV-A".into(),
        label: "压缩机异响（含墙面反射）".into(),
        window_start: t0,
        window_end: t0 + 0.6,
        sound_speed_mps: SOUND_SPEED,
        site_bounds: bounds(),
    });
    let mut obs = Vec::new();
    for (i, (sid, off)) in offsets.iter().enumerate() {
        let (x, y) = pos(ctx, sid);
        let toa = dist(x, y, sx, sy) / SOUND_SPEED;
        // S1 多上报一次（重复观测），S4 缺席。
        let mut lags = Vec::new();
        if *sid != "S4" {
            lags.push(lag(ctx, sid, "S2", sx, sy, 0.0, 0, "B-A", i));
        }
        if *sid == "S1" {
            // S1 的第二个 CCF 峰对应墙面反射（路程多 7.5 m）。
            lags.push(lag(ctx, sid, "S2", sx, sy, 7.5, 1, "B-A", i));
        }
        obs.push(ObservationInput {
            obs_uid: format!("obs:B-A:{}", sid),
            station_id: sid.to_string(),
            local_onset_sec: t0 + toa + off,
            peak_band_hz: 1800.0,
            snr_db: 15.0,
            lags,
        });
    }
    // 重复上报：相同 obs_uid（设备重传）。
    let _s1dup = obs[0].clone();
    let _s1dup = obs[0].clone();
    obs.push(_s1dup);
    ctx.batches.push(BatchInput {
        batch_uid: "B-A".into(), event_id: "EV-A".into(), observations: obs,
    });
}

// ---- 事件 B：只有两台站有观测，时钟段存在但未锁定 → 欠定（时钟自由度）----
fn seed_event_b(ctx: &mut Ctx) {
    let (sx, sy) = (14.0, 22.0);
    let t0 = 2000.0;
    let offsets = [("S1", 0.012), ("S3", -0.004)];
    for (sid, off) in offsets {
        ctx.clocks.push(ClockSegment {
            // 紧接 EV-A 的第二段（从 1000.3 起）开始，避免跨事件重叠。
            station_id: sid.into(), t_start: 1999.0, t_end: Some(t0 + 0.3),
            offset_sec: off, source: "synthetic".into(),
        });
        ctx.clocks.push(ClockSegment {
            station_id: sid.into(), t_start: t0 + 0.3, t_end: Some(2500.0),
            offset_sec: off, source: "synthetic".into(),
        });
    }
    ctx.events.push(EventInput {
        event_id: "EV-B".into(),
        label: "管道泄漏（仅两台上报）".into(),
        window_start: t0,
        window_end: t0 + 0.6,
        sound_speed_mps: SOUND_SPEED,
        site_bounds: bounds(),
    });
    let mut obs = Vec::new();
    for (i, (sid, off)) in offsets.iter().enumerate() {
        let (x, y) = pos(ctx, sid);
        let toa = dist(x, y, sx, sy) / SOUND_SPEED;
        // 无 CCF 延迟：只有本地时钙。两台站给一条基线 TDOA（未锁时钟时不可用）。
        obs.push(ObservationInput {
            obs_uid: format!("obs:B-B:{}", sid),
            station_id: sid.to_string(),
            local_onset_sec: t0 + toa + off,
            peak_band_hz: 900.0,
            snr_db: 6.0,
            lags: vec![],
        });
        let _ = i;
    }
    ctx.batches.push(BatchInput {
        batch_uid: "B-B".into(), event_id: "EV-B".into(), observations: obs,
    });
}

// ---- 事件 C：互相关延迟超过基线几何极限 → 超光速，不可满足 ----
fn seed_event_c(ctx: &mut Ctx) {
    let t0 = 3000.0;
    for sid in ["S1", "S2", "S3", "S4"] {
        ctx.clocks.push(ClockSegment {
            station_id: sid.into(), t_start: t0 - 1.0, t_end: None,
            offset_sec: 0.0, source: "synthetic".into(),
        });
    }
    ctx.events.push(EventInput {
        event_id: "EV-C".into(),
        label: "异常特征包（超光速延迟）".into(),
        window_start: t0,
        window_end: t0 + 0.6,
        sound_speed_mps: SOUND_SPEED,
        site_bounds: bounds(),
    });
    let mut obs = Vec::new();
    let stations = ["S1", "S2", "S3", "S4"];
    for (i, sid) in stations.iter().enumerate() {
        let (x, y) = pos(ctx, sid);
        obs.push(ObservationInput {
            obs_uid: format!("obs:B-C:{}", sid),
            station_id: sid.to_string(),
            local_onset_sec: t0 + dist(x, y, 10.0, 10.0) / SOUND_SPEED,
            peak_band_hz: 2400.0,
            snr_db: 12.0,
            lags: if *sid == "S1" {
                // S1-S2 基线 40 m，物理上 |tau|*c <= 40 m；给 60 m 等效延迟。
                vec![LagInput {
                    peer_id: "S2".into(),
                    lag_sec: 60.0 / SOUND_SPEED,
                    peak_band_hz: 2400.0,
                    snr_db: 4.0,
                    hint: "direct".into(),
                    lag_uid: Some(format!("lag:B-C:{}:0", i)),
                }]
            } else {
                vec![]
            },
        });
    }
    ctx.batches.push(BatchInput {
        batch_uid: "B-C".into(), event_id: "EV-C".into(), observations: obs,
    });
}
