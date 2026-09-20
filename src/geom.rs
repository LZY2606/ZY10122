//! 二维 TDOA 几何：双曲线采样、高斯-牛顿定位、可观测性诊断。

use crate::model::{Curve, Region, SiteBounds};

#[derive(Debug, Clone, Copy)]
pub struct Pt(pub f64, pub f64);

pub fn dist(a: Pt, b: Pt) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

pub fn clamp_pt(p: Pt, b: SiteBounds) -> Pt {
    Pt(
        p.0.clamp(b.min_x, b.max_x),
        p.1.clamp(b.min_y, b.max_y),
    )
}

/// 距离差双曲线（到 a 与到 b 的距离差 = diff_m）。
/// 用焦点连线为横轴的参数式采样，并与场地边界求交裁剪。
pub fn hyperbola(a: Pt, b: Pt, diff_m: f64, bounds: SiteBounds, steps: usize) -> Option<Curve> {
    let d = dist(a, b);
    if !diff_m.is_finite() || d < 1e-9 || diff_m.abs() >= d - 1e-9 {
        // 超光速差（|tau| c >= 基线长度）没有实双曲线——属于超出可观测几何的输入。
        return None;
    }
    let mid = Pt((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let alpha = ((b.1 - a.1).atan2(b.0 - a.0) + std::f64::consts::TAU) % std::f64::consts::TAU;
    let (sin_a, cos_a) = alpha.sin_cos();
    let aa = diff_m.abs() / 2.0;
    let bb2 = (d * d - diff_m * diff_m) / 4.0;
    let bb = bb2.max(0.0).sqrt();
    // 覆盖足够大的参数范围，之后用场地框裁剪。
    let span = (bounds.max_x - bounds.min_x).hypot(bounds.max_y - bounds.min_y).max(10.0);
    let t_max = (span / aa.max(0.5)).asinh().max(1.0) * 3.0;
    let sign = diff_m.signum();
    let mut points = Vec::new();
    for i in 0..=steps {
        let t = -t_max + 2.0 * t_max * (i as f64) / (steps as f64);
    let (ch, sh) = (t.cosh(), t.sinh());
        // diff>0 时点靠近 b（到 a 更远），参数 x' 取 sign*a*cosh。
        let xp = sign * aa * ch;
        let yp = bb * sh;
        let x = mid.0 + xp * cos_a - yp * sin_a;
        let y = mid.1 + xp * sin_a + yp * cos_a;
        if x.is_finite()
            && y.is_finite()
            && x >= bounds.min_x - span
            && x <= bounds.max_x + span
            && y >= bounds.min_y - span
            && y <= bounds.max_y + span
        {
            points.push(clamp_pt(Pt(x, y), bounds));
        }
    }
    if points.len() < 2 {
        return None;
    }
    Some(Curve {
        kind: "hyperbola".into(),
        focus_a: [a.0, a.1],
        focus_b: [b.0, b.1],
        difference_m: diff_m,
        points: points.iter().map(|p| [p.0, p.1]).collect(),
    })
}

/// 一条 TDOA 观测：预测残差 r(x) = (|x-a|-|x-b|)/c - tau。
#[derive(Debug, Clone)]
pub struct Tdoa {
    pub a: Pt,
    pub b: Pt,
    pub tau: f64,
    pub c: f64,
    pub weight: f64,
    pub tag: String,
}

/// 高斯-牛顿（LM）非线性最小二乘。**不加边界约束**：
/// 超出可观测几何的解不被硬拉回场地，而是通过残差与发散度交由上层标记。
pub fn solve_lm(meas: &[Tdoa], x0: Pt, max_iter: usize) -> LmResult {
    let mut x = x0;
    let mut lambda = 1e-3;
    let mut last = f64::MAX;
    let n = meas.len();
    for _ in 0..max_iter {
        let r: Vec<f64> = meas
            .iter()
            .map(|m| ((dist(x, m.a) - dist(x, m.b)) / m.c - m.tau) * m.weight)
            .collect();
        let mut j = vec![0.0f64; 2 * n];
        for (i, m) in meas.iter().enumerate() {
            let da = dist(x, m.a).max(1e-6);
            let db = dist(x, m.b).max(1e-6);
            j[2 * i] = m.weight * ((x.0 - m.a.0) / da - (x.0 - m.b.0) / db) / m.c;
            j[2 * i + 1] = m.weight * ((x.1 - m.a.1) / da - (x.1 - m.b.1) / db) / m.c;
        }
        let mut jtj = [0.0f64; 4];
        let mut jtr = [0.0f64; 2];
        for i in 0..n {
            for u in 0..2 {
                jtr[u] -= j[2 * i + u] * r[i];
                for v in 0..2 {
                    jtj[2 * u + v] += j[2 * i + u] * j[2 * i + v];
                }
            }
        }
        let chi2: f64 = r.iter().map(|v| v * v).sum();
        if chi2 > last * 1e6 + 1e3 {
            return LmResult { x, diverged: true, rms: (chi2 / n.max(1) as f64).sqrt() };
        }
        last = chi2;
        // (JtJ + lambda diag) dx = Jt r
        let mut dx = [0.0f64; 2];
        if !solve2(
            jtj[0] + lambda * jtj[0].max(1e-12),
            jtj[1],
            jtj[3] + lambda * jtj[3].max(1e-12),
            jtr[0],
            jtr[1],
            &mut dx,
        ) {
            lambda *= 10.0;
            if lambda > 1e12 {
                return LmResult { x, diverged: true, rms: (chi2 / n.max(1) as f64).sqrt() };
            }
            continue;
        }
        let nx = Pt(x.0 + dx[0], x.1 + dx[1]);
        let nchi: f64 = meas
            .iter()
            .map(|m| {
                let e = ((dist(nx, m.a) - dist(nx, m.b)) / m.c - m.tau) * m.weight;
                e * e
            })
            .sum();
        if nchi < chi2 {
            x = nx;
            lambda *= 0.3;
            if dx[0].hypot(dx[1]) < 1e-10 {
                return LmResult { x, diverged: false, rms: (nchi / n.max(1) as f64).sqrt() };
            }
        } else {
            lambda *= 10.0;
            if lambda > 1e12 {
                return LmResult { x, diverged: true, rms: (chi2 / n.max(1) as f64).sqrt() };
            }
        }
    }
    let chi2: f64 = meas
        .iter()
        .map(|m| {
            let e = ((dist(x, m.a) - dist(x, m.b)) / m.c - m.tau) * m.weight;
            e * e
        })
        .sum();
    LmResult { x, diverged: false, rms: (chi2 / n.max(1) as f64).sqrt() }
}

pub struct LmResult {
    pub x: Pt,
    pub diverged: bool,
    pub rms: f64,
}

fn solve2(a: f64, b: f64, d: f64, e: f64, f: f64, x: &mut [f64; 2]) -> bool {
    let det = a * d - b * b;
    if det.abs() < 1e-18 {
        return false;
    }
    x[0] = (e * d - b * f) / det;
    x[1] = (a * f - e * b) / det;
    x.iter().all(|v| v.is_finite())
}

/// 可观测性：对加权雅可比 JtJ 做特征分解（2x2 闭式），
/// 返回 (最小特征值, 条件数, 主轴方向)。
pub fn observability(meas: &[Tdoa], x: Pt) -> (f64, f64, f64) {
    let n = meas.len();
    let mut jtj = [0.0f64; 4];
    for m in meas {
        let da = dist(x, m.a).max(1e-6);
        let db = dist(x, m.b).max(1e-6);
        let gx = ((x.0 - m.a.0) / da - (x.0 - m.b.0) / db) / m.c * m.weight;
        let gy = ((x.1 - m.a.1) / da - (x.1 - m.b.1) / db) / m.c * m.weight;
        jtj[0] += gx * gx;
        jtj[1] += gx * gy;
        jtj[3] += gy * gy;
    }
    let _ = n;
    let tr = jtj[0] + jtj[3];
    let disc = ((jtj[0] - jtj[3]).powi(2) + 4.0 * jtj[1] * jtj[1]).max(0.0).sqrt();
    let l1 = (tr + disc) / 2.0;
    let l2 = (tr - disc) / 2.0;
    let angle = if jtj[1].abs() < 1e-15 && (jtj[0] - jtj[3]).abs() < 1e-15 {
        0.0
    } else {
        0.5 * (2.0 * jtj[1]).atan2(jtj[0] - jtj[3])
    };
    let cond = if l2 > 1e-15 { l1 / l2 } else { f64::INFINITY };
    (l2, cond, angle)
}

/// 由残差 RMS 与局部可观测性构造不确定区域（误差椭圆→多边形）。
/// sigma 距离尺度 ≈ c * rms（秒换算成米），弱观测方向再按条件数放大。
pub fn uncertainty_region(
    center: Pt,
    meas: &[Tdoa],
    rms_sec: f64,
    bounds: SiteBounds,
) -> Region {
    let (lmin, cond, angle) = observability(meas, center);
    let base = meas.first().map(|m| m.c).unwrap_or(343.0) * rms_sec.max(1e-4) * 3.0;
    let weak = if lmin <= 1e-12 || !cond.is_finite() {
        base * 100.0
    } else {
        base * cond.sqrt().min(100.0)
    };
    ellipse_region(center, base.max(0.5), weak.max(0.5), angle, bounds, 48)
}

fn ellipse_region(
    center: Pt,
    strong: f64,
    weak: f64,
    angle: f64,
    bounds: SiteBounds,
    verts: usize,
) -> Region {
    let (s, c) = angle.sin_cos();
    let mut polygon = Vec::with_capacity(verts);
    for i in 0..verts {
        let t = std::f64::consts::TAU * (i as f64) / (verts as f64);
        let lx = strong * t.cos();
        let ly = weak * t.sin();
        let x = center.0 + lx * c - ly * s;
        let y = center.1 + lx * s + ly * c;
        polygon.push([
            x.clamp(bounds.min_x, bounds.max_x),
            y.clamp(bounds.min_y, bounds.max_y),
        ]);
    }
    Region {
        kind: "ellipse".into(),
        center: [center.0, center.1],
        semi_axes: [strong, weak],
        angle_rad: angle,
        polygon,
    }
}

/// 全场地区域（零有效观测时的欠定结果）。
pub fn whole_site_region(bounds: SiteBounds) -> Region {
    Region {
        kind: "site".into(),
        center: [
            (bounds.min_x + bounds.max_x) / 2.0,
            (bounds.min_y + bounds.max_y) / 2.0,
        ],
        semi_axes: [(bounds.max_x - bounds.min_x) / 2.0, (bounds.max_y - bounds.min_y) / 2.0],
        angle_rad: 0.0,
        polygon: vec![
            [bounds.min_x, bounds.min_y],
            [bounds.max_x, bounds.min_y],
            [bounds.max_x, bounds.max_y],
            [bounds.min_x, bounds.max_y],
        ],
    }
}

/// 多起点确定性网格：用于给 LM 提供初值（不随机）。
pub fn grid_seeds(bounds: SiteBounds, n: usize) -> Vec<Pt> {
    let mut out = Vec::new();
    for i in 0..n {
        for j in 0..n {
            let fx = (i as f64 + 0.5) / (n as f64);
            let fy = (j as f64 + 0.5) / (n as f64);
            out.push(Pt(
                bounds.min_x + fx * (bounds.max_x - bounds.min_x),
                bounds.min_y + fy * (bounds.max_y - bounds.min_y),
            ));
        }
    }
    out
}
