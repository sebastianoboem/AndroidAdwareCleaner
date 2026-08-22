//! Minimal `resources.arsc` lookup: resolve a resource ID to a string
//! (app label or drawable path) using the default locale when present.

const RES_STRING_POOL: u16 = 0x0001;
const RES_TABLE_PACKAGE: u16 = 0x0200;
const RES_TABLE_TYPE: u16 = 0x0201;
const NO_ENTRY: u32 = 0xFFFF_FFFF;
const VALUE_STRING: u8 = 0x03;
const VALUE_REFERENCE: u8 = 0x01;
const UTF8_FLAG: u32 = 1 << 8;
const COMPLEX_FLAG: u16 = 0x0001;
const COMPACT_FLAG: u16 = 0x0008;

pub fn parse_resource_ref(raw: &str) -> Option<u32> {
    let marker = "Reference/";
    let rest = raw.find(marker).map(|i| &raw[i + marker.len()..])?;
    let token: String = rest
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == 'x')
        .collect();
    if token.is_empty() {
        return None;
    }
    if let Some(hex) = token.strip_prefix("0x").or_else(|| token.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        token.parse().ok()
    }
}

pub fn lookup_string(arsc: &[u8], resid: u32) -> Option<String> {
    match lookup_res_value(arsc, resid)? {
        ResValue::String(s) if !s.is_empty() => Some(s),
        ResValue::Reference(id) if id != 0 && id != resid => lookup_string(arsc, id),
        _ => None,
    }
}

pub fn lookup_all_strings(arsc: &[u8], resid: u32) -> Vec<String> {
    let mut out = Vec::new();
    collect_all_strings(arsc, resid, &mut out, &mut Vec::new());
    out
}

fn collect_all_strings(arsc: &[u8], resid: u32, out: &mut Vec<String>, stack: &mut Vec<u32>) {
    if stack.contains(&resid) {
        return;
    }
    stack.push(resid);
    for value in collect_res_values(arsc, resid) {
        match value {
            ResValue::String(s) if !s.is_empty() => {
                if !out.contains(&s) {
                    out.push(s);
                }
            }
            ResValue::Reference(id) if id != 0 => {
                collect_all_strings(arsc, id, out, stack);
            }
            _ => {}
        }
    }
    stack.pop();
}

enum ResValue {
    String(String),
    Reference(u32),
}

fn lookup_res_value(data: &[u8], resid: u32) -> Option<ResValue> {
    let mut default_hit = None;
    let mut any_hit = None;
    for (is_default, value) in collect_res_values_with_locale(data, resid) {
        if is_default && default_hit.is_none() {
            default_hit = Some(value);
        } else if any_hit.is_none() {
            any_hit = Some(value);
        }
        if default_hit.is_some() && any_hit.is_some() {
            break;
        }
    }
    default_hit.or(any_hit)
}

fn collect_res_values(data: &[u8], resid: u32) -> Vec<ResValue> {
    collect_res_values_with_locale(data, resid)
        .into_iter()
        .map(|(_, v)| v)
        .collect()
}

fn collect_res_values_with_locale(data: &[u8], resid: u32) -> Vec<(bool, ResValue)> {
    let mut out = Vec::new();
    if data.len() < 12 {
        return out;
    }
    let want_pkg = ((resid >> 24) & 0xff) as u8;
    let want_type = ((resid >> 16) & 0xff) as u8;
    let want_entry = resid & 0xffff;

    let Some(table_hs) = u16_at(data, 2).map(|v| v as usize) else {
        return out;
    };
    let Some(global_pool_off) = find_chunk(data, table_hs, data.len(), RES_STRING_POOL) else {
        return out;
    };

    let mut off = table_hs;
    while let Some((typ, hs, size)) = chunk_at(data, off) {
        if typ == RES_TABLE_PACKAGE {
            if data.get(off + 8).copied() == Some(want_pkg) {
                collect_in_package(
                    data,
                    off + hs,
                    off + size,
                    global_pool_off,
                    want_type,
                    want_entry,
                    &mut out,
                );
            }
        }
        off += size;
        if off >= data.len() {
            break;
        }
    }
    out
}

fn collect_in_package(
    data: &[u8],
    start: usize,
    end: usize,
    global_pool_off: usize,
    want_type: u8,
    want_entry: u32,
    out: &mut Vec<(bool, ResValue)>,
) {
    let mut off = start;
    while let Some((typ, hs, size)) = chunk_at(data, off) {
        if off + size > end {
            break;
        }
        if typ == RES_TABLE_TYPE {
            if data.get(off + 8).copied() == Some(want_type) {
                out.extend(read_type_entry(
                    data,
                    off,
                    hs,
                    size,
                    want_entry,
                    global_pool_off,
                ));
            }
        }
        off += size;
        if off >= end {
            break;
        }
    }
}

