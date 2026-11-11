use super::scene_io::Input;
use super::*;
const NODE_LIMIT: usize = 100_000;
const VERTEX_LIMIT: usize = 2_000_000;
const TOTAL_VERTEX_LIMIT: usize = 10_000_000;
const TRIANGLE_LIMIT: usize = 8_000_000;
const BINARY_LIMIT: usize = scene_bundle::MAX_BINARY_BYTES;
pub(crate) const IDENTITY: [f64; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];
#[derive(Serialize)]
pub(crate) struct Node {
    pub section: String,
    pub offset: usize,
    pub guid: String,
    pub parent_guid: Option<String>,
    pub name: String,
    pub matrix: [f64; 16],
    pub children: Vec<String>,
    pub mesh: Option<usize>,
    pub reference: Option<String>,
    pub visibility_storage: [u8; 4],
    pub user_value_count: usize,
    pub opaque_guid_list_count: usize,
    pub source_diffuse: Option<[f64; 3]>,
    pub source_diffuse_origin: Option<SourceDiffuseOrigin>,
    pub source_diffuse_status: &'static str,
    pub source_object_color: Option<[f64; 3]>,
    pub source_object_color_origin: Option<SourceDiffuseOrigin>,
    pub source_opacity: Option<f64>,
}

#[derive(Serialize)]
pub(crate) struct Mesh {
    pub section: String,
    pub offset: usize,
    pub owner: String,
    pub vertex_count: usize,
    pub triangle_count: usize,
    pub vertex_byte_offset: usize,
    pub index_byte_offset: usize,
    pub source_precision_bits: u32,
    pub local_bounds: [[f64; 3]; 2],
}
pub(crate) struct Geometry<'a> {
    pub nodes: Vec<Node>,
    pub meshes: Vec<Mesh>,
    pub roots: BTreeMap<String, Vec<String>>,
    pub binary_bytes: usize,
    vertices: usize,
    triangles: usize,
    seen: BTreeSet<String>,
    user_count: usize,
    opaque_refs: usize,
    name_bytes: usize,
    pub keep_going: &'a mut dyn FnMut(f32) -> bool,
    pub binary: Vec<u8>,
}
impl<'a> Geometry<'a> {
    pub fn keep_callback(&mut self) -> &mut dyn FnMut(f32) -> bool {
        self.keep_going
    }

