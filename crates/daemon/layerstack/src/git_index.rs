#[derive(Debug, Clone, PartialEq, Eq)]
struct GitIndexSemantic {
    entries: Vec<GitIndexEntrySemantic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitIndexEntrySemantic {
    path: Vec<u8>,
    mode: u32,
    object_id: [u8; 20],
    flags: u16,
    extended_flags: Option<u16>,
}

pub(crate) fn git_index_semantically_unchanged(
    base_bytes: Option<&[u8]>,
    base_exists: bool,
    new_bytes: &[u8],
) -> bool {
    if base_exists && base_bytes == Some(new_bytes) {
        return true;
    }

    let Some(new_index) = parse_git_index_semantic(new_bytes) else {
        return false;
    };
    match (base_exists, base_bytes.and_then(parse_git_index_semantic)) {
        (false, _) => new_index.entries.is_empty(),
        (true, Some(base_index)) => new_index == base_index,
        (true, None) => false,
    }
}

fn parse_git_index_semantic(bytes: &[u8]) -> Option<GitIndexSemantic> {
    if bytes.len() < 12 || bytes.get(0..4)? != b"DIRC" {
        return None;
    }
    let version = read_be_u32(bytes.get(4..8)?)?;
    if !matches!(version, 2 | 3) {
        return None;
    }
    let entry_count = usize::try_from(read_be_u32(bytes.get(8..12)?)?).ok()?;
    let mut offset = 12_usize;
    let mut entries = Vec::with_capacity(entry_count);
    for _ in 0..entry_count {
        let entry_start = offset;
        let fixed = bytes.get(offset..offset.checked_add(62)?)?;
        let mode = read_be_u32(fixed.get(24..28)?)?;
        let object_id: [u8; 20] = fixed.get(40..60)?.try_into().ok()?;
        let raw_flags = read_be_u16(fixed.get(60..62)?)?;
        offset = offset.checked_add(62)?;
        let extended_flags = if raw_flags & 0x4000 != 0 {
            if version < 3 {
                return None;
            }
            let extended = read_be_u16(bytes.get(offset..offset.checked_add(2)?)?)?;
            offset = offset.checked_add(2)?;
            Some(extended)
        } else {
            None
        };
        let path_end =
            offset.checked_add(bytes.get(offset..)?.iter().position(|byte| *byte == 0)?)?;
        let path = bytes.get(offset..path_end)?.to_vec();
        let entry_len = path_end.checked_add(1)?.checked_sub(entry_start)?;
        let padded_len = entry_len.checked_add((8 - (entry_len % 8)) % 8)?;
        offset = entry_start.checked_add(padded_len)?;
        if offset > bytes.len() {
            return None;
        }
        entries.push(GitIndexEntrySemantic {
            path,
            mode,
            object_id,
            flags: raw_flags & 0xf000,
            extended_flags,
        });
    }
    Some(GitIndexSemantic { entries })
}

fn read_be_u32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn read_be_u16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
}