const TYPE_FLAG_SPARSE: u8 = 0x01;

fn read_type_entry(
    data: &[u8],
    off: usize,
    hs: usize,
    _size: usize,
    want_entry: u32,
    global_pool_off: usize,
) -> Vec<(bool, ResValue)> {
    let Some(type_flags) = data.get(off + 9).copied() else {
        return Vec::new();
    };
    let Some(entry_count) = u32_at(data, off + 12) else {
        return Vec::new();
    };
    let Some(entries_start) = u32_at(data, off + 16).map(|v| v as usize) else {
        return Vec::new();
    };
    let epos = if type_flags & TYPE_FLAG_SPARSE != 0 {
        match sparse_entry_pos(data, off, hs, entries_start, entry_count, want_entry) {
            Some(p) => p,
            None => return Vec::new(),
        }
    } else {
        if want_entry >= entry_count {
            return Vec::new();
        }
        let Some(eoff) = u32_at(data, off + hs + 4 * want_entry as usize) else {
            return Vec::new();
        };
        if eoff == NO_ENTRY {
            return Vec::new();
        }
        off + entries_start + eoff as usize
    };
    let Some(flags) = u16_at(data, epos + 2) else {
        return Vec::new();
    };
    let lang0 = *data.get(off + 28).unwrap_or(&0);
    let lang1 = *data.get(off + 29).unwrap_or(&0);
    let is_default = lang0 == 0 && lang1 == 0;
    if flags & COMPLEX_FLAG != 0 {
        return read_complex_values(data, epos, flags, global_pool_off)
            .into_iter()
            .map(|v| (is_default, v))
            .collect();
    }
    // ResTable_entry is 8 bytes, then Res_value { size:u16, res0:u8, dataType:u8, data:u32 }.
    // FLAG_COMPACT packs dataType into flags' high byte and data into the key field.
    let parsed = if flags & COMPACT_FLAG != 0 {
        data.get(epos + 3).copied().and_then(|dtype| {
            u32_at(data, epos + 4).map(|value_data| (dtype, value_data))
        })
    } else {
        data.get(epos + 11).copied().and_then(|dtype| {
            u32_at(data, epos + 12).map(|value_data| (dtype, value_data))
        })
    };
    let Some((dtype, value_data)) = parsed else {
        return Vec::new();
    };
    let value = match dtype {
        VALUE_STRING => string_at_pool(data, global_pool_off, value_data).map(ResValue::String),
        VALUE_REFERENCE => Some(ResValue::Reference(value_data)),
        _ => None,
    };
    value.into_iter().map(|v| (is_default, v)).collect()
}

const ATTR_FOREGROUND: u32 = 0x0101_0200;

fn read_complex_values(
    data: &[u8],
    epos: usize,
    flags: u16,
    global_pool_off: usize,
) -> Vec<ResValue> {
    if flags & COMPACT_FLAG != 0 {
        return Vec::new();
    }
    let Some(count) = u32_at(data, epos + 12).map(|v| v as usize) else {
        return Vec::new();
    };
    let mut maps = Vec::new();
    let mut p = epos + 16;
    for _ in 0..count {
        let Some(name) = u32_at(data, p) else {
            break;
        };
        let Some(size) = u16_at(data, p + 4).map(|v| v as usize) else {
            break;
        };
        let Some(dtype) = data.get(p + 7).copied() else {
            break;
        };
        let Some(value_data) = u32_at(data, p + 8) else {
            break;
        };
        p += size.max(12);
        let value = match dtype {
            VALUE_STRING => string_at_pool(data, global_pool_off, value_data).map(ResValue::String),
            VALUE_REFERENCE if value_data != 0 => Some(ResValue::Reference(value_data)),
            _ => None,
        };
        if let Some(v) = value {
            maps.push((name, v));
        }
    }
    maps.sort_by_key(|(name, _)| if *name == ATTR_FOREGROUND { 0 } else { 1 });
    maps.into_iter().map(|(_, v)| v).collect()
}

fn sparse_entry_pos(
    data: &[u8],
    off: usize,
    hs: usize,
    entries_start: usize,
    entry_count: u32,
    want_entry: u32,
) -> Option<usize> {
    for i in 0..entry_count as usize {
        let idx = u16_at(data, off + hs + 4 * i)? as u32;
        let off4 = u16_at(data, off + hs + 4 * i + 2)? as usize;
        if idx == want_entry {
            return Some(off + entries_start + off4 * 4);
        }
    }
    None
}

fn find_chunk(data: &[u8], start: usize, end: usize, want: u16) -> Option<usize> {
    let mut off = start;
    while let Some((typ, _, size)) = chunk_at(data, off) {
        if typ == want {
            return Some(off);
        }
        off += size;
        if off >= end {
            break;
        }
    }
    None
}

