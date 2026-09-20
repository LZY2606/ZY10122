//! TDOA 定位求解器。
//!
//! 设计要点（与 README 对应）：
//! - 两类测量：CCF 站内互相关延迟（时钟无关）与到达时刻差（依赖锁定的时钟段）。
//! - 枚举“直达/反射”证据组合（假设），每个假设独立求解；并行不改变排名与编号。
//! - 优化器无场地边界约束：超出可观测几何的输入得到域外解并显式标记，绝不硬拉回边界。
//! - 不足定位时返回点 / 双曲线 / 区域三种形状，并注明原因类别。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::canonical_fingerprint;
use crate::geom::{
    self, grid_seeds, hyperbola, uncertainty_region, whole_site_region, Pt, Tdoa,
};
use crate::model::*;

pub const SOLVER_VERSION: &str = "solver-1.0.0";
pub const MAX_HYPOTHESES: usize = 24;
pub const INCONSISTENT_RMS_SEC: f64 = 0.004;
pub const WEAK_LAMBDA_MIN: f64 = 1e-8;

#[derive(Debug, Clone)]
pub struct SolverEvent {
    pub event_id: String,
    pub window_start: f64,
    pub window_end: f64,
    pub sound_speed_mps: f64,
    pub bounds: SiteBounds,
}

#[derive(Debug, Clone)]
pub struct SolverInput {
    pub event: SolverEvent,
    pub stations: Vec<Station>,
    pub clock_segments: Vec<(ClockSegment, i64)>,
    pub observations: Vec<ObservationRow>,
    pub lags: Vec<LagRow>,
    pub evidence: Vec<EvidenceRow>,
}

