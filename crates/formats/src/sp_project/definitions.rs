use super::*;

pub(crate) struct Index {
    pub codes: BTreeSet<String>,
    pub profiles: BTreeSet<String>,
    pub users: BTreeMap<String, (u32, u32)>,
    pub code_tail_bytes: usize,
    pub storage: BTreeMap<String, serde_json::Value>,
}
fn identity(data: &[u8], at: usize, tag: &[u8], seen: &mut BTreeSet<String>) -> Result<(String, usize)> {
    if seen.len() >= RECORD_LIMIT {
        return Err(fail("limit", "Definition count budget"));
    }
    marker(data, at, tag)?;
    marker(data, checked_add(at, 4)?, &[0; 8])?;
    let raw = bytes(data, checked_add(at, 12)?, 16)?;
    if raw.iter().all(|v| *v == 0) {
        return Err(fail("invalid_identity", "Nil definition GUID"));
    }
    let guid = guid_at(data, checked_add(at, 12)?)?;
    if !seen.insert(guid.clone()) {
        return Err(fail("invalid_identity", "Duplicate definition GUID"));
    }
    let (_, end) = string_at(data, checked_add(at, 28)?)?;
    Ok((guid, end))
}
fn root(data: &[u8]) -> Result<usize> {
    marker(data, 24, OPEN)?;
    count_at(data, 28)
}
fn finish(data: &[u8], at: usize) -> Result<()> {
    marker(data, at, CLOSE)?;
    if checked_add(at, 4)? != data.len() {
        return Err(fail("unsupported_layout", "Unconsumed definition bytes"));
    }
    Ok(())
}
fn code_value(data: &[u8], at: usize, depth: usize, all: &mut BTreeSet<String>, values: &mut BTreeSet<String>) -> Result<usize> {
    if depth > 64 || all.len() >= RECORD_LIMIT {
        return Err(fail("limit", "Definition nesting/count budget"));
    }
    let (guid, end) = identity(data, at, &[0x5b, 0xe6, 0x84, 0xff], all)?;
    values.insert(guid);
    let (_, desc_end) = string_at(data, end)?;
    marker(data, desc_end, OPEN)?;
    let n = count_at(data, checked_add(desc_end, 4)?)?;
    let mut pos = checked_add(desc_end, 8)?;
    for _ in 0..n {
        pos = code_value(data, pos, depth + 1, all, values)?;
    }
    marker(data, pos, CLOSE)?;
    marker(data, checked_add(pos, 16)?, &[0x17, 0xb2, 0x21, 0xf0])?;
    bytes(data, pos, 24)?;
    checked_add(pos, 24)
}
pub(crate) fn parse(c: &Container) -> Result<Index> {
    let users_data = c.decoded.get("User Fields").ok_or_else(|| fail("missing_dependency", "User Fields definitions missing"))?;
    let mut users = BTreeMap::new();
    let mut storage = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut pos = 32;
    for _ in 0..root(users_data)? {
        let (guid, end) = identity(users_data, pos, &[0xe6, 0x34, 0x7c, 0x4b], &mut seen)?;
        let kind = u32_at(users_data, end)?;
        let domain = u32_at(users_data, checked_add(end, 4)?)?;
        marker(users_data, checked_add(end, 26)?, &[255, 254, 255, 0])?;
        marker(users_data, checked_add(end, 38)?, &[255, 254, 255, 0])?;
        marker(users_data, checked_add(end, 42)?, &[0x02, 0xd2, 0x02, 0xbb])?;
        bytes(users_data, end, 50)?;
        storage.insert(guid.clone(), serde_json::json!({"section":"User Fields","offset":pos,"name":string_at(users_data,checked_add(pos,28)?)?.0,"kind_storage":kind,"domain_storage":domain,"raw_tail_hex":hex(bytes(users_data,end,50)?)}));
        users.insert(guid, (kind, domain));
        pos = checked_add(end, 50)?;
    }
    finish(users_data, pos)?;
    let profile_data = c.decoded.get("Use Profiles").ok_or_else(|| fail("missing_dependency", "Profile definitions missing"))?;
    let mut profiles = BTreeSet::new();
    pos = 32;
    for _ in 0..root(profile_data)? {
        let (guid, end) = identity(profile_data, pos, &[0xc1, 0xcf, 0xd6, 0xe8], &mut profiles)?;
        for at in [6, 69, 176] {
            marker(profile_data, checked_add(end, at)?, &[0x10, 0x48, 0x69, 0x23])?;
        }
        for at in [65, 128, 235] {
            marker(profile_data, checked_add(end, at)?, &[0x6c, 0x4a, 0x92, 0x90])?;
        }
        marker(profile_data, checked_add(end, 132)?, OPEN)?;
        marker(profile_data, checked_add(end, 136)?, &[0; 4])?;
        marker(profile_data, checked_add(end, 140)?, CLOSE)?;
        marker(profile_data, checked_add(end, 239)?, &[0x3d, 0xc8, 0xb0, 0x03])?;
        bytes(profile_data, end, 247)?;
        storage.insert(guid, serde_json::json!({"section":"Use Profiles","offset":pos,"name":string_at(profile_data,checked_add(pos,28)?)?.0,"raw_tail_hex":hex(bytes(profile_data,end,247)?)}));
        pos = checked_add(end, 247)?;
    }
    finish(profile_data, pos)?;
    let code_data = c.decoded.get("Activity Codes").ok_or_else(|| fail("missing_dependency", "Code definitions missing"))?;
    let mut codes = BTreeSet::new();
    let mut all = BTreeSet::new();
    pos = 32;
    for _ in 0..root(code_data)? {
        let (_, end) = identity(code_data, pos, &[0x8b, 0x12, 0x9e, 0xf2], &mut all)?;
        marker(code_data, checked_add(end, 8)?, OPEN)?;
        let n = count_at(code_data, checked_add(end, 12)?)?;
        pos = checked_add(end, 16)?;
        for _ in 0..n {
            pos = code_value(code_data, pos, 0, &mut all, &mut codes)?;
        }
        marker(code_data, pos, CLOSE)?;
        marker(code_data, checked_add(pos, 8)?, &[0x47, 0x64, 0x60, 0x46])?;
        bytes(code_data, pos, 16)?;
        pos = checked_add(pos, 16)?;
    }
    marker(code_data, pos, CLOSE)?;
    pos = checked_add(pos, 4)?;
    let code_tail_bytes = code_data.len().checked_sub(pos).ok_or_else(|| fail("truncated", "Code suffix"))?;
    Ok(Index { codes, profiles, users, code_tail_bytes, storage })
}
pub(crate) fn check_user(index: &Index, value: &UserValue, domain: Option<u32>) -> Result<()> {
    let expected = match value.variant {
        3 => 1,
        4 => 2,
        6 => 3,
        _ => return Err(fail("unsupported_layout", "Unknown user-value type")),
    };
    match index.users.get(&value.definition_guid) {
        Some((kind, actual_domain)) if *kind == expected && domain.is_none_or(|d| d == *actual_domain) => Ok(()),
        _ => Err(fail("missing_dependency", "User definition absent or type/domain mismatched")),
    }
}