fn chunk_at(data: &[u8], off: usize) -> Option<(u16, usize, usize)> {
    if off + 8 > data.len() {
        return None;
    }
    let typ = u16_at(data, off)?;
    let hs = u16_at(data, off + 2)? as usize;
    let size = u32_at(data, off + 4)? as usize;
    if size < 8 || off + size > data.len() {
        return None;
    }
    Some((typ, hs, size))
}

fn string_at_pool(data: &[u8], pool_off: usize, index: u32) -> Option<String> {
    let string_count = u32_at(data, pool_off + 8)?;
    if index >= string_count {
        return None;
    }
    let flags = u32_at(data, pool_off + 16)?;
    let strings_start = u32_at(data, pool_off + 20)? as usize;
    let offset = u32_at(data, pool_off + 28 + 4 * index as usize)? as usize;
    let mut p = pool_off + strings_start + offset;
    if flags & UTF8_FLAG != 0 {
        let (_, p1) = utf8_len(data, p)?;
        let (blen, p2) = utf8_len(data, p1)?;
        let bytes = data.get(p2..p2 + blen)?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    } else {
        let (n, p1) = utf16_len(data, p)?;
        p = p1;
        let mut chars = Vec::with_capacity(n);
        for _ in 0..n {
            chars.push(u16_at(data, p)?);
            p += 2;
        }
        String::from_utf16(&chars).ok()
    }
}

fn utf8_len(data: &[u8], mut p: usize) -> Option<(usize, usize)> {
    let mut n = *data.get(p)? as usize;
    p += 1;
    if n & 0x80 != 0 {
        n = ((n & 0x7f) << 8) | (*data.get(p)? as usize);
        p += 1;
    }
    Some((n, p))
}

fn utf16_len(data: &[u8], mut p: usize) -> Option<(usize, usize)> {
    let mut n = u16_at(data, p)? as usize;
    p += 2;
    if n & 0x8000 != 0 {
        n = ((n & 0x7fff) << 16) | (u16_at(data, p)? as usize);
        p += 2;
    }
    Some((n, p))
}

fn u16_at(data: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(off..off + 4)?.try_into().ok()?))
}

