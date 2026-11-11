//! A bounded reader for the original version-1 scene bundle and its packed geometry.
//! File access is owned by callers. Units, axes, materials and native parity remain unverified.
pub mod math;
use crate::FormatError as Error;
use math::{Bounds, Matrix, Point};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub type Result<T> = std::result::Result<T, Error>;
pub const MAX_TOTAL_BYTES: usize = 384 * 1024 * 1024;
pub const MAX_BINARY_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_NODES: usize = 100_000;
const MAX_INSTANCES: usize = 100_000;
const MAX_VERTICES: usize = 10_000_000;
const MAX_TRIANGLES: usize = 8_000_000;
/// Fixed member names are data, not paths supplied by a bundle.
pub const MEMBERS: [(&str, usize); 7] = [
    ("scene.json", 1024 * 1024),
    ("nodes.jsonl", MAX_METADATA_BYTES),
    ("meshes.jsonl", MAX_METADATA_BYTES),
    ("instances.jsonl", MAX_METADATA_BYTES),
    ("resource-bindings.jsonl", MAX_METADATA_BYTES),
    ("project.json", MAX_METADATA_BYTES),
    ("geometry.bin", MAX_BINARY_BYTES),
];
#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    pub manifest: &'a [u8],
    pub nodes: &'a [u8],
    pub meshes: &'a [u8],
    pub instances: &'a [u8],
    pub bindings: &'a [u8],
    pub project: &'a [u8],
    pub binary: &'a [u8],
}
#[derive(Debug, Deserialize)]
pub struct Layout {
    pub nodes: String,
    pub meshes: String,
    pub instances: String,
    pub resource_bindings: String,
    pub project: String,
    pub binary: String,
    pub positions: String,
    pub triangle_indices: String,
}
#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub schema_version: u32,
    pub status: String,
    pub source_sha256: String,
    pub entities: usize,
    pub library_nodes: usize,
    pub meshes: usize,
    pub vertices: usize,
    pub triangles: usize,
    pub instances: usize,
    pub direct_resource_bindings: usize,
    pub binary_bytes: usize,
    pub coordinate_unit: String,
    pub world_axis_convention: String,
    pub artifact_layout: Layout,
}
#[derive(Debug, Deserialize)]
struct StoredNode {
    guid: String,
    parent_guid: Option<String>,
    section: String,
    name: String,
    children: Vec<String>,
    mesh: Option<usize>,
    reference: Option<String>,
    matrix: Matrix,
}
#[derive(Debug, Deserialize)]
struct StoredMesh {
    owner: String,
    vertex_count: usize,
    triangle_count: usize,
    vertex_byte_offset: usize,
    index_byte_offset: usize,
    source_precision_bits: u32,
    local_bounds: [Point; 2],
}
#[derive(Debug, Deserialize)]
struct StoredInstance {
    instance_id: String,
    entity_node_guid: String,
    mesh_id: usize,
    path: Vec<String>,
    world_matrix: Matrix,
    conservative_world_bounds: [Point; 2],
    resource_guids: Vec<String>,
}
#[derive(Debug, Deserialize)]
struct Binding {
    resource_guid: String,
    node_guid: String,
    source_section: String,
    binding_kind: String,
}
#[derive(Debug, Deserialize)]
struct Project {
    schema_version: u32,
    resources: Option<Vec<Resource>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    pub guid: String,
    pub name: String,
    pub parent_guid: Option<String>,
}
#[derive(Debug)]
pub struct Mesh {
    pub vertex_start: usize,
    pub index_start: usize,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub origin: Point,
    pub bounds: Bounds,
}
#[derive(Debug)]
pub struct Instance {
    pub id: String,
    pub entity_guid: String,
    pub mesh: usize,
    pub matrix: Matrix,
    pub inverse: Option<Matrix>,
    pub bounds: Bounds,
    pub resources: Vec<usize>,
}
#[derive(Debug)]
pub struct Scene {
    pub id: String,
    pub source_sha256: String,
    pub unit_declaration: String,
    pub axis_declaration: String,
    pub resources: Vec<Resource>,
    pub resource_instance_counts: Vec<usize>,
    pub meshes: Vec<Mesh>,
    pub instances: Vec<Instance>,
    pub positions: Vec<Point>,
    pub gpu_positions: Vec<u8>,
    pub gpu_indices: Vec<u8>,
    pub bounds: Bounds,
    pub render_origin: Point,
    pub triangles: usize,
    pub node_count: usize,
}
fn malformed(s: impl Into<String>) -> Error {
    Error::Malformed(s.into())
}
fn over(s: &str) -> Error {
    Error::TooLarge(s.into())
}
fn plus(a: usize, b: usize) -> Result<usize> {
    a.checked_add(b).ok_or_else(|| over("scene offset arithmetic"))
}
fn times(a: usize, b: usize) -> Result<usize> {
    a.checked_mul(b).ok_or_else(|| over("scene length arithmetic"))
}
fn range(data: &[u8], at: usize, n: usize) -> Result<&[u8]> {
    data.get(at..plus(at, n)?).ok_or_else(|| Error::Truncated("scene geometry range".into()))
}
fn number(data: &[u8], at: usize) -> Result<f64> {
    let v = f64::from_le_bytes(range(data, at, 8)?.try_into().map_err(|_| malformed("scene coordinate"))?);
    if !v.is_finite() {
        return Err(malformed("nonfinite scene position"));
    }
    Ok(v)
}
fn index(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(range(data, at, 4)?.try_into().map_err(|_| malformed("scene index"))?))
}
fn guid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_hexdigit() })
        && s.bytes().any(|b| b != b'0' && b != b'-')
}
fn json<T: DeserializeOwned>(data: &[u8]) -> Result<T> {
    serde_json::from_slice(data).map_err(|e| malformed(format!("scene JSON: {e}")))
}
fn records<T: DeserializeOwned>(data: &[u8], cap: usize) -> Result<Vec<T>> {
    let mut rows = Vec::new();
    for line in data.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        if line.len() > MAX_RECORD_BYTES || rows.len() >= cap {
            return Err(over("scene record budget"));
        }
        rows.push(json(line)?);
    }
    Ok(rows)
}
fn reserve<T>(v: &mut Vec<T>, n: usize) -> Result<()> {
    v.try_reserve(n).map_err(|_| over("scene allocation"))
}
pub fn parse(input: Inputs<'_>, mut keep_going: impl FnMut(f32) -> bool) -> Result<Scene> {
    let data = [input.manifest, input.nodes, input.meshes, input.instances, input.bindings, input.project, input.binary];
    let mut total = 0usize;
    for (bytes, (_, cap)) in data.iter().zip(MEMBERS) {
        if bytes.len() > cap {
            return Err(over("scene member size"));
        }
        total = plus(total, bytes.len())?;
    }
    if total > MAX_TOTAL_BYTES {
        return Err(over("scene total input"));
    }
    if !keep_going(0.) {
        return Err(Error::Unsupported("scene parsing cancelled".into()));
    }
    let m: Manifest = json(input.manifest)?;
    if m.schema_version != 1 || m.status != "experimental_scene_partial" {
        return Err(Error::Unsupported("scene bundle version/status".into()));
    }
    let l = &m.artifact_layout;
    if [l.nodes.as_str(), l.meshes.as_str(), l.instances.as_str(), l.resource_bindings.as_str(), l.project.as_str(), l.binary.as_str()]
        != ["nodes.jsonl", "meshes.jsonl", "instances.jsonl", "resource-bindings.jsonl", "project.json", "geometry.bin"]
        || l.positions != "little-endian f64 xyz, 24 bytes per vertex"
        || l.triangle_indices != "little-endian u32 abc, 12 bytes per triangle"
    {
        return Err(Error::Unsupported("scene member layout/encoding".into()));
    }
    if m.binary_bytes != input.binary.len()
        || m.vertices > MAX_VERTICES
        || m.triangles > MAX_TRIANGLES
        || m.instances > MAX_INSTANCES
        || plus(m.entities, m.library_nodes)? > MAX_NODES
        || m.meshes > MAX_NODES
        || m.direct_resource_bindings > 200_000
    {
        return Err(over("scene declared counts"));
    }
    if m.source_sha256.len() != 64
        || !m.source_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || m.coordinate_unit.len() > 256
        || m.world_axis_convention.len() > 256
    {
        return Err(malformed("scene declarations"));
    }
    let project: Project = json(input.project)?;
    if project.schema_version != 4 {
        return Err(Error::Unsupported("scene project metadata version".into()));
    }
    let resources = project.resources.ok_or_else(|| malformed("scene resource metadata is absent"))?;
    if resources.len() > 10_000 {
        return Err(over("scene resources"));
    }
    let mut resource_ids = BTreeMap::new();
    for (i, r) in resources.iter().enumerate() {
        if !guid(&r.guid) || r.name.len() > 65536 || resource_ids.insert(r.guid.as_str(), i).is_some() {
            return Err(malformed("invalid/duplicate scene Resource"));
        }
    }
    for r in &resources {
        if let Some(parent) = &r.parent_guid
            && (parent == &r.guid || !resource_ids.contains_key(parent.as_str()))
        {
            return Err(malformed("scene Resource parent"));
        }
    }
    for r in &resources {
        let mut cursor = r.parent_guid.as_deref();
        let mut seen = BTreeSet::new();
        seen.insert(r.guid.as_str());
        while let Some(id) = cursor {
            if !seen.insert(id) || seen.len() > 65 {
                return Err(malformed("Resource hierarchy cycle/depth"));
            }
            cursor = resource_ids.get(id).and_then(|i| resources.get(*i)).and_then(|r| r.parent_guid.as_deref());
        }
    }
    let nodes: Vec<StoredNode> = records(input.nodes, MAX_NODES)?;
    if nodes.len() != plus(m.entities, m.library_nodes)? {
        return Err(malformed("scene node count"));
    }
    let mut ids = BTreeMap::new();
    for (i, n) in nodes.iter().enumerate() {
        if !guid(&n.guid)
            || !math::affine(&n.matrix)
            || n.name.len() > 65536
            || ids.insert(n.guid.as_str(), i).is_some()
            || !matches!(n.section.as_str(), "Entities" | "IncludeLibrary")
        {
            return Err(malformed("scene node identity/transform"));
        }
    }
    if nodes.iter().filter(|n| n.section == "Entities").count() != m.entities {
        return Err(malformed("scene section counts"));
    }
    let edge_count = nodes.iter().try_fold(0usize, |total, n| plus(total, plus(n.children.len(), usize::from(n.reference.is_some()))?))?;
    if edge_count > 500_000 {
        return Err(over("scene graph edges"));
    }
    let child_edges = nodes.iter().flat_map(|n| n.children.iter().map(move |c| (n.guid.as_str(), c.as_str()))).collect::<BTreeSet<_>>();
    let mut named = 0usize;
    for n in &nodes {
        named = plus(named, n.name.len())?;
        if named > 16 * 1024 * 1024 {
            return Err(over("scene names"));
        }
        if let Some(parent) = &n.parent_guid {
            let parent = ids.get(parent.as_str()).and_then(|i| nodes.get(*i)).ok_or_else(|| malformed("missing scene parent"))?;
            if parent.section != n.section || !child_edges.contains(&(parent.guid.as_str(), n.guid.as_str())) {
                return Err(malformed("inconsistent reverse scene parent"));
            }
        }
        let mut cursor = n.parent_guid.as_deref();
        let mut seen = BTreeSet::new();
        seen.insert(n.guid.as_str());
        while let Some(id) = cursor {
            if !seen.insert(id) || seen.len() > 65 {
                return Err(malformed("node hierarchy cycle/depth"));
            }
            cursor = ids.get(id).and_then(|i| nodes.get(*i)).and_then(|n| n.parent_guid.as_deref());
        }
        let mut children = BTreeSet::new();
        for id in &n.children {
            if !children.insert(id.as_str()) {
                return Err(malformed("duplicate scene child"));
            }
            let child = ids.get(id.as_str()).and_then(|i| nodes.get(*i)).ok_or_else(|| malformed("missing scene child"))?;
            if child.parent_guid.as_ref() != Some(&n.guid) {
                return Err(malformed("inconsistent scene parent"));
            }
        }
        if let Some(id) = &n.reference
            && (children.contains(id.as_str()) || !ids.contains_key(id.as_str()))
        {
            return Err(malformed("invalid/parallel scene reference"));
        }
    }
    // Iterative three-colour traversal also checks unused reference subgraphs.
    let mut colors = vec![0u8; nodes.len()];
    for start in 0..nodes.len() {
        if colors.get(start) != Some(&0) {
            continue;
        }
        let mut stack = vec![(start, false)];
        while let Some((id, exit)) = stack.pop() {
            if !keep_going(0.08) {
                return Err(Error::Unsupported("scene parsing cancelled".into()));
            }
            let color = colors.get_mut(id).ok_or_else(|| malformed("scene graph index"))?;
            if exit {
                *color = 2;
                continue;
            }
            if *color == 1 {
                return Err(malformed("scene reference graph cycle"));
            }
            if *color == 2 {
                continue;
            }
            *color = 1;
            stack.push((id, true));
            let node = nodes.get(id).ok_or_else(|| malformed("scene graph node"))?;
            for child in node.children.iter().chain(node.reference.iter()) {
                let next = *ids.get(child.as_str()).ok_or_else(|| malformed("scene graph target"))?;
                stack.push((next, false));
            }
        }
    }
    let bindings: Vec<Binding> = records(input.bindings, 200_000)?;
    if bindings.len() != m.direct_resource_bindings {
        return Err(malformed("scene binding count"));
    }
    let mut binding_ids = BTreeSet::new();
    let mut direct = BTreeMap::<&str, BTreeSet<usize>>::new();
    for b in &bindings {
        if !binding_ids.insert((&b.resource_guid, &b.node_guid)) {
            return Err(malformed("duplicate direct resource binding"));
        }
        let r = *resource_ids.get(b.resource_guid.as_str()).ok_or_else(|| malformed("binding Resource missing"))?;
        let node = ids.get(b.node_guid.as_str()).and_then(|i| nodes.get(*i)).ok_or_else(|| malformed("binding node missing"))?;
        if node.section != "Entities" || b.source_section != "Resources" || b.binding_kind != "direct_subtree" {
            return Err(Error::Unsupported("scene binding kind".into()));
        }
        direct.entry(b.node_guid.as_str()).or_default().insert(r);
    }
    let stored_meshes: Vec<StoredMesh> = records(input.meshes, MAX_NODES)?;
    if stored_meshes.len() != m.meshes {
        return Err(malformed("scene mesh count"));
    }
    let mut meshes = Vec::new();
    let mut positions = Vec::new();
    let mut gpu_positions = Vec::new();
    let mut gpu_indices = Vec::new();
    reserve(&mut positions, m.vertices)?;
    reserve(&mut gpu_positions, times(m.vertices, 12)?)?;
    reserve(&mut gpu_indices, times(m.triangles, 12)?)?;
    let mut end = 0usize;
    let mut triangles = 0usize;
    for (mesh_id, sm) in stored_meshes.iter().enumerate() {
        if !keep_going(0.1 + 0.5 * mesh_id as f32 / stored_meshes.len().max(1) as f32) {
            return Err(Error::Unsupported("scene parsing cancelled".into()));
        }
        if sm.vertex_count == 0
            || sm.vertex_count > 2_000_000
            || sm.triangle_count > MAX_TRIANGLES
            || !matches!(sm.source_precision_bits, 32 | 64)
            || !ids.contains_key(sm.owner.as_str())
            || sm.vertex_byte_offset != end
        {
            return Err(malformed("scene mesh descriptor"));
        }
        let bound = Bounds(sm.local_bounds);
        if !bound.valid() {
            return Err(malformed("scene local bounds"));
        }
        let origin = bound.center();
        let start = positions.len();
        let index_start = gpu_indices.len() / 4;
        let mut measured = Bounds::empty();
        let vbytes = times(sm.vertex_count, 24)?;
        range(input.binary, end, vbytes)?;
        if plus(positions.len(), sm.vertex_count)? > m.vertices {
            return Err(over("scene vertex total"));
        }
        for i in 0..sm.vertex_count {
            if i % 65536 == 0 && !keep_going(0.2) {
                return Err(Error::Unsupported("scene parsing cancelled".into()));
            }
            let at = plus(end, times(i, 24)?)?;
            let p = [number(input.binary, at)?, number(input.binary, plus(at, 8)?)?, number(input.binary, plus(at, 16)?)?];
            measured.include(p);
            positions.push(p);
            for v in math::sub(p, origin) {
                let v = v as f32;
                if !v.is_finite() {
                    return Err(Error::Unsupported("scene coordinates exceed GPU range".into()));
                }
                gpu_positions.extend_from_slice(&v.to_le_bytes());
            }
        }
        if measured != bound {
            return Err(malformed("scene local bounds do not match vertices"));
        }
        end = plus(end, vbytes)?;
        if sm.index_byte_offset != end {
            return Err(malformed("scene index offset"));
        }
        let ibytes = times(sm.triangle_count, 12)?;
        let bytes = range(input.binary, end, ibytes)?;
        for v in bytes.as_chunks::<4>().0 {
            let i = u32::from_le_bytes(*v);
            if usize::try_from(i).map_err(|_| over("scene index"))? >= sm.vertex_count {
                return Err(malformed("triangle index outside mesh"));
            }
        }
        gpu_indices.extend_from_slice(bytes);
        end = plus(end, ibytes)?;
        triangles = plus(triangles, sm.triangle_count)?;
        if triangles > m.triangles {
            return Err(over("scene triangle total"));
        }
        if ids.get(sm.owner.as_str()).and_then(|i| nodes.get(*i)).and_then(|n| n.mesh) != Some(mesh_id) {
            return Err(malformed("scene mesh owner"));
        }
        meshes.push(Mesh { vertex_start: start, index_start, vertex_count: sm.vertex_count, triangle_count: sm.triangle_count, origin, bounds: bound });
    }
    if end != input.binary.len() || positions.len() != m.vertices || triangles != m.triangles {
        return Err(malformed("scene binary totals/trailing data"));
    }
    let stored: Vec<StoredInstance> = records(input.instances, MAX_INSTANCES)?;
    if stored.len() != m.instances {
        return Err(malformed("scene instance count"));
    }
    let mut instances = Vec::new();
    reserve(&mut instances, stored.len())?;
    let mut instance_ids = BTreeSet::new();
    let mut path_items = 0usize;
    let mut owner_items = 0usize;
    let mut bounds = Bounds::empty();
    for si in stored {
        path_items = plus(path_items, si.path.len())?;
        owner_items = plus(owner_items, si.resource_guids.len())?;
        if si.path.is_empty()
            || si.path.len() > 65
            || path_items > 1_000_000
            || owner_items > 1_000_000
            || si.resource_guids.len() > 64
            || !math::affine(&si.world_matrix)
        {
            return Err(over("scene instance metadata/transform"));
        }
        let mut seen = BTreeSet::new();
        let mut owners = BTreeSet::new();
        let mut matrix = math::IDENTITY;
        let mut previous: Option<&StoredNode> = None;
        let mut entity = None;
        let mut h = Sha256::new();
        h.update(m.source_sha256.as_bytes());
        for id in &si.path {
            let node = ids.get(id.as_str()).and_then(|i| nodes.get(*i)).ok_or_else(|| malformed("instance path node missing"))?;
            if !seen.insert(id.as_str()) {
                return Err(malformed("instance path cycle"));
            }
            if let Some(prior) = previous {
                if !child_edges.contains(&(prior.guid.as_str(), id.as_str())) && prior.reference.as_ref() != Some(id) {
                    return Err(malformed("instance path edge missing"));
                }
            } else if node.section != "Entities" || node.parent_guid.is_some() {
                return Err(malformed("instance path is not rooted in Entities"));
            }
            if node.section == "Entities" {
                entity = Some(node.guid.as_str());
            }
            matrix = math::multiply(&matrix, &node.matrix)?;
            if let Some(rs) = direct.get(id.as_str()) {
                owners.extend(rs);
            }
            h.update(b"/");
            h.update(id.as_bytes());
            previous = Some(node);
        }
        if format!("{:x}", h.finalize()) != si.instance_id
            || !instance_ids.insert(si.instance_id.clone())
            || entity != Some(si.entity_node_guid.as_str())
            || previous.and_then(|n| n.mesh) != Some(si.mesh_id)
        {
            return Err(malformed("instance identity/terminal mesh"));
        }
        if matrix.iter().zip(si.world_matrix).any(|(a, b)| (a - b).abs() > 1e-9 * a.abs().max(1.)) {
            return Err(malformed("instance world transform differs from graph"));
        }
        let mesh = meshes.get(si.mesh_id).ok_or_else(|| malformed("instance mesh missing"))?;
        let b = mesh.bounds.transformed(&matrix)?;
        if b.0.iter().flatten().zip(si.conservative_world_bounds.iter().flatten()).any(|(a, b)| (a - b).abs() > 1e-8 * a.abs().max(1.)) {
            return Err(malformed("instance bounds differ from mesh"));
        }
        if b.0.iter().flatten().any(|v| v.abs() > 1e12) {
            return Err(Error::Unsupported("scene world extent exceeds viewer range".into()));
        }
        let declared = si
            .resource_guids
            .iter()
            .map(|g| resource_ids.get(g.as_str()).copied().ok_or_else(|| malformed("instance Resource missing")))
            .collect::<Result<BTreeSet<_>>>()?;
        if declared != owners || declared.len() != si.resource_guids.len() {
            return Err(malformed("instance Resource membership differs from graph"));
        }
        bounds.include(b.0[0]);
        bounds.include(b.0[1]);
        instances.push(Instance {
            id: si.instance_id,
            entity_guid: si.entity_node_guid,
            mesh: si.mesh_id,
            matrix,
            inverse: math::inverse(&matrix),
            bounds: b,
            resources: owners.into_iter().collect(),
        });
    }
    if instances.is_empty() || !bounds.valid() {
        return Err(Error::Unsupported("scene has no drawable mesh instances".into()));
    }
    if !keep_going(0.95) {
        return Err(Error::Unsupported("scene parsing cancelled".into()));
    }
    let mut fingerprint = Sha256::new();
    for ((name, _), bytes) in MEMBERS.iter().zip(data) {
        fingerprint.update(name.as_bytes());
        fingerprint.update((bytes.len() as u64).to_le_bytes());
        for block in bytes.chunks(1024 * 1024) {
            if !keep_going(0.98) {
                return Err(Error::Unsupported("scene parsing cancelled".into()));
            }
            fingerprint.update(block);
        }
    }
    let mut resource_instance_counts = vec![0usize; resources.len()];
    for i in &instances {
        for r in &i.resources {
            if let Some(n) = resource_instance_counts.get_mut(*r) {
                *n += 1;
            }
        }
    }
    Ok(Scene {
        id: format!("{:x}", fingerprint.finalize()),
        source_sha256: m.source_sha256,
        unit_declaration: m.coordinate_unit,
        axis_declaration: m.world_axis_convention,
        resources,
        resource_instance_counts,
        meshes,
        instances,
        positions,
        gpu_positions,
        gpu_indices,
        bounds,
        render_origin: bounds.center(),
        triangles,
        node_count: nodes.len(),
    })
}
#[derive(Debug, Serialize)]
pub struct Pick {
    pub instance: Option<usize>,
    pub position: Option<Point>,
    pub distance: Option<f64>,
    pub triangles_tested: usize,
    pub candidates: usize,
    pub complete: bool,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visibility {
    pub hidden_resources: BTreeSet<usize>,
    pub isolate_resources: BTreeSet<usize>,
    pub hidden_instances: BTreeSet<usize>,
    pub isolate_instances: BTreeSet<usize>,
}
impl Scene {
    pub fn is_visible(&self, id: usize, filters: &Visibility) -> bool {
        self.instances.get(id).is_some_and(|i| {
            !filters.hidden_instances.contains(&id)
                && (filters.isolate_instances.is_empty() || filters.isolate_instances.contains(&id))
                && self.visible(i, &filters.hidden_resources, &filters.isolate_resources)
        })
    }
    pub fn visible(&self, instance: &Instance, hidden: &BTreeSet<usize>, isolate: &BTreeSet<usize>) -> bool {
        !instance.resources.iter().any(|i| hidden.contains(i)) && (isolate.is_empty() || instance.resources.iter().any(|i| isolate.contains(i)))
    }
    pub fn resource_bounds(&self, id: usize) -> Option<Bounds> {
        let mut b = Bounds::empty();
        for i in &self.instances {
            if i.resources.contains(&id) {
                b.include(i.bounds.0[0]);
                b.include(i.bounds.0[1]);
            }
        }
        b.valid().then_some(b)
    }
    /// Surface picking is bounded and reports incomplete when work or singular transforms prevent certainty.
    pub fn pick(&self, origin: Point, direction: Point, filters: &Visibility, mut keep_going: impl FnMut() -> bool) -> Result<Pick> {
        if !math::finite(origin) || origin.iter().any(|v| v.abs() > 1e15) {
            return Err(malformed("invalid picking origin"));
        }
        let direction = math::normalized(direction)?;
        let mut candidates = self
            .instances
            .iter()
            .enumerate()
            .filter(|(id, _)| self.is_visible(*id, filters))
            .filter_map(|(id, i)| i.bounds.ray(origin, direction).map(|t| (id, t)))
            .collect::<Vec<_>>();
        candidates.sort_by(|a, b| a.1.total_cmp(&b.1));
        let count = candidates.len();
        let mut tested = 0usize;
        let mut nearest = f64::INFINITY;
        let mut hit = None;
        let mut complete = true;
        for (id, entry) in candidates {
            if entry > nearest {
                break;
            }
            let i = self.instances.get(id).ok_or_else(|| malformed("pick instance"))?;
            let Some(inv) = i.inverse else {
                complete = false;
                continue;
            };
            let local_origin = math::transform(&inv, origin);
            let local_direction = math::vector(&inv, direction);
            let mesh = self.meshes.get(i.mesh).ok_or_else(|| malformed("pick mesh"))?;
            for triangle in 0..mesh.triangle_count {
                if tested >= 2_000_000 || (tested.is_multiple_of(1024) && !keep_going()) {
                    return Ok(Pick { instance: None, position: None, distance: None, triangles_tested: tested, candidates: count, complete: false });
                }
                tested += 1;
                let at = times(plus(mesh.index_start, times(triangle, 3)?)?, 4)?;
                let mut v = [[0.; 3]; 3];
                for (corner, p) in v.iter_mut().enumerate() {
                    let ix = usize::try_from(index(&self.gpu_indices, plus(at, times(corner, 4)?)?)?).map_err(|_| over("pick index"))?;
                    *p = *self.positions.get(plus(mesh.vertex_start, ix)?).ok_or_else(|| malformed("pick vertex"))?;
                }
                let e1 = math::sub(v[1], v[0]);
                let e2 = math::sub(v[2], v[0]);
                let p = math::cross(local_direction, e2);
                let det = math::dot(e1, p);
                let eps = 1e-14
                    * e1[0].hypot(e1[1]).hypot(e1[2])
                    * e2[0].hypot(e2[1]).hypot(e2[2])
                    * local_direction[0].hypot(local_direction[1]).hypot(local_direction[2]);
                if !det.is_finite() || det.abs() <= eps.max(1e-100) {
                    continue;
                }
                let rel = math::sub(local_origin, v[0]);
                let u = math::dot(rel, p) / det;
                if !(0. - 1e-10..=1. + 1e-10).contains(&u) {
                    continue;
                }
                let q = math::cross(rel, e1);
                let v = math::dot(local_direction, q) / det;
                if v < -1e-10 || u + v > 1. + 1e-10 {
                    continue;
                }
                let t = math::dot(e2, q) / det;
                if t >= 0. && t < nearest && t.is_finite() {
                    nearest = t;
                    hit = Some(id);
                }
            }
        }
        if !complete {
            return Ok(Pick { instance: None, position: None, distance: None, triangles_tested: tested, candidates: count, complete: false });
        }
        Ok(Pick {
            instance: hit,
            position: hit.map(|_| math::add(origin, math::scale(direction, nearest))),
            distance: hit.map(|_| nearest),
            triangles_tested: tested,
            candidates: count,
            complete,
        })
    }
}

#[cfg(test)]
mod tests;
