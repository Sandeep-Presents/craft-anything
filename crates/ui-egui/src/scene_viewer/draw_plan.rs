//! Pure conservative draw selection. AABB tests may keep an invisible box, but never replace
//! geometry with a sample or silently remove triangles to meet a rendering budget.
use super::camera::Camera;
use craft_anything_formats::scene_bundle::{
    Instance, Mesh, Scene, Visibility,
    math::{Bounds, Matrix, Point},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const DEFAULT_TRIANGLE_BUDGET: u64 = 50_000_000;
pub const MAX_TRIANGLE_BUDGET: u64 = 1_000_000_000;
const MAX_INSTANCES: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawStats {
    pub total_instances: u64,
    pub visible: u64,
    pub hidden: u64,
    pub frustum_culled: u64,
    /// Mesh triangle count multiplied by the number of surviving instances.
    pub expanded_triangles: u64,
    pub max_triangles: u64,
}

#[derive(Debug, Clone)]
pub struct DrawPlan {
    pub visible_instances: BTreeSet<usize>,
    pub stats: DrawStats,
}

pub fn plan(scene: &Scene, camera: &Camera, filters: &Visibility, aspect: f64, max_triangles: u64) -> Result<DrawPlan, String> {
    plan_with_cancel(scene, camera, filters, aspect, max_triangles, || false)
}

/// `cancelled` is checked every 256 instances and at completion; it must be nonblocking.
pub fn plan_with_cancel(
    scene: &Scene,
    camera: &Camera,
    filters: &Visibility,
    aspect: f64,
    max_triangles: u64,
    mut cancelled: impl FnMut() -> bool,
) -> Result<DrawPlan, String> {
    if max_triangles > MAX_TRIANGLE_BUDGET {
        return Err(format!("The draw budget exceeds {MAX_TRIANGLE_BUDGET} triangles. Choose a budget at or below that limit."));
    }
    if scene.instances.len() > MAX_INSTANCES {
        return Err("The scene exceeds 100,000 instances. Split the scene into bounded bundles.".into());
    }
    if !scene.bounds.valid() || !scene.render_origin.iter().all(|v| v.is_finite()) || !scene.bounds.radius().is_finite() {
        return Err("The scene has invalid or overflowing bounds. Validate the scene bundle before drawing.".into());
    }
    let matrix = camera.projection(scene.render_origin, aspect, scene.bounds.radius())?;
    let total = u64::try_from(scene.instances.len()).map_err(|_| "The instance count cannot be represented.".to_string())?;
    let mut stats = DrawStats { total_instances: total, visible: 0, hidden: 0, frustum_culled: 0, expanded_triangles: 0, max_triangles };
    let mut visible_instances = BTreeSet::new();
    for (index, instance) in scene.instances.iter().enumerate() {
        if index % 256 == 0 && cancelled() {
            return Err("Draw planning cancelled; the previous frame remains available.".into());
        }
        if !scene.is_visible(index, filters) {
            stats.hidden = increment(stats.hidden)?;
            continue;
        }
        let mesh = scene.meshes.get(instance.mesh).ok_or("A visible instance references a missing mesh. Validate the scene bundle before drawing.")?;
        let intersects = match quantization_margin(instance, mesh, scene.render_origin) {
            Some(margin) => intersects_clip_with_margin(instance.bounds, scene.render_origin, &matrix, margin)?,
            None => true,
        };
        if !intersects {
            stats.frustum_culled = increment(stats.frustum_culled)?;
            continue;
        }
        let triangles = u64::try_from(mesh.triangle_count).map_err(|_| "A mesh triangle count cannot be represented.".to_string())?;
        stats.expanded_triangles =
            stats.expanded_triangles.checked_add(triangles).ok_or("The expanded triangle count overflows. Split or filter the scene.")?;
        if stats.expanded_triangles > max_triangles {
            return Err(format!(
                "The visible scene needs more than {max_triangles} triangles (at least {} counted). Hide or isolate resources, move the camera, or raise the draw budget up to {MAX_TRIANGLE_BUDGET}. No partial draw plan was produced.",
                stats.expanded_triangles
            ));
        }
        stats.visible = increment(stats.visible)?;
        visible_instances.insert(index);
    }
    if cancelled() {
        return Err("Draw planning cancelled; the previous frame remains available.".into());
    }
    Ok(DrawPlan { visible_instances, stats })
}

fn increment(value: u64) -> Result<u64, String> {
    value.checked_add(1).ok_or_else(|| "The instance count overflows. Split the scene into smaller bundles.".into())
}

/// Homogeneous WebGPU clip volume: -w <= x,y <= w and 0 <= z <= w.
/// Reject only when all eight corners are definitely outside the same plane. No divide by w:
/// boxes crossing the eye or near plane, and boxes enclosing the frustum, stay conservative.
/// Numerical uncertainty (overflow or a value within its error allowance) keeps the box.
// Match the renderer's recentered vertex/instance packing. The factor allows the f32
// vertex and matrix casts, products, and additions, including cancellation of large terms.
// Unknown/overflowing error bounds must retain the instance rather than reject it.
fn quantization_margin(instance: &Instance, mesh: &Mesh, origin: Point) -> Option<Point> {
    let mut local = [0.0f64; 3];
    for (axis, extent) in local.iter_mut().enumerate() {
        let center = *mesh.origin.get(axis)?;
        *extent = (mesh.bounds.0.first()?.get(axis)? - center).abs().max((mesh.bounds.0.get(1)?.get(axis)? - center).abs());
    }
    let mut margin = [0.0f64; 3];
    for (axis, error) in margin.iter_mut().enumerate() {
        let mut local_magnitude = 0.0;
        let mut origin_magnitude = 0.0;
        let mut shift = *instance.matrix.get(12 + axis)?;
        for (column, extent) in local.iter().enumerate() {
            let coefficient = *instance.matrix.get(column * 4 + axis)?;
            local_magnitude += coefficient.abs() * extent;
            let term = coefficient * mesh.origin.get(column)?;
            origin_magnitude += term.abs();
            shift += term;
        }
        let rebase = *origin.get(axis)?;
        shift -= rebase;
        *error = 64.0 * f64::from(f32::EPSILON) * (local_magnitude + shift.abs() + 1.0)
            + 64.0 * f64::EPSILON * (origin_magnitude + instance.matrix.get(12 + axis)?.abs() + rebase.abs() + 1.0);
        if !error.is_finite() || *error < 0.0 {
            return None;
        }
    }
    Some(margin)
}
#[cfg(test)]
fn intersects_clip(bounds: Bounds, origin: Point, matrix: &Matrix) -> Result<bool, String> {
    intersects_clip_with_margin(bounds, origin, matrix, [0.; 3])
}
fn intersects_clip_with_margin(bounds: Bounds, origin: Point, matrix: &Matrix, margin: Point) -> Result<bool, String> {
    if !bounds.valid() || !origin.iter().all(|v| v.is_finite()) || !matrix.iter().all(|v| v.is_finite()) {
        return Err("Invalid world bounds or projection. Fit the model again or validate the bundle.".into());
    }
    if margin.iter().any(|v| !v.is_finite() || *v < 0.0) {
        return Ok(true);
    }
    let mut all_outside = [true; 6];
    for bits in 0..8 {
        let mut point = [0.; 4];
        let mut rebase_error = [0.; 4];
        for (axis, value) in point.iter_mut().take(3).enumerate() {
            let source = bounds.0.get((bits >> axis) & 1).and_then(|p| p.get(axis)).copied().ok_or("Invalid bounds corner")?;
            let offset = origin.get(axis).copied().ok_or("Invalid render origin")?;
            let expansion = *margin.get(axis).ok_or("Invalid quantization margin")?;
            *value = source - offset + if (bits >> axis) & 1 == 0 { -expansion } else { expansion };
            if let Some(error) = rebase_error.get_mut(axis) {
                *error = f64::EPSILON * (source.abs() + offset.abs());
            }
        }
        if let Some(w) = point.get_mut(3) {
            *w = 1.;
        }
        let mut clip = [0.; 4];
        let mut error = [0.; 4];
        for (row, value) in clip.iter_mut().enumerate() {
            let mut magnitude = 0.;
            let mut input_error = 0.;
            for (column, coordinate) in point.iter().enumerate() {
                let coefficient = matrix.get(column * 4 + row).copied().ok_or("Invalid projection matrix")?;
                let term = coefficient * coordinate;
                *value += term;
                magnitude += term.abs();
                input_error += coefficient.abs() * rebase_error.get(column).copied().ok_or("Invalid coordinate error")?;
            }
            if let Some(allowance) = error.get_mut(row) {
                *allowance = 64. * f64::from(f32::EPSILON) * (magnitude + 1.) + input_error;
            }
        }
        if clip.iter().chain(error.iter()).any(|v| !v.is_finite()) {
            return Ok(true);
        }
        let (x, y, z, w) = match clip.as_slice() {
            [x, y, z, w] => (*x, *y, *z, *w),
            _ => return Err("Invalid clip coordinates".into()),
        };
        let (ex, ey, ez, ew) = match error.as_slice() {
            [x, y, z, w] => (*x, *y, *z, *w),
            _ => return Err("Invalid clip error".into()),
        };
        let distances = [x + w, w - x, y + w, w - y, z, w - z];
        let allowances = [ex + ew, ex + ew, ey + ew, ey + ew, ez, ez + ew];
        for ((outside, distance), allowance) in all_outside.iter_mut().zip(distances).zip(allowances) {
            if !distance.is_finite() || !allowance.is_finite() {
                return Ok(true);
            }
            *outside &= distance < -allowance;
        }
    }
    Ok(!all_outside.iter().any(|outside| *outside))
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_anything_formats::scene_bundle::math::IDENTITY;
    fn box_at(min: Point, max: Point) -> Bounds {
        Bounds([min, max])
    }
    fn hit(bounds: Bounds) -> bool {
        intersects_clip(bounds, [0.; 3], &IDENTITY).unwrap()
    }
    #[test]
    fn identity_clip_rejects_each_of_the_six_planes() {
        assert!(hit(box_at([-0.5, -0.5, 0.2], [0.5, 0.5, 0.8])));
        for bounds in [
            box_at([-3., 0., 0.2], [-2., 0.2, 0.8]),
            box_at([2., 0., 0.2], [3., 0.2, 0.8]),
            box_at([0., -3., 0.2], [0.2, -2., 0.8]),
            box_at([0., 2., 0.2], [0.2, 3., 0.8]),
            box_at([0., 0., -3.], [0.2, 0.2, -2.]),
            box_at([0., 0., 2.], [0.2, 0.2, 3.]),
        ] {
            assert!(!hit(bounds));
        }
    }
    #[test]
    fn near_far_and_side_boundaries_are_inclusive_and_straddling_is_retained() {
        assert!(hit(box_at([-1., -1., 0.], [1., 1., 1.])));
        assert!(hit(box_at([0., 0., -1.], [0.1, 0.1, 0.1])));
        assert!(hit(box_at([0., 0., 0.9], [0.1, 0.1, 2.])));
        assert!(hit(box_at([-2., -2., -2.], [2., 2., 2.])));
        assert!(hit(box_at([-1e100; 3], [1e100; 3])));
    }
    #[test]
    fn roundoff_and_overflow_are_conservative() {
        assert!(hit(box_at([1. + f64::EPSILON, 0., 0.2], [1. + f64::EPSILON, 0.1, 0.4])));
        let mut matrix = IDENTITY;
        matrix[0] = f64::MAX;
        assert!(intersects_clip(box_at([2., 0., 0.2], [3., 0.1, 0.4]), [0.; 3], &matrix).unwrap());
        assert!(intersects_clip(box_at([0.; 3], [1.; 3]), [f64::NAN, 0., 0.], &IDENTITY).is_err());
        assert!(intersects_clip(box_at([1.; 3], [0.; 3]), [0.; 3], &IDENTITY).is_err());
    }
    #[test]
    fn perspective_handles_behind_eye_near_crossing_and_enclosing_boxes() {
        let camera = Camera { target: [0.; 3], yaw: 0., pitch: 0., distance: 10., y_up: false };
        let matrix = camera.projection([0.; 3], 1., 20.).unwrap();
        assert!(intersects_clip(box_at([-1.; 3], [1.; 3]), [0.; 3], &matrix).unwrap());
        assert!(!intersects_clip(box_at([11., -1., -1.], [12., 1., 1.]), [0.; 3], &matrix).unwrap());
        assert!(intersects_clip(box_at([9., -1., -1.], [11., 1., 1.]), [0.; 3], &matrix).unwrap());
        assert!(intersects_clip(box_at([-1e20; 3], [1e20; 3]), [0.; 3], &matrix).unwrap());
    }
    #[test]
    fn rebasing_preserves_projection_at_large_world_offsets_for_both_up_axes() {
        for y_up in [true, false] {
            let origin = [1e12, -2e12, 3e12];
            let camera = Camera { target: origin, yaw: 0.2, pitch: 0.4, distance: 10., y_up };
            let bounds = box_at([origin[0] - 1., origin[1] - 1., origin[2] - 1.], [origin[0] + 1., origin[1] + 1., origin[2] + 1.]);
            assert!(intersects_clip(bounds, origin, &camera.projection(origin, 1.5, 2.).unwrap()).unwrap());
        }
    }
    fn scene() -> Scene {
        let bounds = box_at([-1.; 3], [1.; 3]);
        let instance = |name: &str, bounds| Instance {
            id: name.into(),
            entity_guid: String::new(),
            mesh: 0,
            matrix: IDENTITY,
            inverse: Some(IDENTITY),
            bounds,
            resources: Vec::new(),
        };
        Scene {
            id: "synthetic".into(),
            source_sha256: String::new(),
            unit_declaration: String::new(),
            axis_declaration: String::new(),
            resources: Vec::new(),
            resource_instance_counts: Vec::new(),
            meshes: vec![Mesh { vertex_start: 0, index_start: 0, vertex_count: 3, triangle_count: 7, origin: [0.; 3], bounds }],
            instances: vec![instance("a", bounds), instance("b", bounds), instance("outside", box_at([11., -1., -1.], [12., 1., 1.]))],
            positions: Vec::new(),
            gpu_positions: Vec::new(),
            gpu_indices: Vec::new(),
            bounds,
            render_origin: [0.; 3],
            triangles: 7,
            node_count: 3,
            source_colors: Default::default(),
            object_colors: Default::default(),
        }
    }
    fn camera() -> Camera {
        Camera { target: [0.; 3], yaw: 0., pitch: 0., distance: 10., y_up: false }
    }
    #[test]
    fn instance_counts_and_expanded_triangles_partition_the_scene() {
        let scene = scene();
        let filters = Visibility { hidden_instances: BTreeSet::from([1]), ..Default::default() };
        let out = plan(&scene, &camera(), &filters, 1., 7).unwrap();
        assert_eq!(out.visible_instances, BTreeSet::from([0]));
        assert_eq!((out.stats.visible, out.stats.hidden, out.stats.frustum_culled, out.stats.expanded_triangles), (1, 1, 1, 7));
        assert_eq!(out.stats.visible + out.stats.hidden + out.stats.frustum_culled, out.stats.total_instances);
    }
    #[test]
    fn budgets_fail_without_truncation_and_filters_can_make_the_plan_fit() {
        let scene = scene();
        let error = plan(&scene, &camera(), &Visibility::default(), 1., 13).unwrap_err();
        assert!(error.contains("No partial draw plan"));
        assert_eq!(plan(&scene, &camera(), &Visibility::default(), 1., 14).unwrap().stats.expanded_triangles, 14);
        assert!(plan(&scene, &camera(), &Visibility::default(), 1., MAX_TRIANGLE_BUDGET + 1).is_err());
        let filters = Visibility { hidden_instances: BTreeSet::from([0, 1, 2]), ..Default::default() };
        assert_eq!(plan(&scene, &camera(), &filters, 1., 0).unwrap().stats.visible, 0);
    }
    #[test]
    fn invalid_camera_missing_mesh_and_cancellation_are_actionable() {
        let mut scene = scene();
        assert!(plan(&scene, &camera(), &Visibility::default(), 0., 50).is_err());
        assert!(plan_with_cancel(&scene, &camera(), &Visibility::default(), 1., 50, || true).unwrap_err().contains("cancelled"));
        scene.meshes.clear();
        assert!(plan(&scene, &camera(), &Visibility::default(), 1., 50).unwrap_err().contains("missing mesh"));
    }

    #[test]
    fn projection_quantization_cancellation_retains_gpu_inside_points() {
        let mut matrix = IDENTITY;
        matrix[12] = -100_000_000.;
        let bounds = box_at([100_000_001.25, -0.1, 0.4], [100_000_001.5, 0.1, 0.6]);
        // f64 says x>1, but packing both operands to f32 cancels them to x=0.
        assert!(bounds.0[0][0] + matrix[12] > 1.);
        assert_eq!(bounds.0[0][0] as f32 + matrix[12] as f32, 0.);
        assert!(intersects_clip(bounds, [0.; 3], &matrix).unwrap());
    }
    #[test]
    fn world_margin_encloses_packed_affine_vertices_with_large_origins() {
        let mut scene = scene();
        let instance = &mut scene.instances[0];
        let mesh = &mut scene.meshes[0];
        mesh.origin = [1e12, -2e12, 3e12];
        mesh.bounds = box_at([1e12 - 1000., -2e12 - 2000., 3e12 - 3000.], [1e12 + 1000., -2e12 + 2000., 3e12 + 3000.]);
        instance.matrix = [1.00000006, 0.2, -0.3, 0., -0.99999994, 1.00000006, 0.4, 0., 0.001, 0.002, 1.00000006, 0., 1e12, -2e12, 3e12, 1.];
        let origin = craft_anything_formats::scene_bundle::math::transform(&instance.matrix, mesh.origin);
        let margin = quantization_margin(instance, mesh, origin).unwrap();
        let shift =
            craft_anything_formats::scene_bundle::math::sub(craft_anything_formats::scene_bundle::math::transform(&instance.matrix, mesh.origin), origin);
        for bits in 0..8 {
            let point = std::array::from_fn(|axis| mesh.bounds.0[(bits >> axis) & 1][axis]);
            let local: [f32; 3] = std::array::from_fn(|axis| (point[axis] - mesh.origin[axis]) as f32);
            let exact = craft_anything_formats::scene_bundle::math::sub(craft_anything_formats::scene_bundle::math::transform(&instance.matrix, point), origin);
            for axis in 0..3 {
                let packed = instance.matrix[axis] as f32 * local[0]
                    + instance.matrix[4 + axis] as f32 * local[1]
                    + instance.matrix[8 + axis] as f32 * local[2]
                    + shift[axis] as f32;
                assert!((f64::from(packed) - exact[axis]).abs() <= margin[axis], "axis{axis} margin{}", margin[axis]);
            }
        }
    }
    #[test]
    fn overflowing_quantization_margin_keeps_instance_uncertain() {
        let mut scene = scene();
        scene.instances[0].matrix[0] = f64::MAX;
        scene.meshes[0].bounds = box_at([-f64::MAX; 3], [f64::MAX; 3]);
        assert!(quantization_margin(&scene.instances[0], &scene.meshes[0], [0.; 3]).is_none());
        assert!(intersects_clip_with_margin(box_at([20., 0., 0.], [21., 1., 1.]), [0.; 3], &IDENTITY, [f64::INFINITY, 0., 0.]).unwrap());
    }
}
