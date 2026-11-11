use super::*;
use craft_anything_testkit::scene_bundle::{A, B, R, fixture};
use serde_json::{Value, json};
fn line(value: Value) -> Vec<u8> {
    let mut v = serde_json::to_vec(&value).unwrap();
    v.push(b'\n');
    v
}
fn read(files: &[Vec<u8>]) -> Result<Scene> {
    parse(
        Inputs { manifest: &files[0], nodes: &files[1], meshes: &files[2], instances: &files[3], bindings: &files[4], project: &files[5], binary: &files[6] },
        |_| true,
    )
}
fn modify(files: &mut [Vec<u8>], member: usize, row: usize, f: impl FnOnce(&mut Value)) {
    let mut rows = files[member].split(|b| *b == b'\n').filter(|b| !b.is_empty()).map(|b| serde_json::from_slice::<Value>(b).unwrap()).collect::<Vec<_>>();
    f(&mut rows[row]);
    files[member] = rows.into_iter().flat_map(line).collect();
}
#[test]
fn frontmost_surface_visibility_and_membership() {
    let s = read(&fixture()).unwrap();
    assert_eq!(s.resource_instance_counts, vec![2]);
    let mut filters = Visibility::default();
    let hit = s.pick([0., 0., 5.], [0., 0., -1.], &filters, || true).unwrap();
    assert!(hit.complete);
    assert_eq!(hit.instance, Some(1));
    assert_eq!(hit.distance, Some(3.));
    filters.hidden_instances.insert(1);
    assert_eq!(s.pick([0., 0., 5.], [0., 0., -1.], &filters, || true).unwrap().instance, Some(0));
    filters.hidden_resources.insert(0);
    assert_eq!(s.pick([0., 0., 5.], [0., 0., -1.], &filters, || true).unwrap().instance, None);
    filters = Visibility::default();
    filters.isolate_instances.insert(0);
    assert_eq!(s.pick([0., 0., 5.], [0., 0., -1.], &filters, || true).unwrap().instance, Some(0));
    assert_eq!(s.resource_bounds(0), Some(s.bounds));
}
#[test]
fn cancellation_and_invalid_ray_never_claim_a_hit() {
    let files = fixture();
    let s = read(&files).unwrap();
    assert!(s.pick([0.; 3], [0.; 3], &Visibility::default(), || true).is_err());
    let hit = s.pick([0., 0., 5.], [0., 0., -1.], &Visibility::default(), || false).unwrap();
    assert!(!hit.complete);
    assert!(hit.instance.is_none());
    assert!(
        parse(
            Inputs {
                manifest: &files[0],
                nodes: &files[1],
                meshes: &files[2],
                instances: &files[3],
                bindings: &files[4],
                project: &files[5],
                binary: &files[6]
            },
            |_| false
        )
        .is_err()
    );
}
#[test]
fn rejects_bad_versions_layout_counts_indices_bounds_and_transform() {
    let mut cases = Vec::new();
    for (member, row, key, value) in [
        (0, 0, "schema_version", json!(99)),
        (0, 0, "entities", json!(1)),
        (0, 0, "vertices", json!(usize::MAX)),
        (2, 0, "index_byte_offset", json!(usize::MAX)),
        (2, 0, "local_bounds", json!([[-2., -1., 0.], [1., 1., 0.]])),
        (3, 1, "world_matrix", json!(math::IDENTITY)),
        (3, 0, "resource_guids", json!([R, R])),
    ] {
        let mut f = fixture();
        modify(&mut f, member, row, |v| v[key] = value);
        cases.push(f);
    }
    let mut f = fixture();
    modify(&mut f, 0, 0, |v| v["artifact_layout"]["binary"] = json!("../outside.bin"));
    cases.push(f);
    let mut f = fixture();
    f[6][72..76].copy_from_slice(&9u32.to_le_bytes());
    cases.push(f);
    let mut f = fixture();
    f[6][..8].copy_from_slice(&f64::NAN.to_le_bytes());
    cases.push(f);
    let mut f = fixture();
    f[6].pop();
    cases.push(f);
    for (i, f) in cases.into_iter().enumerate() {
        assert!(read(&f).is_err(), "hostile case {i}");
    }
}
#[test]
fn rejects_missing_reverse_parent_and_resource_cycle() {
    let mut f = fixture();
    modify(&mut f, 1, 1, |v| v["parent_guid"] = json!(A));
    assert!(read(&f).is_err());
    let mut f = fixture();
    modify(&mut f, 5, 0, |v| v["resources"][0]["parent_guid"] = json!(R));
    assert!(read(&f).is_err());
}
#[test]
fn every_truncated_member_and_corrupt_json_is_an_error() {
    let original = fixture();
    for member in 0..7 {
        let mut f = original.clone();
        f[member].clear();
        assert!(read(&f).is_err(), "empty member {member}");
        let mut f = original.clone();
        let half = f[member].len() / 2;
        f[member].truncate(half);
        assert!(read(&f).is_err(), "half member {member}");
    }
}