#[derive(Debug, Clone)]
struct DerivedMeas {
    tdoa: Tdoa,
    kind: String, // "ccf_lag" | "onset_pair"
    lag_uid: Option<String>,
    station_id: String,
    peer_id: Option<String>,
    measured_sec: f64,
    reflection: bool,
    clock_locked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SolveOutput {
    pub fingerprint: String,
    pub candidates: Vec<Candidate>,
}

/// 作业输入指纹：固定输入 + SOLVER_VERSION 下结果完全确定。
/// 任何新观测或新证据都会改变指纹，从而触发只消耗新证据的重算。
pub fn input_fingerprint(input: &SolverInput) -> String {
    let e = &input.event;
    let mut segs: Vec<&(ClockSegment, i64)> = input.clock_segments.iter().collect();
    segs.sort_by_key(|(_, id)| *id);
    let active: Vec<serde_json::Value> = input
        .evidence
        .iter()
        .map(|v| serde_json::json!({"id":v.evidence_id,"kind":v.kind,"payload":v.payload}))
        .collect();
    let fp_json = serde_json::json!({
        "solver_version": SOLVER_VERSION,
        "event_id": e.event_id,
        "sound_speed_mps": e.sound_speed_mps,
        "bounds": [e.bounds.min_x, e.bounds.min_y, e.bounds.max_x, e.bounds.max_y],
        "stations": input.stations,
        "clock_segments": segs.iter().map(|(s, id)|
            serde_json::json!({"id":id,"station":s.station_id,"start":s.t_start,
                               "end":s.t_end,"offset":s.offset_sec})).collect::<Vec<_>>(),
        "observations": input.observations.iter().map(|o|
            serde_json::json!({"uid":o.obs_uid,"station":o.station_id,
                "local":o.local_onset_sec,"corrected":o.corrected_onset_sec,
                "duplicate_of":o.duplicate_of})).collect::<Vec<_>>(),
        "lags": input.lags.iter().map(|l|
            serde_json::json!({"uid":l.lag_uid,"station":l.station_id,"peer":l.peer_id,
                "lag":l.lag_sec,"hint":l.hint,"snr":l.snr_db})).collect::<Vec<_>>(),
        "evidence": active,
    });
    canonical_fingerprint(&fp_json)
}

fn sha8(s: &str) -> String {
    hex::encode(&Sha256::digest(s.as_bytes())[..4])
}

/// 当前生效的手工证据主体集合（同主体以最新证据为准）。
fn active_subjects(evidence: &[EvidenceRow]) -> (BTreeSet<String>, BTreeSet<i64>, Vec<(String, String)>) {
    let mut excluded_lags: BTreeMap<String, String> = BTreeMap::new();
    let mut locked_segments: BTreeMap<String, i64> = BTreeMap::new();
    let mut ties: Vec<(String, String)> = Vec::new();
    for ev in evidence {
        match ev.kind.as_str() {
            "exclude_reflection" => {
                if let Some(uid) = ev.payload.get("lag_uid").and_then(|v| v.as_str()) {
                    excluded_lags.insert(uid.to_string(), ev.evidence_id.clone());
                }
            }
            "lock_clock" => {
                if let Some(id) = ev.payload.get("segment_id").and_then(|v| v.as_i64()) {
                    locked_segments.insert(id.to_string(), id);
                }
            }
            "keep_ambiguous" => {
                if let Some(arr) = ev.payload.get("candidate_ids").and_then(|v| v.as_array()) {
                    let ids: Vec<String> = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                    if ids.len() == 2 {
                        ties.push((ids[0].clone(), ids[1].clone()));
                    }
                }
            }
            _ => {}
        }
    }
    (
        excluded_lags.keys().cloned().collect(),
        locked_segments.values().copied().collect(),
        ties,
    )
}

/// 从观测与延迟推导候选 TDOA 测量。
fn derive_measurements(input: &SolverInput) -> Vec<DerivedMeas> {
    let e = &input.event;
    let positions: BTreeMap<String, Pt> = input
        .stations
        .iter()
        .map(|s| (s.station_id.clone(), Pt(s.x, s.y)))
        .collect();
    let (_excluded_lags, locked_segments, _) = active_subjects(&input.evidence);
    let mut out = Vec::new();

    // 1) CCF 互相关延迟：本地计算，不依赖时钟段。
    for lag in &input.lags {
        let (Some(p), Some(q)) = (positions.get(&lag.station_id), positions.get(&lag.peer_id))
        else {
            continue;
        };
        // lag > 0 表示本台晚到：d(station)-d(peer) = c*lag。
        out.push(DerivedMeas {
            tdoa: Tdoa { a: *p, b: *q, tau: lag.lag_sec, c: e.sound_speed_mps, weight: 1.0,
                         tag: format!("ccf:{}", lag.lag_uid) },
            kind: "ccf_lag".into(),
            lag_uid: Some(lag.lag_uid.clone()),
            station_id: lag.station_id.clone(),
            peer_id: Some(lag.peer_id.clone()),
            measured_sec: lag.lag_sec,
            reflection: lag.hint == "reflection",
            clock_locked: true,
        });
    }

    // 2) 到达时刻差（相对最早 station_id 的锚台）。要求双方时钟段均被锁定。
    let mut by_station: BTreeMap<String, &ObservationRow> = BTreeMap::new();
    for o in &input.observations {
        if o.duplicate_of.is_some() {
            continue; // 重复上报不参与
        }
        if o.local_onset_sec < e.window_start || o.local_onset_sec >= e.window_end {
            continue;
        }
        by_station
            .entry(o.station_id.clone())
            .and_modify(|cur| {
                if o.obs_uid < cur.obs_uid {
                    *cur = o;
                }
            })
            .or_insert(o);
    }
    let anchor = by_station.keys().next().cloned();
    if let Some(anchor_id) = anchor {
        let anchor_obs = by_station[&anchor_id];
        let anchor_seg = anchor_obs.clock_segment_id;
        let anchor_ok = anchor_seg.map_or(false, |id| locked_segments.contains(&id));
        for (sid, obs) in &by_station {
            if sid == &anchor_id {
                continue;
            }
            let seg_ok = obs.clock_segment_id.map_or(false, |id| locked_segments.contains(&id));
            let (Some(t_other), Some(t_anchor)) = (obs.corrected_onset_sec, anchor_obs.corrected_onset_sec)
            else {
                continue;
            };
            let (Some(p), Some(q)) = (positions.get(sid), positions.get(&anchor_id)) else {
                continue;
            };
            let locked = anchor_ok && seg_ok;
            let tau = t_other - t_anchor;
            out.push(DerivedMeas {
                tdoa: Tdoa { a: *p, b: *q, tau, c: e.sound_speed_mps, weight: 1.0,
                             tag: format!("onset:{}:{}", sid, anchor_id) },
                kind: "onset_pair".into(),
                lag_uid: None,
                station_id: sid.clone(),
                peer_id: Some(anchor_id.clone()),
                measured_sec: tau,
                reflection: false,
                clock_locked: locked,
            });
        }
    }
    out
}

#[derive(Clone)]
struct Hypothesis {
    index: usize,
    excluded: BTreeSet<String>,
    signature: String,
}

fn enumerate_hypotheses(reflection_lags: &[String], manual_excluded: &BTreeSet<String>) -> Vec<Hypothesis> {
    // 基线（直达）假设：排除全部已被工程师手工标记的反射峰，
    // 其余反射峰再按字典序枚举“暂时保留”的组合。
    let candidates: Vec<String> = reflection_lags.to_vec();
    let mut hyps: Vec<Hypothesis> = Vec::new();
    let push = |hyps: &mut Vec<Hypothesis>, excluded: BTreeSet<String>| {
        if hyps.len() >= MAX_HYPOTHESES {
            return;
        }
        let sig = if excluded.is_empty() {
            "direct".into()
        } else {
            let mut v: Vec<&String> = excluded.iter().collect();
            v.sort();
            format!("without:{}", v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(","))
        };
        let index = hyps.len();
        hyps.push(Hypothesis { index, excluded, signature: sig });
    };
    // 顺序固定：手工排除集合优先（基线），然后逐个少排除一个反射峰……
    let all: BTreeSet<String> = candidates.iter().cloned().collect();
    let base: BTreeSet<String> = all.intersection(manual_excluded).cloned().collect();
    let optional: Vec<&String> = all.difference(manual_excluded).collect();
    push(&mut hyps, base.clone());
    for mask in 0u64..(1u64 << optional.len().min(20)) {
        if hyps.len() >= MAX_HYPOTHESES {
            break;
        }
        let mut set = base.clone();
        optional.iter().enumerate().for_each(|(i, u)| {
            if mask & (1 << i) == 0 {
                set.insert((*u).clone());
            }
        });
        push(&mut hyps, set);
    }
    hyps
}

struct HypResult {
    hyp_index: usize,
    signature: String,
    candidate: Candidate,
}

fn residuals_for(
    meas: &[DerivedMeas],
    point: Option<Pt>,
    used_flags: &[bool],
    reason: &[Option<String>],
) -> Vec<CandidateResidual> {
    meas.iter()
        .enumerate()
        .map(|(i, m)| {
            let predicted = point.map(|p| {
                (geom::dist(p, m.tdoa.a) - geom::dist(p, m.tdoa.b)) / m.tdoa.c
            });
            CandidateResidual {
                kind: m.kind.clone(),
                lag_uid: m.lag_uid.clone(),
                station_id: m.station_id.clone(),
                peer_id: m.peer_id.clone(),
                measured_sec: m.measured_sec,
                predicted_sec: predicted,
                residual_sec: predicted.map(|p| m.measured_sec - p),
                weight: m.tdoa.weight,
                used: used_flags[i],
                excluded_reason: reason[i].clone(),
            }
        })
        .collect()
}

fn solve_hypothesis(
    input: Arc<SolverInput>,
    meas: Arc<Vec<DerivedMeas>>,
    hyp: Hypothesis,
) -> HypResult {
    let e = &input.event;
    let n = meas.len();
    let mut used = vec![false; n];
    let mut reasons: Vec<Option<String>> = vec![None; n];
    let mut active_idx: Vec<usize> = Vec::new();

    for (i, m) in meas.iter().enumerate() {
        if let Some(uid) = &m.lag_uid {
            if hyp.excluded.contains(uid) {
                reasons[i] = Some("manual_reflection_excluded".into());
                continue;
            }
        }
        if m.kind == "onset_pair" && !m.clock_locked {
            reasons[i] = Some("clock_segment_unlocked".into());
            continue;
        }
        // 超光速延迟：几何上不可满足，不参与优化（避免被数值器硬拉）。
        let baseline = geom::dist(m.tdoa.a, m.tdoa.b);
        if (m.tdoa.tau.abs() * m.tdoa.c) > baseline + 1e-9 {
            reasons[i] = Some("superluminal_lag".into());
            continue;
        }
        used[i] = true;
        active_idx.push(i);
    }

    let active: Vec<Tdoa> = active_idx.iter().map(|&i| meas[i].tdoa.clone()).collect();
    // job_id 在求解入口收尾时再拼接（见 solve()），保证跨重算不撞主键；
    // 同一固定输入内候选编号仍由事件 + 枚举序 + 假设指纹稳定决定。
    let id_base = format!(
        "cand-{}:jobTBD:{:02}:{}",
        e.event_id,
        hyp.index + 1,
        sha8(&hyp.signature)
    );

    let build_residuals = |point: Option<Pt>| residuals_for(&meas, point, &used, &reasons);

    let candidate = match active.len() {
        0 => Candidate {
            candidate_id: id_base,
            job_id: 0,
            rank: 0,
            kind: "region".into(),
            point: None,
            curve: None,
            region: Some(whole_site_region(e.bounds)),
            rms_sec: None,
            underdetermined: if reasons.iter().any(|r| r.as_deref() == Some("superluminal_lag")) {
                "unobservable_input".into()
            } else if clock_freedom_dominates(&meas) {
                "clock_freedom".into()
            } else {
                "sensor_geometry".into()
            },
            score: f64::INFINITY,
            signature: hyp.signature.clone(),
            residuals: build_residuals(None),
        },
        1 => {
            let m = &meas[active_idx[0]];
            let diff = m.tdoa.tau * m.tdoa.c;
            let curve = hyperbola(m.tdoa.a, m.tdoa.b, diff, e.bounds, 240);
            Candidate {
                candidate_id: id_base,
                job_id: 0,
                rank: 0,
                kind: "curve".into(),
                point: None,
                curve,
                region: None,
                rms_sec: None,
                underdetermined: "sensor_geometry".into(),
                score: f64::MAX / 4.0,
                signature: hyp.signature.clone(),
                residuals: build_residuals(None),
            }
        }
        _ => {
            // 多起点无约束 LM；选取不加权 RMS 最小的收敛解。
            let seeds = grid_seeds(e.bounds, 5);
            let mut best: Option<(geom::LmResult, f64)> = None;
            for seed in seeds {
                let res = geom::solve_lm(&active, seed, 40);
                let raw_rms = raw_rms(&active, res.x);
                if best.as_ref().map_or(true, |(_, r)| raw_rms < *r)
                    || best
                        .as_ref()
                        .map_or(false, |(b, r)| (raw_rms - *r).abs() < 1e-12 && tiebreak(res.x, b.x))
                {
                    best = Some((res, raw_rms));
                }
            }
            let (lm, raw_rms) = best.expect("seeds non-empty");
            let (lmin, cond, _angle) = geom::observability(&active, lm.x);
            let inside = lm.x.0 >= e.bounds.min_x
                && lm.x.0 <= e.bounds.max_x
                && lm.x.1 >= e.bounds.min_y
                && lm.x.1 <= e.bounds.max_y;
            let residuals = build_residuals(Some(lm.x));

            // 欠定/不可满足优先于点结果，保持诚实：优化器不通过夹紧制造“可行”。
            if raw_rms > INCONSISTENT_RMS_SEC || lm.diverged {
                let region = uncertainty_region(lm.x, &active, raw_rms.max(1e-3), e.bounds);
                Candidate {
                    candidate_id: id_base, job_id: 0, rank: 0,
                    kind: "region".into(), point: None, curve: None,
                    region: Some(region),
                    rms_sec: Some(raw_rms),
                    underdetermined: if has_reflection_tension(&meas, &used) {
                        "multipath".into()
                    } else {
                        "inconsistent_measurements".into()
                    },
                    score: raw_rms + 1e6,
                    signature: hyp.signature.clone(),
                    residuals,
                }
            } else if lmin < WEAK_LAMBDA_MIN || !cond.is_finite() || cond > 1e6 {
                let region = uncertainty_region(lm.x, &active, raw_rms.max(1e-4), e.bounds);
                Candidate {
                    candidate_id: id_base, job_id: 0, rank: 0,
                    kind: "region".into(),
                    point: Some([lm.x.0, lm.x.1]),
                    curve: None, region: Some(region),
                    rms_sec: Some(raw_rms),
                    underdetermined: "sensor_geometry".into(),
                    score: raw_rms + 10.0,
                    signature: hyp.signature.clone(),
                    residuals,
                }
            } else if !inside {
                // 域外解：保留位置并标记，绝不夹回边界。
                Candidate {
                    candidate_id: id_base, job_id: 0, rank: 0,
                    kind: "point".into(),
                    point: Some([lm.x.0, lm.x.1]),
                    curve: None, region: None,
                    rms_sec: Some(raw_rms),
                    underdetermined: "outside_observable_geometry".into(),
                    score: raw_rms + 1e3,
                    signature: hyp.signature.clone(),
                    residuals,
                }
            } else {
                Candidate {
                    candidate_id: id_base, job_id: 0, rank: 0,
                    kind: "point".into(),
                    point: Some([lm.x.0, lm.x.1]),
                    curve: None, region: None,
                    rms_sec: Some(raw_rms),
                    underdetermined: "".into(),
                    score: raw_rms,
                    signature: hyp.signature.clone(),
                    residuals,
                }
            }
        }
    };

    HypResult { hyp_index: hyp.index, signature: hyp.signature.clone(), candidate }
}

fn raw_rms(meas: &[Tdoa], x: Pt) -> f64 {
    let s: f64 = meas
        .iter()
        .map(|m| {
            let r = (geom::dist(x, m.a) - geom::dist(x, m.b)) / m.c - m.tau;
            r * r
        })
        .sum();
    (s / meas.len().max(1) as f64).sqrt()
}

fn tiebreak(a: Pt, b: Pt) -> bool {
    if (a.0 - b.0).abs() > 1e-9 {
        a.0 < b.0
    } else {
        a.1 < b.1
    }
}

fn clock_freedom_dominates(meas: &[DerivedMeas]) -> bool {
    meas.iter().any(|m| m.kind == "onset_pair" && !m.clock_locked)
        && !meas.iter().any(|m| m.kind == "ccf_lag")
}

fn has_reflection_tension(meas: &[DerivedMeas], used: &[bool]) -> bool {
    meas.iter().enumerate().any(|(i, m)| used[i] && m.reflection)
}

/// 求解入口：并行计算各假设，输出稳定排序的候选列表。
///
/// 并行确定性保证：
/// - 假设按字典序枚举，候选编号在枚举时固定；
/// - 工作线程只返回结果，排名在收集后由排序键统一决定；
/// - 同分时按候选编号（=枚举序）决胜，线程调度不影响输出。
pub fn solve(input: SolverInput, job_id: i64) -> SolveOutput {
    let fingerprint = input_fingerprint(&input);
    let meas = derive_measurements(&input);
    let (manual_excluded, _locked, ties) = active_subjects(&input.evidence);
    let reflection_lags: Vec<String> = meas
        .iter()
        .filter(|m| m.reflection && m.lag_uid.is_some())
        .map(|m| m.lag_uid.clone().unwrap())
        .collect();
    let hyps = enumerate_hypotheses(&reflection_lags, &manual_excluded);

    let input = Arc::new(input);
    let meas = Arc::new(meas);

    // 并行求解（最多 4 线程）；结果按假设编号回填，不依赖完成顺序。
    let chunks: Vec<Vec<Hypothesis>> = hyps
        .chunks(4)
        .map(|c| c.to_vec())
        .collect();
    let mut handles = Vec::new();
    for chunk in chunks {
        let input = Arc::clone(&input);
        let meas = Arc::clone(&meas);
        handles.push(std::thread::spawn(move || {
            chunk
                .into_iter()
                .map(|h| solve_hypothesis(Arc::clone(&input), Arc::clone(&meas), h))
                .collect::<Vec<_>>()
        }));
    }
    let mut results: Vec<HypResult> = Vec::new();
    for h in handles {
        results.extend(h.join().expect("solver worker panicked"));
    }
    results.sort_by_key(|r| r.hyp_index);

    // 几何上相同的点候选去重（不同反射子集可能给出同一位置）。
    let mut dedup: Vec<HypResult> = Vec::new();
    for r in results {
        let dup = dedup.iter().any(|d| match (&d.candidate.point, &r.candidate.point) {
            (Some(a), Some(b)) => (a[0] - b[0]).hypot(a[1] - b[1]) < 0.05,
            _ => d.signature == r.signature,
        });
        if !dup {
            dedup.push(r);
        }
    }

    dedup.sort_by(|a, b| {
        a.candidate
            .score
            .partial_cmp(&b.candidate.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.hyp_index.cmp(&b.hyp_index))
    });

    let mut candidates: Vec<Candidate> = dedup
        .into_iter()
        .enumerate()
        .map(|(rank, mut r)| {
            r.candidate.rank = rank as i64 + 1;
            r.candidate.job_id = job_id;
            r.candidate.candidate_id = r
                .candidate
                .candidate_id
                .replace(":jobTBD:", &format!(":j{job_id}:"));
            r.candidate
        })
        .collect();

    // 不可分位置：若前两名点候选 RMS 差距在噪声带内，标记并保留两者。
    if candidates.len() >= 2 {
        let a = candidates[0].clone();
        let b = candidates[1].clone();
        if a.kind == "point" && b.kind == "point" {
            let gap = (a.score - b.score).abs();
            if gap < 0.0008 {
                candidates[0].underdetermined = "ambiguous_pair".into();
                candidates[1].underdetermined = "ambiguous_pair".into();
            }
        }
    }
    // 工程师手工保留的两个候选：无论当前排名都打标（重算后若编号仍存在）。
    let idset: BTreeSet<String> = candidates.iter().map(|c| c.candidate_id.clone()).collect();
    for (x, y) in &ties {
        if idset.contains(x) && idset.contains(y) {
            for c in candidates.iter_mut() {
                if &c.candidate_id == x || &c.candidate_id == y {
                    c.underdetermined = "kept_indistinguishable".into();
                }
            }
        }
    }

    SolveOutput { fingerprint, candidates }
}