    pub fn new(keep_going: &'a mut dyn FnMut(f32) -> bool) -> Result<Self> {
        Ok(Self {
            nodes: Vec::new(),
            meshes: Vec::new(),
            roots: BTreeMap::new(),
            binary_bytes: 0,
            vertices: 0,
            triangles: 0,
            seen: BTreeSet::new(),
            user_count: 0,
            opaque_refs: 0,
            name_bytes: 0,
            keep_going,
            binary: Vec::new(),
        })
    }
    fn write(&mut self, b: &[u8]) -> Result<()> {
        self.binary_bytes = checked_add(self.binary_bytes, b.len())?;
        if self.binary_bytes > BINARY_LIMIT {
            return Err(fail("limit", "Scene binary budget"));
        }
        self.binary.try_reserve(b.len()).map_err(|_| fail("limit", "Geometry allocation"))?;
        self.binary.extend_from_slice(b);
        Ok(())
    }
    fn attribute(&mut self, r: &mut Input) -> Result<AttributeColours> {
        match r.byte()? {
            0 => Ok(AttributeColours::default()),
            1 => {
                r.marker(&[0x5d, 0xa2, 0x6d, 0x26])?;
                let n = r.count(16 * 1024 * 1024)?;
                let payload = bytes(r.data, r.pos, n)?;
                let candidate = AttributeColours { diffuse: source_diffuse_candidate(payload), object: source_object_color_candidate(payload) };
                r.skip(checked_add(n, 1)?)?;
                let tag = r.take::<4>()?;
                if tag == [0xd0, 0xbc, 0xa1, 0x06] {
                    let n = r.count(200_000)?;
                    self.opaque_refs = checked_add(self.opaque_refs, n)?;
                    if self.opaque_refs > 1_000_000 {
                        return Err(fail("limit", "Geometry attribute reference budget"));
                    }
                    r.skip(n.checked_mul(20).ok_or_else(|| fail("limit", "Attribute references"))?)?;
                    r.marker(&[0x2c, 0x57, 0xd9, 0xda])?;
                    r.marker(&[0x59, 0xee, 0x21, 0xf4])?;
                    Ok(candidate)
                } else if tag == [0x59, 0xee, 0x21, 0xf4] {
                    Ok(candidate)
                } else {
                    Err(fail("unsupported_layout", "Attribute footer"))
                }
            }
            _ => Err(fail("unsupported_layout", "Unknown geometry attribute flag")),
        }
    }
    fn mesh(&mut self, r: &mut Input, owner: &str, section: &str, precision: u32) -> Result<usize> {
        let offset = r.pos;
        r.marker(&[0xb6, 0x69, 0x4a, 0x30])?;
        let n = r.count(VERTEX_LIMIT)?;
        if n == 0 {
            return Err(fail("unsupported_layout", "Empty mesh"));
        }
        self.vertices = checked_add(self.vertices, n)?;
        if self.vertices > TOTAL_VERTEX_LIMIT {
            return Err(fail("limit", "Scene vertex budget"));
        }
        let vertex_byte_offset = self.binary_bytes;
        let mut bounds = [[f64::INFINITY; 3], [f64::NEG_INFINITY; 3]];
        for vertex in 0..n {
            if vertex % 4096 == 0 {
                check(self.keep_going, 0.5)?;
            }
            for axis in 0..3 {
                let v = if precision == 32 { r.f32()? } else { r.f64()? };
                if let Some(lo) = bounds.get_mut(0).and_then(|b| b.get_mut(axis)) {
                    *lo = lo.min(v);
                }
                if let Some(hi) = bounds.get_mut(1).and_then(|b| b.get_mut(axis)) {
                    *hi = hi.max(v);
                }
                self.write(&v.to_le_bytes())?;
            }
        }
        let flat = r.count(16_000_000)?;
        if flat % 4 != 0 {
            return Err(fail("unsupported_layout", "Only framed triangles admitted"));
        }
        let triangles = flat / 4;
        self.triangles = checked_add(self.triangles, triangles)?;
        if self.triangles > TRIANGLE_LIMIT {
            return Err(fail("limit", "Scene triangle budget"));
        }
        let index_byte_offset = self.binary_bytes;
        for triangle in 0..triangles {
            if triangle % 4096 == 0 {
                check(self.keep_going, 0.55)?;
            }
            if r.u32()? != 3 {
                return Err(fail("unsupported_layout", "Non-triangle polygon"));
            }
            for _ in 0..3 {
                let v = r.u32()?;
                if usize::try_from(v).map_err(|_| fail("limit", "Mesh index"))? >= n {
                    return Err(fail("invalid_geometry", "Mesh index outside vertices"));
                }
                self.write(&v.to_le_bytes())?;
            }
        }
        r.marker(&[1, 0xc0, 0x5a, 0x01, 0xf3])?;
        let uv = r.count(VERTEX_LIMIT)?;
        let components = r.count(4)?;
        let uv_precision = r.byte()?;
        let width = match uv_precision {
            0 => 8usize,
            1 => 4usize,
            _ => return Err(fail("unsupported_layout", "UV precision")),
        };
        if (uv == 0 && (components != 0 || uv_precision != 0)) || (uv > 0 && (uv != n || components != 3 || uv_precision != 1)) {
            return Err(fail("unsupported_layout", "Empty UV arity"));
        }
        let uvbytes = uv.checked_mul(components).and_then(|n| n.checked_mul(width)).ok_or_else(|| fail("limit", "UV bytes"))?;
        r.skip(uvbytes)?;
        r.marker(&[0])?;
        let _ = self.attribute(r)?;
        r.skip(8)?;
        let normals = r.count(6_000_000)?;
        if (normals != 0 && normals != n.checked_mul(3).ok_or_else(|| fail("limit", "Normal count arithmetic"))?) || (precision == 64 && normals != 0) {
            return Err(fail("unsupported_layout", "Unobserved mesh normal layout"));
        }
        r.skip(normals.checked_mul(4).ok_or_else(|| fail("limit", "Normal bytes"))?)?;
        r.skip(12)?;
        r.marker(&[0x1c, 0xb3, 0x2c, 0x53])?;
        let id = self.meshes.len();
        self.meshes.push(Mesh {
            section: section.into(),
            offset,
            owner: owner.into(),
            vertex_count: n,
            triangle_count: triangles,
            vertex_byte_offset,
            index_byte_offset,
            source_precision_bits: precision,
            local_bounds: bounds,
        });
        Ok(id)
    }
    fn user_values(&mut self, r: &mut Input, index: &definitions::Index) -> Result<usize> {
        let flag = r.byte()?;
        if flag == 0 {
            return Ok(0);
        }
        if flag != 1 {
            return Err(fail("unsupported_layout", "Geometry user-list flag"));
        }
        r.marker(OPEN)?;
        let n = r.count(RECORD_LIMIT)?;
        self.user_count = checked_add(self.user_count, n)?;
        if self.user_count > 1_000_000 {
            return Err(fail("limit", "Geometry user-value budget"));
        }
        for _ in 0..n {
            let offset = r.pos;
            r.marker(&[0xa3, 0x7b, 0x0a, 0x44])?;
            let variant = r.u32()?;
            let mut integer_storage = None;
            let mut number_storage = None;
            let mut text_storage = None;
            match variant {
                3 => integer_storage = Some(i32::from_le_bytes(r.take()?)),
                4 => number_storage = Some(r.f64()?),
                6 => text_storage = Some(r.string()?),
                _ => return Err(fail("unsupported_layout", "Geometry user-value type")),
            };
            let reference_offset = r.pos;
            let definition_guid = r.guid()?;
            r.skip(6)?;
            let ordinal_storage = r.u32()?;
            definitions::check_user(
                index,
                &UserValue {
                    offset,
                    variant,
                    reference_offset,
                    definition_guid,
                    integer_storage,
                    number_storage,
                    text_storage,
                    opaque_tail_hex: String::new(),
                    ordinal_storage,
                },
                Some(3),
            )?;
        }
        r.marker(CLOSE)?;
        Ok(n)
    }
    fn node(&mut self, r: &mut Input, section: &str, parent: Option<&str>, depth: usize, index: &definitions::Index) -> Result<String> {
        if depth > 64 || self.seen.len() >= NODE_LIMIT {
            return Err(fail("limit", "Scene node/depth/deadline budget"));
        }
        check(self.keep_going, 0.45)?;
        let offset = r.pos;
        r.marker(&[0x37, 0x2a, 0x1a, 0x54])?;
        r.marker(&[0; 8])?;
        let guid = r.guid()?;
        if !self.seen.insert(guid.clone()) {
            return Err(fail("invalid_identity", "Duplicate scene node GUID"));
        }
        let name = r.string()?;
        self.name_bytes = checked_add(self.name_bytes, name.len())?;
        if self.name_bytes > 16 * 1024 * 1024 {
            return Err(fail("limit", "Scene names budget"));
        }
        r.marker(&[0])?;
        let mut matrix = IDENTITY;
        match r.byte()? {
            0 => {}
            1 => {
                for v in &mut matrix {
                    *v = r.f64()?;
                }
            }
            _ => return Err(fail("unsupported_layout", "Transform flag")),
        };
        let attribute_offset = r.pos;
        let attribute_colours = self.attribute(r)?;
        let source_diffuse = attribute_colours.diffuse;
        let source_object_color = attribute_colours.object;
        let mut children = Vec::new();
        match r.byte()? {
            0 => {}
            1 => {
                r.marker(OPEN)?;
                let n = r.count(NODE_LIMIT)?;
                for _ in 0..n {
                    children.push(self.node(r, section, Some(&guid), depth + 1, index)?);
                }
                r.marker(CLOSE)?;
            }
            _ => return Err(fail("unsupported_layout", "Scene children flag")),
        };
        let mut mesh = None;
        let mut reference = None;
        match r.byte()? {
            0 => {}
            1 => {
                r.marker(&[0xae, 0xec, 0x55, 0xca])?;
                let mut pairs = 0usize;
                loop {
                    match r.byte()? {
                        0 => {
                            r.marker(&[1])?;
                            pairs += 1;
                            if pairs > 32 {
                                return Err(fail("unsupported_layout", "Geometry wrapper prefix"));
                            }
                        }
                        1 => break,
                        _ => return Err(fail("unsupported_layout", "Geometry wrapper fields")),
                    }
                }
                r.marker(&[1])?;
                if r.u32()? != 1 {
                    return Err(fail("unsupported_layout", "Primitive list count"));
                }
                match pairs {
                    6 => {
                        r.marker(&[0xe7, 0x30, 0x6a, 0xd0])?;
                        reference = Some(r.guid()?);
                    }
                    13 | 29 => {
                        mesh = Some(self.mesh(r, &guid, section, if pairs == 13 { 32 } else { 64 })?);
                    }
                    _ => return Err(fail("unsupported_layout", "Unknown geometry wrapper layout")),
                }
                if pairs == 6 {
                    r.skip(3)?;
                    r.marker(&[1, 0x23, 0x4e, 0xb0, 0xdd])?;
                    for _ in 0..26 {
                        r.marker(&[0, 1])?;
                    }
                } else {
                    r.marker(&[0, 1, 0xd2, 0xa0, 0x97, 0x39])?;
                    let material_pairs = if pairs == 29 { 3 } else { 19 };
                    for _ in 0..material_pairs {
                        r.marker(&[0, 1])?;
                    }
                }
                r.marker(&[0xca, 0x82, 0x13, 0x15])?;
            }
            _ => return Err(fail("unsupported_layout", "Geometry body flag")),
        }
        let visibility_storage = r.take::<4>()?;
        r.marker(&[1])?;
        let user_value_count = self.user_values(r, index)?;
        r.skip(5)?;
        r.marker(&[0xd0, 0xbc, 0xa1, 0x06])?;
        let opaque_guid_list_count = r.count(200_000)?;
        self.opaque_refs = checked_add(self.opaque_refs, opaque_guid_list_count)?;
        if self.opaque_refs > 1_000_000 {
            return Err(fail("limit", "Scene opaque references budget"));
        }
        r.skip(opaque_guid_list_count.checked_mul(20).ok_or_else(|| fail("limit", "Scene reference bytes"))?)?;
        r.marker(&[0x2c, 0x57, 0xd9, 0xda, 0, 0x73, 0x71, 0x5f, 0xd7])?;
        r.skip(4)?;
        self.nodes.push(Node {
            section: section.into(),
            offset,
            guid: guid.clone(),
            parent_guid: parent.map(str::to_owned),
            name,
            matrix,
            children,
            mesh,
            reference,
            visibility_storage,
            user_value_count,
            opaque_guid_list_count,
            source_diffuse,
            source_diffuse_origin: source_diffuse.map(|_| SourceDiffuseOrigin {
                section: section.to_owned(),
                decoded_offset: attribute_offset,
                encoding: "framed_material_text",
            }),
            source_diffuse_status: if source_diffuse.is_some() { "candidate_unverified" } else { "unresolved" },
            source_object_color,
            source_object_color_origin: source_object_color.map(|_| SourceDiffuseOrigin {
                section: section.to_owned(),
                decoded_offset: attribute_offset,
                encoding: "framed_binary_object_rgb",
            }),
            source_opacity: None,
        });
        Ok(guid)
    }
    pub fn section(&mut self, data: &[u8], name: &str, index: &definitions::Index) -> Result<()> {
        let mut r = Input::new(data);
        r.skip(24)?;
        r.marker(OPEN)?;
        let n = r.count(NODE_LIMIT)?;
        let mut roots = Vec::new();
        for _ in 0..n {
            roots.push(self.node(&mut r, name, None, 0, index).map_err(|mut e| {
                e.section = Some(name.into());
                if e.offset.is_none() {
                    e.offset = Some(r.pos);
                }
                e
            })?);
        }
        r.marker(CLOSE)?;
        if r.pos != r.len {
            return Err(fail("unsupported_layout", "Unconsumed scene bytes"));
        }
        self.roots.insert(name.into(), roots);
        Ok(())
    }
}

