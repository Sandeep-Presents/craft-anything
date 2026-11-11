use super::*;
const OPAQUE_REFERENCE_LIMIT: usize = 200_000;
struct Reader<'a> {
    data: &'a [u8],
    rows: Vec<ResourceIdentity>,
    seen: BTreeSet<String>,
    opaque: usize,
    values: usize,
    costs: usize,
}
impl Reader<'_> {
    fn skip(&self, at: usize, n: usize) -> Result<usize> {
        bytes(self.data, at, n)?;
        checked_add(at, n)
    }
    fn guid_list(&mut self, at: usize, retain: bool) -> Result<(usize, usize, Vec<NodeBinding>)> {
        marker(self.data, at, &[0xd0, 0xbc, 0xa1, 0x06])?;
        let n = usize::try_from(u32_at(self.data, checked_add(at, 4)?)?).map_err(|_| fail("limit", "Opaque list count"))?;
        self.opaque = checked_add(self.opaque, n)?;
        if self.opaque > OPAQUE_REFERENCE_LIMIT {
            return Err(fail("limit", "Opaque Resource GUID-list budget exceeded"));
        }
        let len = n.checked_mul(20).ok_or_else(|| fail("limit", "GUID list arithmetic"))?;
        let end = self.skip(checked_add(at, 8)?, len)?;
        marker(self.data, end, &[0x2c, 0x57, 0xd9, 0xda])?;
        let mut bindings = Vec::new();
        if retain {
            for i in 0..n {
                let offset = checked_add(checked_add(at, 8)?, i.checked_mul(20).ok_or_else(|| fail("limit", "Binding offset"))?)?;
                bindings.push(NodeBinding {
                    node_guid: guid_at(self.data, offset)?,
                    ordinal_storage: u32_at(self.data, checked_add(offset, 16)?)?,
                    source_offset: offset,
                });
            }
        }
        Ok((checked_add(end, 4)?, n, bindings))
    }
    fn cost(&mut self, at: usize) -> Result<(usize, serde_json::Value)> {
        self.costs = checked_add(self.costs, 1)?;
        if self.costs > RECORD_LIMIT {
            return Err(fail("limit", "Resource cost budget"));
        }
        marker(self.data, at, &[0xbd, 0xaf, 0x1b, 0x70])?;
        marker(self.data, checked_add(at, 4)?, &[0; 8])?;
        let mut pos = self.skip(at, 40)?;
        marker(self.data, pos, OPEN)?;
        let n = count_at(self.data, checked_add(pos, 4)?)?;
        pos = checked_add(pos, 8)?;
        let mut items = Vec::new();
        for _ in 0..n {
            self.costs = checked_add(self.costs, 1)?;
            if self.costs > RECORD_LIMIT {
                return Err(fail("limit", "Resource cost-item budget"));
            }
            marker(self.data, pos, &[0x9c, 0x0e, 0xe3, 0x04])?;
            marker(self.data, checked_add(pos, 4)?, &[0; 8])?;
            let (description, end) = string_at(self.data, checked_add(pos, 28)?)?;
            items.push(serde_json::json!({"offset":pos,"description":description,"amount_storage":finite_at(self.data,end)?,"type_storage":u32_at(self.data,checked_add(end,8)?)?,"raw_record_hex":hex(bytes(self.data,pos,checked_add(end,36)?.checked_sub(pos).ok_or_else(||fail("limit","Cost item range"))?)?)}));
            finite_at(self.data, end)?;
            marker(self.data, checked_add(end, 12)?, &[0; 16])?;
            marker(self.data, checked_add(end, 28)?, &[0x78, 0xe2, 0x44, 0xa1, 0, 0, 0, 0])?;
            pos = checked_add(end, 36)?;
        }
        marker(self.data, pos, CLOSE)?;
        marker(self.data, checked_add(pos, 4)?, &[0xb9, 0xa7, 0x91, 0x73])?;
        Ok((self.skip(pos, 12)?, serde_json::json!({"offset":at,"raw_header_hex":hex(bytes(self.data,at,40)?),"items":items})))
    }
    fn resource(&mut self, start: usize, parent: Option<&str>, depth: usize) -> Result<usize> {
        if depth > 64 || self.seen.len() >= RECORD_LIMIT {
            return Err(fail("limit", "Resource depth/count budget"));
        }
        marker(self.data, start, &[0x31, 0x5c, 0xa8, 0x57])?;
        marker(self.data, checked_add(start, 4)?, &[0; 8])?;
        let raw = bytes(self.data, checked_add(start, 12)?, 16)?;
        if raw.iter().all(|b| *b == 0) {
            return Err(fail("invalid_identity", "Nil Resource GUID"));
        }
        let guid = guid_at(self.data, checked_add(start, 12)?)?;
        if !self.seen.insert(guid.clone()) {
            return Err(fail("invalid_identity", "Duplicate Resource GUID"));
        }
        let (name, name_end) = string_at(self.data, checked_add(start, 28)?)?;
        let (secondary_text_storage, end) = string_at(self.data, name_end)?;
        let raw_identity_window_hex = hex(bytes(self.data, end, 32)?);
        let pos = self.skip(end, 32)?;
        let (pos, a, geometry_bindings) = self.guid_list(pos, true)?;
        let pre_children = bytes(self.data, pos, 8)?;
        let raw_pre_children_hex = hex(pre_children);
        let kind_storage_byte = *pre_children.get(4).ok_or_else(|| fail("truncated", "Resource kind window"))?;
        let mut pos = self.skip(pos, 8)?;
        marker(self.data, pos, OPEN)?;
        let children = count_at(self.data, checked_add(pos, 4)?)?;
        pos = checked_add(pos, 8)?;
        for _ in 0..children {
            pos = self.resource(pos, Some(&guid), depth + 1)?;
        }
        marker(self.data, pos, CLOSE)?;
        let post_start = checked_add(pos, 4)?;
        let post_children = bytes(self.data, post_start, 20)?;
        let raw_post_children_hex = hex(post_children);
        let unit_storage_byte = *post_children.get(14).ok_or_else(|| fail("truncated", "Resource unit window"))?;
        pos = self.skip(post_start, 20)?;
        let (text_before_values, next) = string_at(self.data, pos)?;
        pos = next;
        let (user_values, extra) = user_values(self.data, pos, RECORD_LIMIT.saturating_sub(self.values))?;
        self.values = checked_add(self.values, user_values.len())?;
        pos = self.skip(pos, checked_add(extra, 1)?)?;
        let (text_after_values, next) = string_at(self.data, pos)?;
        let (text_before_costs, next) = string_at(self.data, next)?;
        pos = next;
        marker(self.data, pos, OPEN)?;
        let cost_count = count_at(self.data, checked_add(pos, 4)?)?;
        pos = checked_add(pos, 8)?;
        let mut cost_storage_groups = Vec::new();
        for _ in 0..cost_count {
            let (end, group) = self.cost(pos)?;
            pos = end;
            cost_storage_groups.push(group);
        }
        marker(self.data, pos, CLOSE)?;
        pos = self.skip(checked_add(pos, 4)?, 31)?;
        let (next, b, _) = self.guid_list(pos, false)?;
        pos = self.skip(next, 5)?;
        let (next, c, _) = self.guid_list(pos, false)?;
        pos = self.skip(next, 16)?;
        marker(self.data, pos, &[0xad, 0xe2, 0xb3, 0x4c])?;
        let end = self.skip(pos, 8)?;
        self.rows.push(ResourceIdentity {
            offset: start,
            guid,
            name,
            parent_guid: parent.map(str::to_owned),
            secondary_text_storage,
            geometry_bindings,
            opaque_text_fields: [text_before_values, text_after_values, text_before_costs],
            bytes: end - start,
            child_count: children,
            opaque_guid_list_counts: [a, b, c],
            user_values,
            cost_record_count: cost_count,
            raw_prefix_hex: hex(bytes(self.data, start, name_end.checked_sub(start).ok_or_else(|| fail("limit", "Resource prefix"))?)?),
            cost_storage_groups,
            raw_identity_window_hex,
            raw_pre_children_hex,
            raw_post_children_hex,
            kind_storage_byte,
            unit_storage_byte,
        });
        Ok(end)
    }
}
pub(crate) fn parse(data: &[u8]) -> Result<Vec<ResourceIdentity>> {
    marker(data, 24, OPEN)?;
    let n = count_at(data, 28)?;
    let mut reader = Reader { data, rows: Vec::new(), seen: BTreeSet::new(), opaque: 0, values: 0, costs: 0 };
    let mut pos = 32;
    for _ in 0..n {
        pos = reader.resource(pos, None, 0)?;
    }
    marker(data, pos, CLOSE)?;
    if checked_add(pos, 4)? != data.len() {
        return Err(fail("unsupported_layout", "Unconsumed Resource bytes"));
    }
    reader.rows.sort_by_key(|r| r.offset);
    Ok(reader.rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cost_storage_keeps_raw_header_and_item_provenance() {
        let mut raw = vec![0xbd, 0xaf, 0x1b, 0x70];
        raw.extend([0; 36]);
        raw.extend(OPEN);
        raw.extend(1u32.to_le_bytes());
        raw.extend([0x9c, 0x0e, 0xe3, 0x04]);
        raw.extend([0; 8]);
        raw.extend([1; 16]);
        raw.extend([255, 254, 255, 1]);
        raw.extend(u16::from(b'X').to_le_bytes());
        raw.extend(7.5f64.to_le_bytes());
        raw.extend(1u32.to_le_bytes());
        raw.extend([0; 16]);
        raw.extend([0x78, 0xe2, 0x44, 0xa1, 0, 0, 0, 0]);
        raw.extend(CLOSE);
        raw.extend([0xb9, 0xa7, 0x91, 0x73]);
        raw.extend([0; 4]);
        let mut reader = Reader { data: &raw, rows: Vec::new(), seen: BTreeSet::new(), opaque: 0, values: 0, costs: 0 };
        let result = reader.cost(0);
        assert!(result.is_ok(), "{result:?}");
        if let Ok((end, group)) = result {
            assert_eq!(end, raw.len());
            assert_eq!(group.get("raw_header_hex").and_then(|v| v.as_str()).map(str::len), Some(80));
            let item = group.get("items").and_then(|v| v.as_array()).and_then(|v| v.first());
            assert_eq!(item.and_then(|v| v.get("description")).and_then(|v| v.as_str()), Some("X"));
            assert_eq!(item.and_then(|v| v.get("amount_storage")).and_then(|v| v.as_f64()), Some(7.5));
        }
    }

    #[test]
    fn resource_kind_and_unit_storage_use_framed_windows() {
        fn guid_list(b: &mut Vec<u8>) {
            b.extend([0xd0, 0xbc, 0xa1, 0x06]);
            b.extend(0u32.to_le_bytes());
            b.extend([0x2c, 0x57, 0xd9, 0xda]);
        }
        fn empty_list(b: &mut Vec<u8>) {
            b.extend(OPEN);
            b.extend(0u32.to_le_bytes());
            b.extend(CLOSE);
        }
        fn empty_text(b: &mut Vec<u8>) {
            b.extend([255, 254, 255, 0]);
        }
        let mut raw = vec![0; 24];
        raw.extend(OPEN);
        raw.extend(1u32.to_le_bytes());
        raw.extend([0x31, 0x5c, 0xa8, 0x57]);
        raw.extend([0; 8]);
        raw.extend([1; 16]);
        empty_text(&mut raw);
        empty_text(&mut raw);
        raw.extend([0; 32]);
        guid_list(&mut raw);
        raw.extend([0, 0, 0, 0, 4, 0, 0, 0]);
        empty_list(&mut raw);
        let mut post = [0; 20];
        if let Some(v) = post.get_mut(14) {
            *v = 7;
        }
        raw.extend(post);
        empty_text(&mut raw);
        raw.push(0);
        empty_text(&mut raw);
        empty_text(&mut raw);
        empty_list(&mut raw);
        raw.extend([0; 31]);
        guid_list(&mut raw);
        raw.extend([0; 5]);
        guid_list(&mut raw);
        raw.extend([0; 16]);
        raw.extend([0xad, 0xe2, 0xb3, 0x4c]);
        raw.extend([0; 4]);
        raw.extend(CLOSE);
        let result = parse(&raw);
        assert!(result.is_ok());
        if let Ok(rows) = result
            && let Some(row) = rows.first()
        {
            assert_eq!(row.kind_storage_byte, 4);
            assert_eq!(row.unit_storage_byte, 7);
            assert_eq!(row.raw_pre_children_hex.len(), 16);
            assert_eq!(row.raw_post_children_hex.len(), 40);
        }
    }
}