impl Clone for ResValue {
    fn clone(&self) -> Self {
        match self {
            ResValue::String(s) => ResValue::String(s.clone()),
            ResValue::Reference(id) => ResValue::Reference(*id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_decimal_resource_reference() {
        assert_eq!(
            parse_resource_ref("ResourceValueType::Reference/2131755036"),
            Some(0x7f10001c)
        );
    }

    #[test]
    fn parses_hex_resource_reference() {
        assert_eq!(
            parse_resource_ref("ResourceValueType::Reference/0x7f10001c"),
            Some(0x7f10001c)
        );
    }

    #[test]
    fn ignores_non_reference_values() {
        assert_eq!(parse_resource_ref("HTML Viewer"), None);
        assert_eq!(parse_resource_ref(""), None);
    }

    const ICON_RES: u32 = 0x7f110021;

    #[test]
    fn lookup_reads_sparse_type_entries() {
        let arsc = build_arsc("res/Gmail.png", true);
        assert_eq!(
            lookup_all_strings(&arsc, ICON_RES),
            vec!["res/Gmail.png".to_string()]
        );
    }

    #[test]
    fn lookup_still_reads_dense_type_entries() {
        let arsc = build_arsc("res/legacy.png", false);
        assert_eq!(
            lookup_all_strings(&arsc, ICON_RES),
            vec!["res/legacy.png".to_string()]
        );
    }

    #[test]
    fn lookup_all_strings_follows_reference_across_configs() {
        let arsc = build_referenced_icon_arsc();
        let mut paths = lookup_all_strings(&arsc, ICON_RES);
        paths.sort();
        assert_eq!(
            paths,
            vec!["res/a.png".to_string(), "res/b.png".to_string()]
        );
    }

    fn build_arsc(path: &str, sparse: bool) -> Vec<u8> {
        let pool = utf8_pool(&[path]);
        let type_chunk = if sparse {
            sparse_type_chunk()
        } else {
            dense_type_chunk()
        };
        let mut package = Vec::new();
        package.extend_from_slice(&0x0200u16.to_le_bytes());
        package.extend_from_slice(&288u16.to_le_bytes());
        package.extend_from_slice(&(288u32 + type_chunk.len() as u32).to_le_bytes());
        package.extend_from_slice(&0x7fu32.to_le_bytes());
        package.extend(std::iter::repeat_n(0u8, 256));
        package.extend_from_slice(&[0u8; 20]);
        debug_assert_eq!(package.len(), 288);
        package.extend_from_slice(&type_chunk);

        let total = 12 + pool.len() + package.len();
        let mut out = Vec::new();
        out.extend_from_slice(&0x0002u16.to_le_bytes());
        out.extend_from_slice(&12u16.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&pool);
        out.extend_from_slice(&package);
        out
    }

    fn build_referenced_icon_arsc() -> Vec<u8> {
        let pool = utf8_pool(&["res/a.png", "res/b.png"]);
        let chunk_a = sparse_entries(&[
            (0x0021, VALUE_REFERENCE, 0x7f110032),
            (0x0032, VALUE_STRING, 0),
        ]);
        let chunk_b = sparse_entries(&[(0x0032, VALUE_STRING, 1)]);
        let inner_len = chunk_a.len() + chunk_b.len();
        let mut package = Vec::new();
        package.extend_from_slice(&0x0200u16.to_le_bytes());
        package.extend_from_slice(&288u16.to_le_bytes());
        package.extend_from_slice(&(288u32 + inner_len as u32).to_le_bytes());
        package.extend_from_slice(&0x7fu32.to_le_bytes());
        package.extend(std::iter::repeat_n(0u8, 256));
        package.extend_from_slice(&[0u8; 20]);
        package.extend_from_slice(&chunk_a);
        package.extend_from_slice(&chunk_b);
        let total = 12 + pool.len() + package.len();
        let mut out = Vec::new();
        out.extend_from_slice(&0x0002u16.to_le_bytes());
        out.extend_from_slice(&12u16.to_le_bytes());
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&pool);
        out.extend_from_slice(&package);
        out
    }

    fn utf8_pool(strings: &[&str]) -> Vec<u8> {
        let header_size = 28u16;
        let mut blob = Vec::new();
        let mut offsets = Vec::new();
        for s in strings {
            offsets.push(blob.len() as u32);
            let bytes = s.as_bytes();
            blob.push(bytes.len() as u8);
            blob.push(bytes.len() as u8);
            blob.extend_from_slice(bytes);
            blob.push(0);
        }
        while blob.len() % 4 != 0 {
            blob.push(0);
        }
        let strings_start = header_size as u32 + 4 * strings.len() as u32;
        let size = strings_start + blob.len() as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&0x0001u16.to_le_bytes());
        out.extend_from_slice(&header_size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0x0000_0100u32.to_le_bytes());
        out.extend_from_slice(&strings_start.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for o in offsets {
            out.extend_from_slice(&o.to_le_bytes());
        }
        out.extend_from_slice(&blob);
        out
    }

    fn sparse_type_chunk() -> Vec<u8> {
        sparse_entries(&[(0x0021, VALUE_STRING, 0)])
    }

    fn sparse_entries(entries: &[(u16, u8, u32)]) -> Vec<u8> {
        let n = entries.len();
        let hs = 84u16;
        let entries_start = 84 + n * 4;
        let size = (entries_start + 16 * n) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&0x0201u16.to_le_bytes());
        out.extend_from_slice(&hs.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.push(0x11);
        out.push(0x01);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(n as u32).to_le_bytes());
        out.extend_from_slice(&(entries_start as u32).to_le_bytes());
        out.extend(std::iter::repeat_n(0u8, 64));
        for (i, (idx, _, _)) in entries.iter().enumerate() {
            out.extend_from_slice(&idx.to_le_bytes());
            out.extend_from_slice(&((i as u16) * 4).to_le_bytes());
        }
        for &(_, dtype, data) in entries {
            out.extend_from_slice(&value_entry(dtype, data));
        }
        out
    }

    fn value_entry(dtype: u8, data: u32) -> [u8; 16] {
        let mut e = [0u8; 16];
        e[0] = 8;
        e[8] = 8;
        e[11] = dtype;
        e[12..16].copy_from_slice(&data.to_le_bytes());
        e
    }

    fn dense_type_chunk() -> Vec<u8> {
        let hs = 84u16;
        let entry_count = 0x22u32;
        let offset_bytes = (entry_count as usize) * 4;
        let entries_start = 84 + offset_bytes;
        let size = (entries_start + 16) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(&0x0201u16.to_le_bytes());
        out.extend_from_slice(&hs.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.push(0x11);
        out.push(0x00);
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&entry_count.to_le_bytes());
        out.extend_from_slice(&(entries_start as u32).to_le_bytes());
        out.extend(std::iter::repeat_n(0u8, 64));
        for i in 0..entry_count {
            let off = if i == 0x21 { 0u32 } else { NO_ENTRY };
            out.extend_from_slice(&off.to_le_bytes());
        }
        out.extend_from_slice(&simple_string_entry());
        out
    }

    fn simple_string_entry() -> [u8; 16] {
        let mut e = [0u8; 16];
        e[0] = 8;
        e[8] = 8;
        e[11] = VALUE_STRING;
        e
    }
}
