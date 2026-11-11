//! Original synthetic scene bundle: two overlapping triangles and one resource.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
const IDENTITY: [f64; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
pub const A: &str = "10000000-0000-0000-0000-000000000001";
pub const B: &str = "10000000-0000-0000-0000-000000000002";
pub const R: &str = "20000000-0000-0000-0000-000000000001";
fn line(value: Value) -> Vec<u8> {
    let mut v = serde_json::to_vec(&value).unwrap();
    v.push(b'\n');
    v
}
pub fn fixture() -> Vec<Vec<u8>> {
    let source = "a".repeat(64);
    let mut binary = Vec::new();
    for p in [[-1f64, -1., 0.], [1., -1., 0.], [0., 1., 0.]] {
        for v in p {
            binary.extend_from_slice(&v.to_le_bytes());
        }
    }
    for v in [0u32, 1, 2] {
        binary.extend_from_slice(&v.to_le_bytes());
    }
    let manifest = json!({"schema_version":1,"status":"experimental_scene_partial","source_sha256":source,"entities":2,"library_nodes":0,"meshes":1,"vertices":3,"triangles":1,"instances":2,"direct_resource_bindings":2,"binary_bytes":binary.len(),"coordinate_unit":"unverified","world_axis_convention":"unverified","artifact_layout":{"nodes":"nodes.jsonl","meshes":"meshes.jsonl","instances":"instances.jsonl","resource_bindings":"resource-bindings.jsonl","project":"project.json","binary":"geometry.bin","positions":"little-endian f64 xyz, 24 bytes per vertex","triangle_indices":"little-endian u32 abc, 12 bytes per triangle"}});
    let mut nodes = Vec::new();
    let mut instances = Vec::new();
    let mut bindings = Vec::new();
    for (id, z) in [(A, 0.), (B, 2.)] {
        let mut matrix = IDENTITY;
        matrix[14] = z;
        nodes.extend(line(
            json!({"guid":id,"section":"Entities","name":"Synthetic triangle","parent_guid":null,"children":[],"mesh":0,"reference":null,"matrix":matrix}),
        ));
        let mut h = Sha256::new();
        h.update(source.as_bytes());
        h.update(b"/");
        h.update(id.as_bytes());
        instances.extend(line(json!({"instance_id":format!("{:x}",h.finalize()),"entity_node_guid":id,"mesh_id":0,"path":[id],"world_matrix":matrix,"conservative_world_bounds":[[-1.,-1.,z],[1.,1.,z]],"resource_guids":[R]})));
        bindings.extend(line(json!({"resource_guid":R,"node_guid":id,"source_section":"Resources","binding_kind":"direct_subtree"})));
    }
    vec![
        line(manifest),
        nodes,
        line(
            json!({"owner":A,"vertex_count":3,"triangle_count":1,"vertex_byte_offset":0,"index_byte_offset":72,"source_precision_bits":64,"local_bounds":[[-1.,-1.,0.],[1.,1.,0.]]}),
        ),
        instances,
        bindings,
        line(json!({"schema_version":4,"resources":[{"guid":R,"name":"Synthetic assembly","parent_guid":null}]})),
        binary,
    ]
}