/// Provenance of a color candidate; this does not establish native display precedence.
#[derive(Serialize)]
pub(crate) struct SourceDiffuseOrigin {
    pub section: String,
    pub decoded_offset: usize,
    pub encoding: &'static str,
}
fn component(s: &str) -> Option<f64> {
    let v = s.parse::<f64>().ok()?;
    (v.is_finite() && (0.0..=1.0).contains(&v)).then_some(v)
}
fn triple(s: &str, green: &str, blue: &str, end: char) -> Option<[f64; 3]> {
    let (red, s) = s.split_once(green)?;
    let (green, s) = s.split_once(blue)?;
    let (blue, _) = s.split_once(end)?;
    Some([component(red)?, component(green)?, component(blue)?])
}
fn source_diffuse_candidate(payload: &[u8]) -> Option<[f64; 3]> {
    // Admit only the observed attribute text envelope, never scan arbitrary binary data.
    if payload.get(..8)? != [0; 8] || payload.get(16).copied()? != 0x11 {
        return None;
    }
    let n = usize::try_from(u64::from_le_bytes(payload.get(8..16)?.try_into().ok()?)).ok()?;
    if n > 16384 || n.checked_add(19)? != payload.len() || payload.get(payload.len().checked_sub(2)?..)? != [0, 0] {
        return None;
    }
    let text = std::str::from_utf8(payload.get(17..17usize.checked_add(n)?)?).ok()?;
    let mut candidate = None;
    let mut count = 0usize;
    for (prefix, green, blue, end) in [("diffuse=(r=", " g=", " b=", ')'), ("diffuse=R=", " G=", " B=", ',')] {
        for (offset, _) in text.match_indices(prefix) {
            count = count.checked_add(1)?;
            if count > 8 {
                return None;
            }
            let start = offset.checked_add(prefix.len())?;
            let rgb = triple(text.get(start..)?, green, blue, end)?;
            if candidate.is_some_and(|prior| prior != rgb) {
                return None;
            }
            candidate = Some(rgb);
        }
    }
    candidate
}

