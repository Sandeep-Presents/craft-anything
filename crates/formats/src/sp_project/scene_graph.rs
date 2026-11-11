use super::*;
use scene_geometry::{Mesh, Node};
#[derive(Serialize)]
pub(crate) struct Instance {
    pub instance_id: String,
    pub entity_node_guid: String,
    pub mesh_id: usize,
    pub path: Vec<String>,
    pub world_matrix: [f64; 16],
    pub conservative_world_bounds: [[f64; 3]; 2],
    pub resource_guids: Vec<String>,
}

pub(crate) fn multiply(a: &[f64; 16], b: &[f64; 16]) -> Result<[f64; 16]> {
    let mut out = [0.; 16];
    for (i, v) in out.iter_mut().enumerate() {
        let r = i % 4;
        let c = i / 4;
        for k in 0..4 {
            *v += a.get(k * 4 + r).copied().ok_or_else(|| fail("limit", "Matrix index"))?
                * b.get(c * 4 + k).copied().ok_or_else(|| fail("limit", "Matrix index"))?;
        }
        if !v.is_finite() {
            return Err(fail("invalid_geometry", "Composed transform overflow"));
        }
    }
    Ok(out)
}
pub(crate) fn point(m: &[f64; 16], p: &[f64; 3]) -> Result<[f64; 3]> {
    let mut q = [0.; 4];
    for (r, v) in q.iter_mut().enumerate() {
        for k in 0..4 {
            *v += m.get(k * 4 + r).copied().ok_or_else(|| fail("limit", "Point matrix index"))? * p.get(k).copied().unwrap_or(1.);
        }
    }
    let w = q.get(3).copied().ok_or_else(|| fail("limit", "Homogeneous index"))?;
    if !w.is_finite() || w.abs() < 1e-12 {
        return Err(fail("invalid_geometry", "Invalid homogeneous point"));
    }
    let mut out = [0.; 3];
    for (r, v) in out.iter_mut().enumerate() {
        *v = q.get(r).copied().ok_or_else(|| fail("limit", "Point index"))? / w;
        if !v.is_finite() {
            return Err(fail("invalid_geometry", "Transformed point overflow"));
        }
    }
    Ok(out)
}
fn bounds(m: &[f64; 16], b: &[[f64; 3]; 2]) -> Result<[[f64; 3]; 2]> {
    for k in [3, 7, 11] {
        if m.get(k).copied().unwrap_or(f64::NAN) != 0.0 {
            return Err(fail("unsupported_layout", "Perspective model transform unsupported"));
        }
    }
    if m.get(15).copied().unwrap_or(f64::NAN) != 1.0 {
        return Err(fail("unsupported_layout", "Non-affine model transform"));
    }
    let mut out = [[f64::INFINITY; 3], [f64::NEG_INFINITY; 3]];
    for bits in 0..8 {
        let mut p = [0.; 3];
        for (axis, v) in p.iter_mut().enumerate() {
            *v = b.get((bits >> axis) & 1).and_then(|r| r.get(axis)).copied().ok_or_else(|| fail("limit", "Bounds corner"))?;
        }
        let p = point(m, &p)?;
        for (axis, value) in p.iter().enumerate() {
            if let Some(v) = out.get_mut(0).and_then(|r| r.get_mut(axis)) {
                *v = v.min(*value);
            }
            if let Some(v) = out.get_mut(1).and_then(|r| r.get_mut(axis)) {
                *v = v.max(*value);
            }
        }
    }
    Ok(out)
}
fn acyclic<'a>(id: &'a str, nodes: &BTreeMap<&'a str, &'a Node>, active: &mut BTreeSet<&'a str>, done: &mut BTreeSet<&'a str>, depth: usize) -> Result<()> {
    if done.contains(id) {
        return Ok(());
    }
    if depth > 64 || !active.insert(id) {
        return Err(fail("invalid_geometry", "Scene reference cycle or combined depth exceeds64"));
    }
    let node = nodes.get(id).ok_or_else(|| fail("missing_dependency", "Scene node missing"))?;
    let mut targets = BTreeSet::new();
    for child in &node.children {
        if !targets.insert(child.as_str()) {
            return Err(fail("invalid_geometry", "Duplicate outgoing scene edge"));
        }
    }
    if let Some(target) = &node.reference
        && !targets.insert(target.as_str())
    {
        return Err(fail("invalid_geometry", "Parallel child/reference scene edges"));
    }
    for child in &node.children {
        acyclic(child, nodes, active, done, depth + 1)?;
    }
    if let Some(target) = &node.reference {
        acyclic(target, nodes, active, done, depth + 1)?;
    }
    active.remove(id);
    done.insert(id);
    Ok(())
}
struct Walker<'a> {
    nodes: BTreeMap<&'a str, &'a Node>,
    meshes: &'a [Mesh],
    direct: BTreeMap<&'a str, BTreeSet<String>>,
    instances: Vec<Instance>,
    path_items: usize,
    owner_items: usize,
    source_sha: &'a str,
    keep_going: &'a mut dyn FnMut(f32) -> bool,
}
impl Walker<'_> {
    fn walk(&mut self, id: &str, parent: &[f64; 16], path: &mut Vec<String>, owners: &BTreeSet<String>, entity: &str) -> Result<()> {
        if path.len() > 64 {
            return Err(fail("limit", "Scene instance traversal budget"));
        }
        check(self.keep_going, 0.65)?;
        let node = self.nodes.get(id).copied().ok_or_else(|| fail("missing_dependency", "Instance node missing"))?;
        let matrix = multiply(parent, &node.matrix)?;
        let mut owners = owners.clone();
        if let Some(local) = self.direct.get(id) {
            owners.extend(local.iter().cloned());
        }
        if owners.len() > 64 {
            return Err(fail("limit", "Overlapping Resource bindings budget"));
        }
        path.push(id.to_owned());
        let entity = if node.section == "Entities" { node.guid.as_str() } else { entity };
        if let Some(mesh_id) = node.mesh {
            if self.instances.len() >= 100_000 {
                return Err(fail("limit", "Scene instance count budget"));
            }
            self.path_items = checked_add(self.path_items, path.len())?;
            self.owner_items = checked_add(self.owner_items, owners.len())?;
            if self.path_items > 1_000_000 || self.owner_items > 1_000_000 {
                return Err(fail("limit", "Scene instance metadata budget"));
            }
            let mesh = self.meshes.get(mesh_id).ok_or_else(|| fail("missing_dependency", "Instance mesh missing"))?;
            let mut hash = Sha256::new();
            hash.update(self.source_sha.as_bytes());
            for item in path.iter() {
                hash.update(b"/");
                hash.update(item.as_bytes());
            }
            self.instances.push(Instance {
                instance_id: format!("{:x}", hash.finalize()),
                entity_node_guid: entity.into(),
                mesh_id,
                path: path.clone(),
                world_matrix: matrix,
                conservative_world_bounds: bounds(&matrix, &mesh.local_bounds)?,
                resource_guids: owners.iter().cloned().collect(),
            });
        }
        for child in &node.children {
            self.walk(child, &matrix, path, &owners, entity)?;
        }
        if let Some(target) = &node.reference {
            self.walk(target, &matrix, path, &owners, entity)?;
        }
        path.pop();
        Ok(())
    }
}
pub(crate) fn build(
    scene_nodes: &[Node],
    meshes: &[Mesh],
    roots: &BTreeMap<String, Vec<String>>,
    bindings: &[Binding],
    source_sha: &str,
    keep_going: &mut dyn FnMut(f32) -> bool,
) -> Result<Vec<Instance>> {
    let nodes = scene_nodes.iter().map(|n| (n.guid.as_str(), n)).collect::<BTreeMap<_, _>>();
    let mut done = BTreeSet::new();
    let mut active = BTreeSet::new();
    for id in nodes.keys() {
        acyclic(id, &nodes, &mut active, &mut done, 0)?;
    }
    let mut direct = BTreeMap::new();
    for b in bindings {
        direct.entry(b.node_guid.as_str()).or_insert_with(BTreeSet::new).insert(b.resource_guid.clone());
    }
    let mut walker = Walker { nodes, meshes, direct, instances: Vec::new(), path_items: 0, owner_items: 0, source_sha, keep_going };
    let roots = roots.get("Entities").ok_or_else(|| fail("missing_dependency", "Entities roots missing"))?;
    for root in roots {
        walker.walk(root, &scene_geometry::IDENTITY, &mut Vec::new(), &BTreeSet::new(), root)?;
    }
    Ok(walker.instances)
}
