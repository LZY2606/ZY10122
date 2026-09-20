//! Householder QR based least squares for up to 3 unknowns, with rank and
//! nullspace. The solver needs a stable 3x3 solve more than a math library.

#[derive(Debug, Clone)]
pub struct SolveResult {
    pub solution: [f64; 3],
    pub rank: usize,
    pub nullspace: Vec<[f64; 3]>,
}

fn norm3(v: &[f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

/// Weighted normal equations -> QR of the stacked weighted rows.
pub fn solve_normal(m: &[[f64; 3]; 3], rhs: &[f64; 3]) -> SolveResult {
    // Gaussian elimination with partial pivoting on the symmetric matrix.
    let mut a = [
        [m[0][0], m[0][1], m[0][2], rhs[0]],
        [m[1][0], m[1][1], m[1][2], rhs[1]],
        [m[2][0], m[2][1], m[2][2], rhs[2]],
    ];
    let scale = a
        .iter()
        .flat_map(|r| r[..3].iter())
        .fold(1.0f64, |acc, &v| acc.max(v.abs()));
    let tol = scale * 1e-10;
    let mut pivot_col = [0usize, 1, 2];
    let mut rank = 0;
    let mut pivot_cols = Vec::new();

    for col in 0..3 {
        let mut best = rank;
        for i in (rank + 1)..3 {
            if a[i][col].abs() > a[best][col].abs() {
                best = i;
            }
        }
        if a[best][col].abs() <= tol {
            continue;
        }
        if best != rank {
            a.swap(best, rank);
            pivot_col.swap(best, rank);
        }
        let piv = a[rank][col];
        for i in (rank + 1)..3 {
            let f = a[i][col] / piv;
            if f.abs() < 1e-300 {
                continue;
            }
            for j in col..4 {
                a[i][j] -= f * a[rank][j];
            }
        }
        pivot_cols.push(col);
        rank += 1;
    }

    let mut xp = [0.0f64; 3];
    // back substitute using pivot rows; pivot_cols[k] gives the variable
    // solved by row k
    for k in (0..rank).rev() {
        let col = pivot_cols[k];
        let mut sum = a[k][3];
        for j in (col + 1)..3 {
            sum -= a[k][j] * xp[j];
        }
        xp[col] = sum / a[k][col];
    }
    let _ = pivot_col;

    let nullspace = if rank < 3 {
        nullspace_of(m, tol)
    } else {
        Vec::new()
    };
    SolveResult {
        solution: xp,
        rank,
        nullspace,
    }
}

fn nullspace_of(m: &[[f64; 3]; 3], tol: f64) -> Vec<[f64; 3]> {
    let rows = [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ];
    // collect independent rows via Gram-Schmidt
    let mut basis: Vec<[f64; 3]> = Vec::new();
    for r in rows {
        let mut v = r;
        for b in &basis {
            let dot = v[0] * b[0] + v[1] * b[1] + v[2] * b[2];
            v[0] -= dot * b[0];
            v[1] -= dot * b[1];
            v[2] -= dot * b[2];
        }
        if norm3(&v) > tol * 1e4 {
            let n = norm3(&v);
            basis.push([v[0] / n, v[1] / n, v[2] / n]);
        }
    }
    if basis.is_empty() {
        return vec![[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    }
    if basis.len() == 1 {
        let r = basis[0];
        let pivot = if r[0].abs() < r[1].abs() { 0 } else { 1 };
        let pivot = if r[pivot].abs() < r[2].abs() {
            pivot
        } else {
            2
        };
        let mut e = [0.0; 3];
        e[pivot] = 1.0;
        let dot = e[0] * r[0] + e[1] * r[1] + e[2] * r[2];
        let mut u = [e[0] - dot * r[0], e[1] - dot * r[1], e[2] - dot * r[2]];
        let n = norm3(&u);
        u = [u[0] / n, u[1] / n, u[2] / n];
        let v = cross(&r, &u);
        return vec![u, v];
    }
    // two independent rows: kernel is their cross product
    let n = cross(&basis[0], &basis[1]);
    vec![n]
}

fn cross(a: &[f64; 3], b: &[f64; 3]) -> [f64; 3] {
    let mut v = [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ];
    let n = norm3(&v).max(1e-300);
    v[0] /= n;
    v[1] /= n;
    v[2] /= n;
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solves_identity() {
        let r = solve_normal(
            &[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            &[2.0, 3.0, 4.0],
        );
        assert_eq!(r.rank, 3);
        assert!((r.solution[0] - 2.0).abs() < 1e-10);
        assert!((r.solution[1] - 3.0).abs() < 1e-10);
        assert!((r.solution[2] - 4.0).abs() < 1e-10);
    }

    #[test]
    fn detects_rank_deficiency() {
        let m = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [3.0, 6.0, 9.0]];
        let r = solve_normal(&m, &[1.0, 2.0, 3.0]);
        assert_eq!(r.rank, 1);
        assert_eq!(r.nullspace.len(), 2);
        for n in &r.nullspace {
            // M n = 0
            for i in 0..3 {
                let v = m[i][0] * n[0] + m[i][1] * n[1] + m[i][2] * n[2];
                assert!(v.abs() < 1e-9);
            }
        }
    }

    #[test]
    fn full_rank_geometry() {
        // rows x + d r0 style independent
        let m = [[10.0, 2.0, 3.0], [2.0, 12.0, 1.0], [3.0, 1.0, 8.0]];
        let rhs = [1.0, 2.0, 3.0];
        let r = solve_normal(&m, &rhs);
        assert_eq!(r.rank, 3);
        for i in 0..3 {
            let v = m[i][0] * r.solution[0] + m[i][1] * r.solution[1] + m[i][2] * r.solution[2];
            assert!((v - rhs[i]).abs() < 1e-9);
        }
    }
}
