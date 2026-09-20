//! TDOA localization with explicit underdetermination.
//!
//! Every solve takes an immutable snapshot (observations + physical model +
//! derived evidence) and produces a NEW candidate set. The output is fully
//! deterministic: given the same snapshot, parallel or repeated solves rank
//! candidates identically and reuse the same candidate keys.

use crate::clock::{effective_offset, EffectiveOffset};
use crate::linalg::solve_normal;
use crate::models::ResidualRow;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

/// A delay spread above this marks an edge as clock-dominated (clock freedom
/// rather than geometry driving the uncertainty).
pub const CLOCK_DOMINANT_SIGMA: f64 = 0.1;
const MAX_MULTIPATH_SUSPECTS: usize = 3;
const POSITION_GRID_M: f64 = 0.1;
const MERGE_DISTANCE_M: f64 = 1.0;
/// Weighted RMS above which the all-direct edge set is treated as
/// inconsistent (a reflected path is masquerading as direct evidence).
const INCONSISTENT_NSIGMA: f64 = 0.8;

#[derive(Debug, Clone)]
pub struct StationInfo {
    pub station_id: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone)]
pub struct EdgeObs {
    pub station_a: String,
    pub station_b: String,
    /// measured arrival difference (b - a), seconds of site time
    pub delay_s: f64,
    pub sigma_s: f64,
    pub multipath: bool,
    pub xc_id: Option<i64>,
    pub peak_index: i64,
    pub origin: String, // "xcorr" | "onset"
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub event_id: String,
    pub t_mid: f64,
    pub sound_speed: f64,
    pub observable_radius: f64,
    pub stations: Vec<StationInfo>,
    pub segments: BTreeMap<String, Vec<crate::models::ClockSegment>>,
    pub edges: Vec<EdgeObs>,
    pub exclude_edges: BTreeSet<(String, String, i64)>,
    pub locks: BTreeMap<String, (f64, f64)>,
    pub kept_pairs: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct Edge {
    a: usize,
    b: usize,
    delay: f64,
    sigma: f64,
    origin_idx: usize,
    suspect: bool,
}

#[derive(Debug, Clone)]
pub struct CandidateOut {
    pub kind: String,
    pub status: String,
    pub reason: String,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub region: Option<Value>,
    pub metrics: Value,
    pub residuals: Vec<ResidualRow>,
    pub cand_key: String,
}

struct Location {
    x: f64,
    y: f64,
    rms_nsigma: f64,
    max_residual_s: f64,
    chi2: f64,
    rank: usize,
}

struct CoreResult {
    points: Vec<Location>,
    region: Option<(Value, String)>,
}

fn offsets(snap: &Snapshot) -> BTreeMap<String, EffectiveOffset> {
    snap.stations
        .iter()
        .map(|s| {
            let lock = snap.locks.get(&s.station_id).copied();
            let segs = snap
                .segments
                .get(&s.station_id)
                .cloned()
                .unwrap_or_default();
            (
                s.station_id.clone(),
                effective_offset(&segs, lock, snap.t_mid),
            )
        })
        .collect()
}

/// Build deterministically ordered, clock-corrected, de-duplicated edges.
/// Returns edges and a per-raw-edge disposition: false means globally
/// excluded (reviewer evidence or reflected peak).
fn prepare_edges(
    snap: &Snapshot,
    off: &BTreeMap<String, EffectiveOffset>,
) -> (Vec<Edge>, Vec<bool>) {
    let pos: BTreeMap<&String, usize> = snap
        .stations
        .iter()
        .enumerate()
        .map(|(i, s)| (&s.station_id, i))
        .collect();
    let mut raw: Vec<(usize, usize, Edge)> = Vec::new();
    let mut used_raw = vec![false; snap.edges.len()];

    for (idx, e) in snap.edges.iter().enumerate() {
        let excluded_peak =
            snap.exclude_edges
                .contains(&(e.station_a.clone(), e.station_b.clone(), e.peak_index))
                || snap.exclude_edges.contains(&(
                    e.station_b.clone(),
                    e.station_a.clone(),
                    e.peak_index,
                ));
        let reflected = e.peak_index > 0;
        if excluded_peak || reflected {
            continue;
        }
        let (Some(&ia), Some(&ib)) = (pos.get(&e.station_a), pos.get(&e.station_b)) else {
            continue;
        };
        let oa = &off[&e.station_a];
        let ob = &off[&e.station_b];
        let delay = e.delay_s + oa.offset_s - ob.offset_s;
        let sigma = (e.sigma_s.powi(2) + oa.sigma_s.powi(2) + ob.sigma_s.powi(2)).sqrt();
        let (a, b, d) = if ia <= ib {
            (ia, ib, delay)
        } else {
            (ib, ia, -delay)
        };
        raw.push((
            a,
            b,
            Edge {
                a,
                b,
                delay: d,
                sigma,
                origin_idx: idx,
                suspect: e.multipath,
            },
        ));
    }

    raw.sort_by(|p, q| (p.0, p.1, p.2.origin_idx).cmp(&(q.0, q.1, q.2.origin_idx)));

    let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
    let mut edges = Vec::new();
    for (a, b, edge) in raw {
        if !seen.insert((a, b)) {
            continue;
        }
        used_raw[edge.origin_idx] = true;
        edges.push(edge);
    }
    (edges, used_raw)
}

fn station_pos(snap: &Snapshot, i: usize) -> (f64, f64) {
    (snap.stations[i].x, snap.stations[i].y)
}

fn predict(snap: &Snapshot, a: usize, b: usize, x: f64, y: f64) -> f64 {
    let (ax, ay) = station_pos(snap, a);
    let (bx, by) = station_pos(snap, b);
    let ra = ((x - ax).powi(2) + (y - ay).powi(2)).sqrt();
    let rb = ((x - bx).powi(2) + (y - by).powi(2)).sqrt();
    (rb - ra) / snap.sound_speed
}

fn in_domain(snap: &Snapshot, x: f64, y: f64) -> bool {
    x.is_finite() && y.is_finite() && (x * x + y * y).sqrt() <= snap.observable_radius
}

fn choose_reference(snap: &Snapshot, edges: &[Edge]) -> usize {
    let mut degree = vec![0usize; snap.stations.len()];
    let mut weight = vec![0.0f64; snap.stations.len()];
    for e in edges {
        degree[e.a] += 1;
        degree[e.b] += 1;
        weight[e.a] += 1.0 / e.sigma.powi(2);
        weight[e.b] += 1.0 / e.sigma.powi(2);
    }
    (0..snap.stations.len())
        .max_by(|&i, &j| {
            degree[i]
                .cmp(&degree[j])
                .then(weight[i].partial_cmp(&weight[j]).unwrap())
        })
        .unwrap_or(0)
}

fn add_row(m: &mut [[f64; 3]; 3], rhs: &mut [f64; 3], row: [f64; 3], target: f64, w: f64) {
    for i in 0..3 {
        rhs[i] += w * row[i] * target;
        for j in 0..3 {
            m[i][j] += w * row[i] * row[j];
        }
    }
}

/// Gauss-Newton refinement over ALL active edges. Returns None if the walk
/// leaves the observable domain (the estimate is discarded, never clamped).
fn refine(snap: &Snapshot, edges: &[Edge], x0: f64, y0: f64) -> Option<(f64, f64)> {
    let (mut x, mut y) = (x0, y0);
    for _ in 0..10 {
        let mut m = [[0.0f64; 2]; 2];
        let mut rhs = [0.0f64; 2];
        for e in edges {
            let (ax, ay) = station_pos(snap, e.a);
            let (bx, by) = station_pos(snap, e.b);
            let ra = ((x - ax).powi(2) + (y - ay).powi(2)).sqrt().max(1e-3);
            let rb = ((x - bx).powi(2) + (y - by).powi(2)).sqrt().max(1e-3);
            let pred = (rb - ra) / snap.sound_speed;
            let res = e.delay - pred;
            let jx = ((x - bx) / rb - (x - ax) / ra) / snap.sound_speed;
            let jy = ((y - by) / rb - (y - ay) / ra) / snap.sound_speed;
            let w = 1.0 / e.sigma.powi(2);
            m[0][0] += w * jx * jx;
            m[0][1] += w * jx * jy;
            m[1][0] += w * jy * jx;
            m[1][1] += w * jy * jy;
            rhs[0] += w * jx * res;
            rhs[1] += w * jy * res;
        }
        let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
        if det.abs() < 1e-18 {
            return Some((x, y));
        }
        let dx = (m[1][1] * rhs[0] - m[0][1] * rhs[1]) / det;
        let dy = (-m[1][0] * rhs[0] + m[0][0] * rhs[1]) / det;
        x += dx;
        y += dy;
        if !in_domain(snap, x, y) {
            return None;
        }
        if dx.hypot(dy) < 1e-7 {
            return Some((x, y));
        }
    }
    Some((x, y))
}

fn evaluate(snap: &Snapshot, edges: &[Edge], x: f64, y: f64, rank: usize) -> Location {
    let mut chi2 = 0.0;
    let mut max_res: f64 = 0.0;
    for e in edges {
        let res = e.delay - predict(snap, e.a, e.b, x, y);
        chi2 += (res / e.sigma).powi(2);
        max_res = max_res.max(res.abs());
    }
    let n = edges.len().max(1) as f64;
    Location {
        x,
        y,
        rms_nsigma: (chi2 / n).sqrt(),
        max_residual_s: max_res,
        chi2,
        rank,
    }
}

/// Exact linear hyperbolic solve (Schaer-style), returning rank, minimum norm
/// z=[x,y,r0] and nullspace basis. Star edges only, relative to `ref_idx`.
fn linear_system(snap: &Snapshot, edges: &[Edge], ref_idx: usize) -> crate::linalg::SolveResult {
    let (rx, ry) = station_pos(snap, ref_idx);
    let k0 = rx * rx + ry * ry;
    let mut m = [[0.0f64; 3]; 3];
    let mut rhs = [0.0f64; 3];
    for e in edges {
        let (other, sign) = if e.a == ref_idx {
            (e.b, 1.0) // delay = (r_i - r_0)/c
        } else if e.b == ref_idx {
            (e.a, -1.0)
        } else {
            continue;
        };
        let (px, py) = station_pos(snap, other);
        let ki = px * px + py * py;
        let d = sign * e.delay * snap.sound_speed;
        let w = 1.0 / (e.sigma * snap.sound_speed).powi(2);
        // (x_i-x_0)x + (y_i-y_0)y + d r0 = (K_i - K_0 - d^2)/2
        let row = [px - rx, py - ry, d];
        let target = (ki - k0 - d * d) / 2.0;
        add_row(&mut m, &mut rhs, row, target, w);
    }
    solve_normal(&m, &rhs)
}

fn solve_core(snap: &Snapshot, edges: &[Edge]) -> CoreResult {
    let ref_idx = choose_reference(snap, edges);
    let (rx, ry) = station_pos(snap, ref_idx);

    // Geometry rank is decided by timely edges only; clock-dominated edges
    // still inform refinement but never inflate an underdetermined result.
    let good: Vec<&Edge> = edges
        .iter()
        .filter(|e| e.sigma < CLOCK_DOMINANT_SIGMA)
        .collect();
    let any_unknown = edges.iter().any(|e| e.sigma >= CLOCK_DOMINANT_SIGMA);
    let n_good = good.len();

    let good_owned: Vec<Edge> = good.iter().map(|e| (*e).clone()).collect();
    let sol = linear_system(snap, &good_owned, ref_idx);
    let rank = sol.rank;

    let mut points: Vec<Location> = Vec::new();
    let mut region: Option<(Value, String)> = None;

    if rank == 3 {
        let (x, y, r0) = (sol.solution[0], sol.solution[1], sol.solution[2]);
        // Over-determined linear solve ignores the quadratic constraint, so
        // require only that the implied range be positive and reasonably close
        // to the reference distance (noise can spread them several metres).
        let rref = ((x - rx).powi(2) + (y - ry).powi(2)).sqrt();
        let consistent = r0 > 1.0 && (r0 - rref).abs() <= 40.0_f64.max(rref * 0.6);
        if consistent && in_domain(snap, x, y) {
            if let Some((rx2, ry2)) = refine(snap, edges, x, y) {
                points.push(final_location(snap, edges, rx2, ry2, 3));
            }
        }
        if points.is_empty() {
            region = Some((
                empty_region(
                    snap,
                    "no consistent solution inside the observable geometry",
                ),
                region_reason(any_unknown, n_good, 3),
            ));
        }
    } else if rank == 2 {
        // z = z0 + t n; impose |p - p_ref|^2 = r0^2.
        let n = sol.nullspace[0];
        let z0 = sol.solution;
        let a = n[0].powi(2) + n[1].powi(2) - n[2].powi(2);
        let b = 2.0 * (n[0] * (z0[0] - rx) + n[1] * (z0[1] - ry) - n[2] * z0[2]);
        let c = (z0[0] - rx).powi(2) + (z0[1] - ry).powi(2) - z0[2].powi(2);
        let mut roots = Vec::new();
        if a.abs() < 1e-12 {
            if b.abs() > 1e-12 {
                roots.push(-c / b);
            }
        } else {
            let disc = b * b - 4.0 * a * c;
            if disc >= 0.0 {
                let q = disc.sqrt();
                roots.push((-b + q) / (2.0 * a));
                roots.push((-b - q) / (2.0 * a));
            }
        }
        let clock_dominates = any_unknown && n_good < 3;
        if clock_dominates {
            let sampled = sample_nullspace(snap, &z0, &n);
            region = Some((
                json!({
                    "shape": "line",
                    "coordinates": sampled,
                    "source": "clock degree of freedom along rank-deficient intersection",
                    "width_m": sigma_width(snap, edges),
                    "coordinate_unit": "m",
                }),
                "clock_freedom".into(),
            ));
        } else {
            for t in roots {
                let x = z0[0] + t * n[0];
                let y = z0[1] + t * n[1];
                let r0v = z0[2] + t * n[2];
                if r0v < 0.0 || !in_domain(snap, x, y) {
                    continue;
                }
                if let Some((rx2, ry2)) = refine(snap, edges, x, y) {
                    points.push(final_location(snap, edges, rx2, ry2, 2));
                }
            }
            if points.is_empty() {
                let sampled = sample_nullspace(snap, &z0, &n);
                region = Some((
                    json!({
                        "shape": "line",
                        "coordinates": sampled,
                        "source": "rank-deficient TDOA intersection",
                        "width_m": sigma_width(snap, edges),
                        "coordinate_unit": "m",
                    }),
                    region_reason(any_unknown, n_good, 2),
                ));
            }
        }
    } else if n_good >= 1 {
        // Single usable difference of ranges -> one hyperbola branch pair.
        let e = good
            .iter()
            .min_by_key(|e| (e.origin_idx, e.a, e.b))
            .unwrap();
        let (ax, ay) = station_pos(snap, e.a);
        let (bx, by) = station_pos(snap, e.b);
        let d = e.delay * snap.sound_speed;
        let reason = region_reason(any_unknown, n_good, 1);
        let adelta = d.abs() / 2.0;
        let focal = ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt() / 2.0;
        if adelta >= focal {
            region = Some((
                empty_region(
                    snap,
                    "measured delay exceeds the baseline; no hyperbolic locus exists",
                ),
                reason,
            ));
        } else {
            region = Some((
                hyperbola_region(snap, ax, ay, bx, by, d, e.sigma * snap.sound_speed),
                reason,
            ));
        }
    } else {
        region = Some((
            empty_region(snap, "no timely pair-wise delay edges"),
            region_reason(any_unknown, n_good, 0),
        ));
    }

    CoreResult { points, region }
}

fn final_location(snap: &Snapshot, edges: &[Edge], x: f64, y: f64, geom_rank: usize) -> Location {
    // The geometric rank from the star linearization is structural: extra
    // collinear/chord edges never remove a mirror ambiguity, even though their
    // Jacobians span numerically during refinement.
    evaluate(snap, edges, x, y, geom_rank)
}

fn region_reason(any_unknown: bool, n_good: usize, rank: usize) -> String {
    // Free clock segments on participating stations leave only clock-dominated
    // differences; structurally the geometry is fine but the clock degree of
    // freedom dominates the uncertainty.
    if any_unknown && n_good < 3 - rank {
        "clock_freedom".into()
    } else {
        "sensor_geometry".into()
    }
}

fn sigma_width(snap: &Snapshot, edges: &[Edge]) -> f64 {
    let wsum: f64 = edges.iter().map(|e| 1.0 / e.sigma.powi(2)).sum();
    if wsum <= 0.0 {
        snap.observable_radius * 0.02
    } else {
        (1.0 / wsum).sqrt() * snap.sound_speed
    }
}

fn sample_nullspace(snap: &Snapshot, z0: &[f64; 3], n: &[f64; 3]) -> Vec<[f64; 2]> {
    let span = snap.observable_radius;
    let mut out = Vec::new();
    let steps = 80;
    for i in 0..=steps {
        let t = -span + 2.0 * span * i as f64 / steps as f64;
        let x = z0[0] + t * n[0];
        let y = z0[1] + t * n[1];
        if in_domain(snap, x, y) {
            out.push([x, y]);
        }
    }
    if out.is_empty() {
        out.push([z0[0], z0[1]]);
    }
    out
}

fn hyperbola_region(
    snap: &Snapshot,
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    d: f64,
    width_m: f64,
) -> Value {
    let cx = (ax + bx) / 2.0;
    let cy = (ay + by) / 2.0;
    let f = ((ax - bx).powi(2) + (ay - by).powi(2)).sqrt() / 2.0;
    let angle = (by - ay).atan2(bx - ax);
    let adelta = d.abs() / 2.0;
    let mut coords: Vec<[f64; 2]> = Vec::new();
    if f <= adelta {
        coords.push([cx, cy]);
    } else {
        let aa = adelta.max(POSITION_GRID_M);
        let bb = (f * f - aa * aa).sqrt();
        let vmax = (snap.observable_radius / aa).min(6.0);
        let steps = 60;
        for sign in [-1.0f64, 1.0] {
            for i in 0..=steps {
                let v = 0.05 + (vmax - 0.05) * i as f64 / steps as f64;
                let u = sign * aa * v.cosh();
                let w = bb * v.sinh();
                let (x, y) = rotate(u, w, angle, cx, cy);
                if in_domain(snap, x, y) {
                    coords.push([x, y]);
                }
            }
            coords.push([f64::NAN, f64::NAN]);
        }
    }
    json!({
        "shape": "hyperbola",
        "coordinates": coords,
        "source": "single pair-wise delay edge",
        "width_m": width_m.max(1.0),
        "coordinate_unit": "m",
    })
}

fn rotate(u: f64, w: f64, angle: f64, cx: f64, cy: f64) -> (f64, f64) {
    let c = angle.cos();
    let s = angle.sin();
    (cx + c * u - s * w, cy + s * u + c * w)
}

fn empty_region(snap: &Snapshot, source: &str) -> Value {
    let r = snap.observable_radius;
    json!({
        "shape": "circle",
        "coordinates": [[0.0, 0.0]],
        "radius_m": r,
        "source": source,
        "coordinate_unit": "m",
    })
}

struct RawEdgeView {
    edge: Edge,
    excluded: bool,
}

fn all_edge_views(snap: &Snapshot, off: &BTreeMap<String, EffectiveOffset>) -> Vec<RawEdgeView> {
    let pos: BTreeMap<&String, usize> = snap
        .stations
        .iter()
        .enumerate()
        .map(|(i, s)| (&s.station_id, i))
        .collect();
    let mut rows = Vec::new();
    for (raw_idx, e) in snap.edges.iter().enumerate() {
        let by_evidence =
            snap.exclude_edges
                .contains(&(e.station_a.clone(), e.station_b.clone(), e.peak_index))
                || snap.exclude_edges.contains(&(
                    e.station_b.clone(),
                    e.station_a.clone(),
                    e.peak_index,
                ));
        let reflected = e.peak_index > 0;
        let excluded = by_evidence || reflected;
        let (ia_v, ib_v) = (
            pos.get(&e.station_a).copied(),
            pos.get(&e.station_b).copied(),
        );
        let (ia, ib) = match (ia_v, ib_v) {
            (Some(a), Some(b)) => (a, b),
            _ => continue,
        };
        let oa = &off[&e.station_a];
        let ob = &off[&e.station_b];
        let (a, b, delay) = {
            let d = e.delay_s + oa.offset_s - ob.offset_s;
            if ia <= ib {
                (ia, ib, d)
            } else {
                (ib, ia, -d)
            }
        };
        let sigma = (e.sigma_s.powi(2) + oa.sigma_s.powi(2) + ob.sigma_s.powi(2)).sqrt();
        rows.push(RawEdgeView {
            edge: Edge {
                a,
                b,
                delay,
                sigma,
                origin_idx: raw_idx,
                suspect: e.multipath,
            },
            excluded,
        });
    }
    rows.sort_by(|p, q| (p.edge.a, p.edge.b).cmp(&(q.edge.a, q.edge.b)));
    rows
}

fn candidate_residuals(
    snap: &Snapshot,
    views: &[RawEdgeView],
    point: Option<(f64, f64)>,
) -> Vec<ResidualRow> {
    views
        .iter()
        .map(|v| {
            let e = &v.edge;
            let (pred, res) = match point {
                Some((x, y)) => {
                    let p = predict(snap, e.a, e.b, x, y);
                    (p, e.delay - p)
                }
                None => (f64::NAN, f64::NAN),
            };
            let raw = &snap.edges[e.origin_idx];
            ResidualRow {
                edge: format!("{}->{}", raw.station_a, raw.station_b),
                kind: raw.origin.clone(),
                station_a: snap.stations[e.a].station_id.clone(),
                station_b: snap.stations[e.b].station_id.clone(),
                measured_s: e.delay,
                predicted_s: pred,
                residual_s: res,
                sigma_s: e.sigma,
                n_sigma: if e.sigma > 0.0 && res.is_finite() {
                    res / e.sigma
                } else {
                    f64::NAN
                },
                multipath: raw.multipath,
                excluded: v.excluded,
            }
        })
        .collect()
}

/// Enumerate deterministic multipath scenarios: the direct-only baseline plus
/// scenarios where individual suspect edges are dropped. Bounded so an
/// explosion of reflected peaks can never dominate the job.
fn multipath_scenarios(edges: &[Edge]) -> Vec<Vec<usize>> {
    let mut suspects: Vec<usize> = edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.suspect)
        .map(|(i, _)| i)
        .collect();
    suspects.sort();
    suspects.truncate(MAX_MULTIPATH_SUSPECTS);
    let mut scenarios = vec![Vec::new()]; // baseline: drop nothing extra
    for i in &suspects {
        scenarios.push(vec![*i]);
    }
    for w in suspects.windows(2) {
        scenarios.push(vec![w[0], w[1]]);
    }
    scenarios
}

