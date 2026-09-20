//! API-level data structures (all units explicit).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct Station {
    pub station_id: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClockSegment {
    pub seg_id: i64,
    pub station_id: String,
    pub start_s: f64,
    pub end_s: Option<f64>,
    pub offset_s: f64,
    pub sigma_s: f64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Observation {
    pub obs_id: i64,
    pub batch_fp: String,
    pub station_id: String,
    pub t_device: f64,
    pub local_onset_s: f64,
    pub peak_band_hz: f64,
    pub snr_db: f64,
    pub multipath: bool,
    pub excluded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Xcorr {
    pub xc_id: i64,
    pub obs_a: i64,
    pub obs_b: i64,
    pub station_a: String,
    pub station_b: String,
    pub delay_s: f64,
    pub sigma_s: f64,
    pub peak_index: i64,
    pub multipath: bool,
    pub excluded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventHeader {
    pub event_id: String,
    pub title: String,
    pub t_start: f64,
    pub t_end: f64,
    pub sound_speed: f64,
    pub coordinate_unit: String,
    pub time_unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResidualRow {
    pub edge: String,
    pub kind: String,
    pub station_a: String,
    pub station_b: String,
    pub measured_s: f64,
    pub predicted_s: f64,
    pub residual_s: f64,
    pub sigma_s: f64,
    pub n_sigma: f64,
    pub multipath: bool,
    pub excluded: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Candidate {
    pub cand_id: i64,
    pub cand_key: String,
    pub job_fp: String,
    pub rank_in_job: i64,
    pub kind: String,
    pub status: String,
    pub reason: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub region: Option<Region>,
    pub metrics: serde_json::Value,
    pub residuals: Vec<ResidualRow>,
    pub kept: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub shape: String,
    pub coordinates: Vec<[f64; 2]>,
    pub source: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateStationReq {
    pub station_id: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Deserialize)]
pub struct ClockSegmentReq {
    pub station_id: String,
    pub start_s: f64,
    pub end_s: Option<f64>,
    pub offset_s: f64,
    #[serde(default = "default_sigma")]
    pub sigma_s: f64,
    #[serde(default)]
    pub note: String,
}

fn default_sigma() -> f64 {
    0.002
}

#[derive(Debug, Deserialize)]
pub struct CreateEventReq {
    pub event_id: String,
    pub title: String,
    pub t_start: f64,
    pub t_end: f64,
    #[serde(default = "default_speed")]
    pub sound_speed: f64,
}

fn default_speed() -> f64 {
    343.0
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct IngestReq {
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub received_at: Option<f64>,
    pub observations: Vec<IngestObs>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestObs {
    pub station_id: String,
    pub event_ref: Option<String>,
    pub t_device: f64,
    pub local_onset_s: f64,
    pub peak_band_hz: f64,
    pub snr_db: f64,
    #[serde(default)]
    pub xcorr: Vec<IngestXcorr>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestXcorr {
    pub obs_index: usize,
    pub delay_s: f64,
    #[serde(default = "default_sigma")]
    pub sigma_s: f64,
    #[serde(default)]
    pub peak_index: i64,
    #[serde(default)]
    pub multipath: bool,
}

#[derive(Debug, Deserialize)]
pub struct EvidenceReq {
    pub kind: String,
    pub payload: serde_json::Value,
}