#[test]
fn unused_reference_cycles_are_rejected() {
    let mut f = fixture();
    modify(&mut f, 1, 0, |v| v["reference"] = json!(B));
    modify(&mut f, 1, 1, |v| v["reference"] = json!(A));
    assert!(read(&f).is_err());
}

#[test]
fn singular_transform_and_triangle_budget_do_not_return_certain_hits() {
    let mut s = read(&fixture()).unwrap();
    s.instances[1].inverse = None;
    let hit = s.pick([0., 0., 5.], [0., 0., -1.], &Visibility::default(), || true).unwrap();
    assert!(!hit.complete && hit.instance.is_none());
    s.instances[1].inverse = Some(math::IDENTITY);
    s.meshes[0].triangle_count = 2_000_001;
    s.gpu_indices = [0u32, 1, 2].into_iter().flat_map(u32::to_le_bytes).cycle().take(2_000_001 * 12).collect();
    let hit = s.pick([0., 0., 5.], [0., 0., -1.], &Visibility::default(), || true).unwrap();
    assert!(!hit.complete && hit.instance.is_none());
    assert_eq!(hit.triangles_tested, 2_000_000);
}

#[test]
fn serialized_geometry_bounds_preserve_binary_f64_precision() {
    let mut seed = 0x915fe12d7788ac01u64;
    for _ in 0..128 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let x = f64::from_bits(0x3ff0000000000000 | (seed & 0x000fffffffffffff));
        let mut f = fixture();
        f[6][0..8].copy_from_slice(&(-x).to_le_bytes());
        f[6][24..32].copy_from_slice(&x.to_le_bytes());
        modify(&mut f, 2, 0, |v| v["local_bounds"] = json!([[-x, -1., 0.], [x, 1., 0.]]));
        for (row, z) in [(0, 0.), (1, 2.)] {
            modify(&mut f, 3, row, |v| v["conservative_world_bounds"] = json!([[-x, -1., z], [x, 1., z]]));
        }
        assert!(read(&f).is_ok(), "round trip for {x}");
    }
}

#[test]
fn legacy_bundle_has_no_source_colors() {
    let scene = read(&fixture()).unwrap();
    assert!(scene.source_colors.is_empty());
    assert!(SOURCE_COLOR_POLICY.contains("unverified"));
}
#[test]
fn source_candidates_follow_declared_nearest_explicit_path_policy() {
    let mut files = fixture();
    modify(&mut files, 1, 0, |v| {
        v["children"] = json!([B]);
        v["source_diffuse"] = json!([0.8, 0.1, 0.05]);
    });
    modify(&mut files, 1, 1, |v| v["parent_guid"] = json!(A));
    let mut hash = Sha256::new();
    hash.update("a".repeat(64).as_bytes());
    for id in [A, B] {
        hash.update(b"/");
        hash.update(id.as_bytes());
    }
    modify(&mut files, 3, 1, |v| {
        v["path"] = json!([A, B]);
        v["instance_id"] = json!(format!("{:x}", hash.finalize()));
    });
    let scene = read(&files).unwrap();
    assert_eq!(scene.source_colors.get(&0), Some(&[0.8f32, 0.1, 0.05]));
    assert_eq!(scene.source_colors.get(&1), Some(&[0.8f32, 0.1, 0.05]));
    modify(&mut files, 1, 1, |v| v["source_diffuse"] = json!([0.0, 0.25, 1.0]));
    let scene = read(&files).unwrap();
    assert_eq!(scene.source_colors.get(&0), Some(&[0.8f32, 0.1, 0.05]));
    assert_eq!(scene.source_colors.get(&1), Some(&[0.0f32, 0.25, 1.0]));
}
#[test]
fn invalid_source_diffuse_is_rejected() {
    for rgb in [json!([-0.01, 0., 0.]), json!([0., 1.01, 0.]), json!([0., 0.]), json!(["NaN", 0., 0.])] {
        let mut files = fixture();
        modify(&mut files, 1, 0, |v| v["source_diffuse"] = rgb);
        assert!(read(&files).is_err());
    }
    let mut files = fixture();
    modify(&mut files, 1, 0, |v| v["source_diffuse"] = json!([0.25, 0.5, 0.75]));
    let text = String::from_utf8(files[1].clone()).unwrap();
    for invalid in ["[0.25,NaN,0.75]", "[0.25,1e999,0.75]"] {
        files[1] = text.replace("[0.25,0.5,0.75]", invalid).into_bytes();
        assert!(read(&files).is_err());
    }
}

