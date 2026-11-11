//! Camera data and projection use stored coordinates; selecting up never verifies source axes.
use craft_anything_formats::scene_bundle::math::{self, Bounds, Matrix, Point};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Camera {
    pub target: Point,
    pub yaw: f64,
    pub pitch: f64,
    pub distance: f64,
    pub y_up: bool,
}
impl Default for Camera {
    fn default() -> Self {
        Self { target: [0.; 3], yaw: -0.8, pitch: 0.55, distance: 10., y_up: false }
    }
}
impl Camera {
    pub fn fit(&mut self, bounds: Bounds) {
        self.target = bounds.center();
        self.distance = (bounds.radius() * 3.5).clamp(1e-6, 1e14);
    }
    pub fn basis(&self) -> (Point, Point, Point, Point) {
        let (s, c) = self.yaw.sin_cos();
        let (p, q) = self.pitch.clamp(-1.5, 1.5).sin_cos();
        let offset = if self.y_up { [c * q, p, s * q] } else { [c * q, s * q, p] };
        let eye = math::add(self.target, math::scale(offset, self.distance));
        let forward = math::scale(offset, -1.);
        let up = if self.y_up { [0., 1., 0.] } else { [0., 0., 1.] };
        let right = math::normalized(math::cross(forward, up)).unwrap_or([1., 0., 0.]);
        let up = math::cross(right, forward);
        (eye, forward, right, up)
    }
    pub fn orbit(&mut self, delta: [f32; 2]) {
        self.yaw = (self.yaw - f64::from(delta[0]) * 0.008).rem_euclid(std::f64::consts::TAU);
        self.pitch = (self.pitch + f64::from(delta[1]) * 0.008).clamp(-1.5, 1.5);
    }
    pub fn pan(&mut self, delta: [f32; 2], height: f32) {
        let (_, _, right, up) = self.basis();
        let scale = self.distance * 0.9 / f64::from(height.max(1.));
        self.target = math::add(self.target, math::add(math::scale(right, -f64::from(delta[0]) * scale), math::scale(up, f64::from(delta[1]) * scale)));
    }
    pub fn zoom(&mut self, delta: f32) {
        self.distance = (self.distance * (-f64::from(delta) * 0.002).exp()).clamp(1e-6, 1e14);
    }
    pub fn ray(&self, ndc: [f64; 2], aspect: f64) -> (Point, Point) {
        let (eye, forward, right, up) = self.basis();
        let tan = (std::f64::consts::FRAC_PI_4 * 0.5).tan();
        let dir = math::add(forward, math::add(math::scale(right, ndc[0] * aspect * tan), math::scale(up, ndc[1] * tan)));
        (eye, math::normalized(dir).unwrap_or(forward))
    }
    pub fn projection(&self, origin: Point, aspect: f64, radius: f64) -> Result<Matrix, String> {
        if !math::finite(self.target)
            || !self.yaw.is_finite()
            || !self.pitch.is_finite()
            || !self.distance.is_finite()
            || self.distance <= 0.
            || !aspect.is_finite()
            || aspect <= 0.
        {
            return Err("Invalid camera state; fit the model again.".into());
        }
        let (eye, forward, right, up) = self.basis();
        let eye = math::sub(eye, origin);
        let view = [
            right[0],
            up[0],
            -forward[0],
            0.,
            right[1],
            up[1],
            -forward[1],
            0.,
            right[2],
            up[2],
            -forward[2],
            0.,
            -math::dot(right, eye),
            -math::dot(up, eye),
            math::dot(forward, eye),
            1.,
        ];
        let near = (self.distance * 0.0001).max(1e-9);
        let far = (self.distance + radius * 4.).max(near * 100.);
        let f = 1. / (std::f64::consts::FRAC_PI_4 * 0.5).tan();
        let proj = [f / aspect, 0., 0., 0., 0., f, 0., 0., 0., 0., far / (near - far), -1., 0., 0., far * near / (near - far), 0.];
        math::multiply(&proj, &view).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn center_ray_and_rebased_projection_agree() {
        for y_up in [false, true] {
            let mut c = Camera { y_up, ..Default::default() };
            c.fit(Bounds([[1e9, 2e9, -1.], [1e9 + 2., 2e9 + 4., 1.]]));
            let (eye, dir) = c.ray([0., 0.], 1.5);
            let towards = math::normalized(math::sub(c.target, eye)).unwrap();
            assert!(math::dot(towards, dir) > 0.999999);
            let m = c.projection(c.target, 1.5, 3.).unwrap();
            assert!(m[12].abs() < 1e-5 && m[13].abs() < 1e-5);
            let depth = m[14] / m[15];
            assert!((0. ..1.).contains(&depth));
        }
    }
}
