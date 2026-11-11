//! Affine scene math in stored coordinates. No unit or axis convention is inferred.
use super::{Error, Result};
pub type Point = [f64; 3];
pub type Matrix = [f64; 16];
pub const IDENTITY: Matrix = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
pub fn add(a: Point, b: Point) -> Point {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn sub(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn scale(a: Point, s: f64) -> Point {
    [a[0] * s, a[1] * s, a[2] * s]
}
pub fn dot(a: Point, b: Point) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn cross(a: Point, b: Point) -> Point {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
pub fn finite(a: Point) -> bool {
    a.iter().all(|v| v.is_finite())
}
pub fn normalized(a: Point) -> Result<Point> {
    let n = a[0].hypot(a[1]).hypot(a[2]);
    if !finite(a) || !n.is_finite() || n < 1e-30 {
        return Err(Error::Malformed("invalid scene direction".into()));
    }
    Ok(scale(a, 1. / n))
}
pub fn transform(m: &Matrix, p: Point) -> Point {
    [m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12], m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13], m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14]]
}
pub fn vector(m: &Matrix, p: Point) -> Point {
    [m[0] * p[0] + m[4] * p[1] + m[8] * p[2], m[1] * p[0] + m[5] * p[1] + m[9] * p[2], m[2] * p[0] + m[6] * p[1] + m[10] * p[2]]
}
pub fn affine(m: &Matrix) -> bool {
    m.iter().all(|v| v.is_finite()) && m[3] == 0. && m[7] == 0. && m[11] == 0. && m[15] == 1.
}
pub fn multiply(a: &Matrix, b: &Matrix) -> Result<Matrix> {
    let mut m = [0.; 16];
    for (i, v) in m.iter_mut().enumerate() {
        let c = i / 4;
        let r = i % 4;
        for k in 0..4 {
            *v += a.get(k * 4 + r).copied().ok_or_else(|| Error::Malformed("matrix index".into()))?
                * b.get(c * 4 + k).copied().ok_or_else(|| Error::Malformed("matrix index".into()))?;
        }
        if !v.is_finite() {
            return Err(Error::Malformed("scene transform overflows".into()));
        }
    }
    Ok(m)
}
pub fn inverse(m: &Matrix) -> Option<Matrix> {
    if !affine(m) {
        return None;
    }
    let a = [m[0], m[1], m[2]];
    let b = [m[4], m[5], m[6]];
    let c = [m[8], m[9], m[10]];
    let det = dot(a, cross(b, c));
    if !det.is_finite() || det.abs() < 1e-24 {
        return None;
    }
    let r0 = scale(cross(b, c), 1. / det);
    let r1 = scale(cross(c, a), 1. / det);
    let r2 = scale(cross(a, b), 1. / det);
    let t = [m[12], m[13], m[14]];
    let out = [r0[0], r1[0], r2[0], 0., r0[1], r1[1], r2[1], 0., r0[2], r1[2], r2[2], 0., -dot(r0, t), -dot(r1, t), -dot(r2, t), 1.];
    out.iter().all(|v| v.is_finite()).then_some(out)
}
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Bounds(pub [Point; 2]);
impl Bounds {
    pub fn empty() -> Self {
        Self([[f64::INFINITY; 3], [f64::NEG_INFINITY; 3]])
    }
    pub fn include(&mut self, p: Point) {
        for (axis, v) in p.iter().enumerate() {
            if let Some(lo) = self.0.get_mut(0).and_then(|x| x.get_mut(axis)) {
                *lo = lo.min(*v);
            }
            if let Some(hi) = self.0.get_mut(1).and_then(|x| x.get_mut(axis)) {
                *hi = hi.max(*v);
            }
        }
    }
    pub fn valid(self) -> bool {
        finite(self.0[0]) && finite(self.0[1]) && (0..3).all(|a| self.0[0].get(a) <= self.0[1].get(a))
    }
    pub fn center(self) -> Point {
        add(scale(self.0[0], 0.5), scale(self.0[1], 0.5))
    }
    pub fn radius(self) -> f64 {
        let d = sub(self.0[1], self.0[0]);
        d[0].hypot(d[1]).hypot(d[2]) * 0.5
    }
    pub fn transformed(self, m: &Matrix) -> Result<Self> {
        if !self.valid() || !affine(m) {
            return Err(Error::Malformed("non-affine scene bounds".into()));
        }
        let mut out = Self::empty();
        for bits in 0..8 {
            let mut p = [0.; 3];
            for (axis, v) in p.iter_mut().enumerate() {
                *v = self.0.get((bits >> axis) & 1).and_then(|r| r.get(axis)).copied().ok_or_else(|| Error::Malformed("bounds corner".into()))?;
            }
            let p = transform(m, p);
            if !finite(p) {
                return Err(Error::Malformed("scene world position overflows".into()));
            }
            out.include(p);
        }
        Ok(out)
    }
    pub fn ray(self, origin: Point, dir: Point) -> Option<f64> {
        let mut lo = 0.0f64;
        let mut hi = f64::INFINITY;
        for axis in 0..3 {
            let o = *origin.get(axis)?;
            let d = *dir.get(axis)?;
            let min = *self.0[0].get(axis)?;
            let max = *self.0[1].get(axis)?;
            if d.abs() < 1e-30 {
                if o < min || o > max {
                    return None;
                }
            } else {
                let a = (min - o) / d;
                let b = (max - o) / d;
                lo = lo.max(a.min(b));
                hi = hi.min(a.max(b));
                if hi < lo {
                    return None;
                }
            }
        }
        Some(lo)
    }
}