#[test]
fn object_colors_use_direct_leaf_values_without_ancestor_fallback() {
    let mut files = fixture();
    modify(&mut files, 1, 0, |v| {
        v["children"] = json!([B]);
        v["source_object_color"] = json!([0.25, 0.5, 0.75]);
    });
    modify(&mut files, 1, 1, |v| v["parent_guid"] = json!(A));
    let mut hash = Sha256::new();
    hash.update("a".repeat(64).as_bytes());
    for id in [A, B] {
        hash.update(b"/");
        hash.update(id.as_bytes());
    }
    modify(&mut files, 3, 1, |v| {
        v["path"] = json!([A, B]);
        v["instance_id"] = json!(format!("{:x}", hash.finalize()));
    });
    let scene = read(&files).unwrap();
    assert_eq!(scene.object_colors.get(&0), Some(&[0.25f32, 0.5, 0.75]));
    assert!(!scene.object_colors.contains_key(&1));
    modify(&mut files, 1, 1, |v| v["source_object_color"] = json!([0.75, 0.5, 0.25]));
    let scene = read(&files).unwrap();
    assert_eq!(scene.object_colors.get(&1), Some(&[0.75f32, 0.5, 0.25]));
    assert!(read(&fixture()).unwrap().object_colors.is_empty());
}
#[test]
fn object_colors_reject_invalid_components() {
    for rgb in [json!([-0.1, 0., 0.]), json!([0., 1.1, 0.]), json!([0., 0.])] {
        let mut files = fixture();
        modify(&mut files, 1, 0, |v| v["source_object_color"] = rgb);
        assert!(read(&files).is_err());
    }
}

#[test]
fn object_color_belongs_to_entity_not_terminal_library_definition() {
    const C: &str = "10000000-0000-0000-0000-000000000003";
    let mut files = fixture();
    modify(&mut files, 0, 0, |v| v["library_nodes"] = json!(1));
    modify(&mut files, 1, 1, |v| {
        v["mesh"] = Value::Null;
        v["reference"] = json!(C);
        v["source_object_color"] = json!([0.25, 0.5, 0.75]);
    });
    files[1].extend(line(json!({"guid":C,"section":"IncludeLibrary","name":"Synthetic definition","parent_guid":null,"children":[],"mesh":0,"reference":null,"matrix":math::IDENTITY,"source_object_color":[0.75,0.5,0.25]})));
    let mut hash = Sha256::new();
    hash.update("a".repeat(64).as_bytes());
    for id in [B, C] {
        hash.update(b"/");
        hash.update(id.as_bytes());
    }
    modify(&mut files, 3, 1, |v| {
        v["path"] = json!([B, C]);
        v["instance_id"] = json!(format!("{:x}", hash.finalize()));
    });
    let scene = read(&files).unwrap();
    assert_eq!(scene.object_colors.get(&1), Some(&[0.25f32, 0.5, 0.75]));
    modify(&mut files, 1, 1, |v| v["source_object_color"] = Value::Null);
    assert!(!read(&files).unwrap().object_colors.contains_key(&1));
}
