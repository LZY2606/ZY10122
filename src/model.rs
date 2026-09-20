//! 持久化与 API 共享的数据模型。
//!
//! 单位约定（在 API 与 README 中显式声明）：
//! - 坐标：米（m），二维 `(x, y)`
//! - 时间、延迟、时钟偏移：秒（s）
//! - 声速：米/秒（m/s）
//! - 频率：赫兹（Hz）
//! - 区间一律采用半开语义 `[start, end)`。

use serde::{Deserialize, Serialize};

/// 一台采集器（单麦克风或固定几何的阵列）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Station {
    pub station_id: String,
    pub x: f64,
    pub y: f64,
    pub label: String,
}

/// 一段分段常值时钟模型：本地读数 = 全局时间 + `offset_sec`（段内成立）。
///
/// 段覆盖半开区间 `[t_start, t_end)`；`t_end` 为 `None` 表示延伸到无穷远。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClockSegment {
    pub station_id: String,
    pub t_start: f64,
    pub t_end: Option<f64>,
    pub offset_sec: f64,
    pub source: String,
}

/// 一条互相关延迟（CCF 峰值）。`lag_sec > 0` 表示本台比 `peer_id` 晚到达。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LagInput {
    pub peer_id: String,
    pub lag_sec: f64,
    pub peak_band_hz: f64,
    pub snr_db: f64,
    pub hint: String,
    pub lag_uid: Option<String>,
}

/// 设备上报的一条观测。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationInput {
    pub obs_uid: String,
    pub station_id: String,
    /// 本地时钟读数（秒）。
    pub local_onset_sec: f64,
    pub peak_band_hz: f64,
    pub snr_db: f64,
    pub lags: Vec<LagInput>,
}

/// 上传批次（幂等：相同 `batch_uid` 只生效一次）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchInput {
    pub batch_uid: String,
    pub event_id: String,
    pub observations: Vec<ObservationInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventInput {
    pub event_id: String,
    pub label: String,
    /// 半开时间区间 `[window_start, window_end)`。
    pub window_start: f64,
    pub window_end: f64,
    pub sound_speed_mps: f64,
    pub site_bounds: SiteBounds,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SiteBounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

/// 落库后的观测（含本地/校正时间等派生展示字段）。
#[derive(Debug, Clone, Serialize)]
pub struct ObservationRow {
    pub obs_uid: String,
    pub batch_uid: String,
    pub event_id: String,
    pub station_id: String,
    pub local_onset_sec: f64,
    pub corrected_onset_sec: Option<f64>,
    pub clock_locked: bool,
    pub clock_segment_id: Option<i64>,
    pub peak_band_hz: f64,
    pub snr_db: f64,
    pub duplicate_of: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LagRow {
    pub lag_uid: String,
    pub obs_uid: String,
    pub station_id: String,
    pub peer_id: String,
    pub lag_sec: f64,
    pub peak_band_hz: f64,
    pub snr_db: f64,
    pub hint: String,
}

/// 派生证据（人工判断）。只追加，永不修改。
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceRow {
    pub evidence_id: String,
    pub event_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
    pub basis: String,
    pub created_at: String,
    pub superseded_by: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateResidual {
    pub kind: String,
    pub lag_uid: Option<String>,
    pub station_id: String,
    pub peer_id: Option<String>,
    pub measured_sec: f64,
    pub predicted_sec: Option<f64>,
    pub residual_sec: Option<f64>,
    pub weight: f64,
    pub used: bool,
    pub excluded_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub candidate_id: String,
    pub job_id: i64,
    pub rank: i64,
    pub kind: String,
    pub point: Option<[f64; 2]>,
    pub curve: Option<Curve>,
    pub region: Option<Region>,
    pub rms_sec: Option<f64>,
    pub underdetermined: String,
    pub score: f64,
    pub signature: String,
    pub residuals: Vec<CandidateResidual>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Curve {
    pub kind: String,
    pub focus_a: [f64; 2],
    pub focus_b: [f64; 2],
    pub difference_m: f64,
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub kind: String,
    pub center: [f64; 2],
    pub semi_axes: [f64; 2],
    pub angle_rad: f64,
    pub polygon: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobView {
    pub job_id: i64,
    pub event_id: String,
    pub status: String,
    pub fingerprint: String,
    pub error: Option<String>,
    pub created_at: String,
}