/// Deterministic public entry point.
pub fn solve(snap: &Snapshot) -> Vec<CandidateOut> {
    let off = offsets(snap);
    let (active, _used_raw) = prepare_edges(snap, &off);
    let views = all_edge_views(snap, &off);

    let mut raw_candidates: Vec<(Location, Vec<usize>)> = Vec::new();
    let mut region_out: Option<(Value, String)> = None;

    let scenarios = multipath_scenarios(&active);
    for dropped in &scenarios {
        let edges: Vec<Edge> = active
            .iter()
            .enumerate()
            .filter(|(i, _)| !dropped.contains(i))
            .map(|(_, e)| e.clone())
            .collect();
        if edges.is_empty() {
            continue;
        }
        let core = solve_core(snap, &edges);
        for loc in core.points {
            let active_index_set: Vec<usize> = edges
                .iter()
                .map(|e| {
                    active
                        .iter()
                        .position(|a| a.origin_idx == e.origin_idx)
                        .unwrap()
                })
                .collect();
            raw_candidates.push((loc, dropped.clone()));
            let _ = active_index_set;
        }
        if region_out.is_none() {
            region_out = core.region;
        }
    }

    // Was the all-direct (no suspect dropped) scenario self-consistent?
    let baseline_ok = raw_candidates
        .iter()
        .any(|(loc, dropped)| dropped.is_empty() && loc.rms_nsigma <= INCONSISTENT_NSIGMA);

    // Deduplicate (nearly) identical numerical points across scenarios,
    // keeping the smallest dropped set and residual for each location.
    raw_candidates.sort_by(|a, b| {
        (
            ordered_f64(a.0.rms_nsigma),
            ordered_f64(a.0.max_residual_s),
            ordered_f64(a.0.x),
            ordered_f64(a.0.y),
        )
            .cmp(&(
                ordered_f64(b.0.rms_nsigma),
                ordered_f64(b.0.max_residual_s),
                ordered_f64(b.0.x),
                ordered_f64(b.0.y),
            ))
    });
    let mut unique: Vec<(Location, Vec<usize>)> = Vec::new();
    for c in raw_candidates {
        if let Some(existing) = unique.iter_mut().find(|u| {
            let dx = u.0.x - c.0.x;
            let dy = u.0.y - c.0.y;
            dx.hypot(dy) < MERGE_DISTANCE_M
        }) {
            // Keep the representation with the smallest residual; remember if
            // reaching it required rejecting a multipath-suspect edge.
            let c_needed_drop = !c.1.is_empty();
            let e_needed_drop = !existing.1.is_empty();
            if c.0.rms_nsigma < existing.0.rms_nsigma {
                let dropped = if c_needed_drop || e_needed_drop {
                    if c_needed_drop {
                        c.1.clone()
                    } else {
                        existing.1.clone()
                    }
                } else {
                    Vec::new()
                };
                let mut loc = c.0;
                loc.rank = loc.rank.max(existing.0.rank);
                *existing = (loc, dropped);
            } else if c_needed_drop && !e_needed_drop {
                existing.1 = c.1.clone();
            }
        } else {
            unique.push(c);
        }
    }

    let mut out: Vec<CandidateOut> = Vec::new();
    for (rank, (loc, dropped)) in unique.iter().enumerate() {
        let key = cand_key(snap, "point", Some((loc.x, loc.y)), rank, None);
        let metrics = json!({
            "rms_nsigma": loc.rms_nsigma,
            "max_residual_s": loc.max_residual_s,
            "chi2": loc.chi2,
            "rank": loc.rank,
            "dropped_suspect_edges": dropped.len(),
            "coordinate_unit": "m",
            "time_unit": "s",
        });
        let geometry_under = loc.rank < 3;
        // Multipath attribution: the all-direct set is statistically
        // inconsistent, and this point only becomes clean by rejecting a
        // suspected reflected path — the reviewer must pick which is direct.
        let multipath_under =
            !dropped.is_empty() && !baseline_ok && loc.rms_nsigma <= INCONSISTENT_NSIGMA;
        let under = geometry_under || multipath_under;
        let reason = if geometry_under {
            "sensor_geometry: two mirror locations fit the two delay differences".into()
        } else if multipath_under {
            "multipath: consistent only after rejecting a suspected reflected path".into()
        } else if dropped.is_empty() {
            "direct evidence consistent".into()
        } else {
            "consistent; also fits when a suspected multipath edge is rejected".into()
        };
        out.push(CandidateOut {
            kind: "point".into(),
            status: if under {
                "underdetermined".into()
            } else {
                "determined".into()
            },
            reason,
            x: Some(loc.x),
            y: Some(loc.y),
            region: None,
            metrics,
            residuals: candidate_residuals(snap, &views, Some((loc.x, loc.y))),
            cand_key: key,
        });
    }

    if out.is_empty() {
        if let Some((region, reason)) = region_out {
            let key = cand_key(snap, "region", None, 0, Some(&region));
            out.push(CandidateOut {
                kind: "region".into(),
                status: "underdetermined".into(),
                reason,
                x: None,
                y: None,
                region: Some(region),
                metrics: json!({ "coordinate_unit": "m", "time_unit": "s" }),
                residuals: candidate_residuals(snap, &views, None),
                cand_key: key,
            });
        }
    }

    // keep_pair evidence: when two surviving points are explicitly retained as
    // inseparable, mark both (no candidate is deleted or rewritten).
    if out.len() >= 2 {
        let keys: Vec<String> = out.iter().map(|c| c.cand_key.clone()).collect();
        for (ka, kb) in &snap.kept_pairs {
            let ia = keys.iter().position(|k| k == ka);
            let ib = keys.iter().position(|k| k == kb);
            if let (Some(ia), Some(ib)) = (ia, ib) {
                mark_kept(&mut out[ia], ka, kb);
                mark_kept(&mut out[ib], ka, kb);
            }
        }
    }

    out
}

fn mark_kept(cand: &mut CandidateOut, ka: &str, kb: &str) {
    cand.reason = format!("kept inseparable pair ({ka}, {kb})");
    if let Some(obj) = cand.metrics.as_object_mut() {
        obj.insert("kept_pair".into(), json!([ka, kb]));
    }
}

fn ordered_f64(v: f64) -> i64 {
    // Ranking only needs stable ordering; quantize residuals/positions.
    (v * 1e9).round() as i64
}

fn cand_key(
    snap: &Snapshot,
    kind: &str,
    point: Option<(f64, f64)>,
    rank: usize,
    region: Option<&Value>,
) -> String {
    let base = match (point, region) {
        (Some((x, y)), _) => format!("{kind}:{:.4},{:.4}", x, y),
        (None, Some(r)) => format!("{kind}:{}", crate::hash::canonical_json(r)),
        (None, None) => format!("{kind}:none"),
    };
    let digest = &crate::hash::sha256_hex(&[&snap.event_id, &base])[..12];
    format!("{kind}-{rank}-{digest}")
}