#[derive(Clone, Copy, Default)]
struct AttributeColours {
    diffuse: Option<[f64; 3]>,
    object: Option<[f64; 3]>,
}
// Native-controlled original-color selection removes this RGB envelope entirely.
// Admit only the observed active override family; other flags retain no object color.
const OBJECT_COLOR_FLAGS: &[u64] = &[17];
fn source_object_color_candidate(payload: &[u8]) -> Option<[f64; 3]> {
    binary_object_color(payload, OBJECT_COLOR_FLAGS)
}
fn binary_object_color(payload: &[u8], flags: &[u64]) -> Option<[f64; 3]> {
    let prefix = u64::from_le_bytes(payload.get(..8)?.try_into().ok()?);
    let text_len = usize::try_from(u64::from_le_bytes(payload.get(8..16)?.try_into().ok()?)).ok()?;
    if !flags.contains(&prefix)
        || text_len > 16384
        || payload.get(16).copied()? != 0x1d
        || text_len.checked_add(31)? != payload.len()
        || payload.get(payload.len().checked_sub(2)?..)? != [0, 0]
    {
        return None;
    }
    // The suffix is retained source material text, not an alternate binary RGB array.
    std::str::from_utf8(payload.get(29..29usize.checked_add(text_len)?)?).ok()?;
    let mut rgb = [0.; 3];
    for (axis, value) in rgb.iter_mut().enumerate() {
        let at = 17usize.checked_add(axis.checked_mul(4)?)?;
        *value = f64::from(f32::from_le_bytes(payload.get(at..at.checked_add(4)?)?.try_into().ok()?));
        if !value.is_finite() || !(0.0..=1.0).contains(value) {
            return None;
        }
    }
    Some(rgb)
}
#[cfg(test)]
mod object_color_tests {
    use super::*;
    fn envelope(rgb: [f32; 3], text: &str) -> Vec<u8> {
        let mut b = 17u64.to_le_bytes().to_vec();
        b.extend((text.len() as u64).to_le_bytes());
        b.push(0x1d);
        for v in rgb {
            b.extend(v.to_le_bytes());
        }
        b.extend(text.as_bytes());
        b.extend([0, 0]);
        b
    }
    #[test]
    fn binary_object_rgb_and_material_text_are_distinct() {
        for text in ["", "face=(diffuse=R=0.1 G=0.2 B=0.3,transmission=R=0 G=0 B=0)"] {
            let payload = envelope([0.25, 0.5, 0.75], text);
            assert_eq!(binary_object_color(&payload, &[17]), Some([0.25, 0.5, 0.75]));
            assert_eq!(binary_object_color(&payload, &[]), None);
        }
    }
    #[test]
    fn invalid_binary_color_and_unadmitted_flags_are_unresolved() {
        for rgb in [[f32::NAN, 0., 0.], [f32::INFINITY, 0., 0.], [-0.1, 0., 0.], [1.1, 0., 0.]] {
            assert_eq!(binary_object_color(&envelope(rgb, ""), &[17]), None);
        }
        let payload = envelope([0.25, 0.5, 0.75], "");
        for n in 0..payload.len() {
            assert_eq!(binary_object_color(payload.get(..n).unwrap_or(&[]), &[17]), None);
        }
        for flags in [18u64, 0x1100] {
            let mut wrong = payload.clone();
            if let Some(v) = wrong.get_mut(..8) {
                v.copy_from_slice(&flags.to_le_bytes());
            }
            assert_eq!(source_object_color_candidate(&wrong), None);
        }
        let mut wrong = payload.clone();
        if let Some(v) = wrong.last_mut() {
            *v = 1;
        }
        assert_eq!(binary_object_color(&wrong, &[17]), None);
        let mut wrong = payload;
        if let Some(v) = wrong.get_mut(8) {
            *v = 1;
        }
        assert_eq!(binary_object_color(&wrong, &[17]), None);
    }

    #[test]
    fn empty_attribute_payload_retains_no_object_or_diffuse_color() -> Result<()> {
        assert_eq!(source_object_color_candidate(&[]), None);
        let mut raw = vec![1, 0x5d, 0xa2, 0x6d, 0x26];
        raw.extend(0u32.to_le_bytes());
        raw.push(0);
        raw.extend([0x59, 0xee, 0x21, 0xf4]);
        let mut callback = |_| true;
        let mut geometry = Geometry::new(&mut callback)?;
        let mut input = Input::new(&raw);
        let colors = geometry.attribute(&mut input)?;
        assert!(colors.object.is_none());
        assert!(colors.diffuse.is_none());
        assert_eq!(input.pos, raw.len());
        Ok(())
    }
}

#[cfg(test)]
mod color_tests {
    use super::*;
    fn envelope(text: &str) -> Vec<u8> {
        let mut b = vec![0; 8];
        b.extend((text.len() as u64).to_le_bytes());
        b.push(0x11);
        b.extend(text.as_bytes());
        b.extend([0, 0]);
        b
    }
    #[test]
    fn observed_synthetic_and_face_material_texts_have_candidates() {
        assert_eq!(source_diffuse_candidate(&envelope("diffuse=(r=0.800000 g=0.100000 b=0.050000)")), Some([0.8, 0.1, 0.05]));
        assert_eq!(source_diffuse_candidate(&envelope("face=(diffuse=R=0.498039 G=0.498039 B=0.498039,transmission=R=0 G=0 B=0)")), Some([0.498039; 3]));
    }
    #[test]
    fn hostile_or_ambiguous_color_text_does_not_resolve() {
        for text in [
            "diffuse=(r=NaN g=0 b=0)",
            "diffuse=(r=1.1 g=0 b=0)",
            "diffuse=(r=-0.1 g=0 b=0)",
            "diffuse=(r=0 g=0)",
            "diffuse=(r=1 g=0 b=0);diffuse=(r=0 g=1 b=0)",
        ] {
            assert_eq!(source_diffuse_candidate(&envelope(text)), None);
        }
        let raw = envelope("diffuse=(r=1 g=0 b=0)");
        for n in 0..raw.len() {
            assert_eq!(source_diffuse_candidate(raw.get(..n).unwrap_or(&[])), None);
        }
        assert_eq!(source_diffuse_candidate(b"diffuse=(r=1 g=0 b=0)"), None);
    }
}
