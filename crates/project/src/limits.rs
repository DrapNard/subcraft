//! Allocation limits and ZIP preflight for untrusted `.subcraft` project archives.
//!
//! The central-directory preflight is adapted from Kalium's bounded project loader
//! (AGPL-3.0-only); see the repository's third-party notices for provenance. It
//! checks attacker-controlled allocation fields before `ZipArchive::new` builds
//! its entry table. The limits below are SubCraft project policy.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};

use zip::ZipArchive;

use crate::ProjectError;

pub const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_ZIP_ENTRIES: usize = 65_536;
pub const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_ENTRY_NAME_BYTES: u16 = 256;
pub const MAX_ENTRY_AUX_BYTES: u16 = 4096;
pub const MAX_ENTRY_UNCOMPRESSED_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub const MAX_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_MESH_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_RASTER_BYTES: u64 = 160 * 1024 * 1024;
pub const MAX_RESOURCE_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_RESOURCE_DECODED_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_TOTAL_RASTER_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_TOTAL_RESOURCE_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_TOTAL_RESOURCE_DECODED_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_TEXTURE_SETS: usize = 64;
pub const MAX_LAYER_DEPTH: usize = 64;
pub const MAX_LAYER_NODES: usize = 100_000;
pub const MAX_JSON_DEPTH: usize = 128;
pub const MAX_JSON_STRUCTURES: usize = 1_000_000;
pub const MAX_RASTER_DIMENSION: u32 = 16_384;

const EOCD_SIG: u32 = 0x0605_4b50;
const ZIP64_LOCATOR_SIG: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIG: u32 = 0x0606_4b50;
const CENTRAL_HEADER_SIG: u32 = 0x0201_4b50;
const EOCD_MIN_BYTES: usize = 22;
const ZIP64_LOCATOR_BYTES: usize = 20;
const ZIP64_EOCD_MIN_BYTES: usize = 56;
const CENTRAL_HEADER_BYTES: u64 = 46;
const MAX_ARCHIVE_PREFIX_BYTES: u64 = 1024 * 1024;
const MAX_ZIP64_EOCD_AUX_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipEntryMeta {
    pub compressed_size: u64,
    pub uncompressed_size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipDirectory {
    pub entry_count: usize,
    pub entries: BTreeMap<String, ZipEntryMeta>,
}

#[derive(Default, Debug)]
pub struct ReadBudget {
    total_uncompressed: u64,
}

impl ReadBudget {
    pub fn total_uncompressed(&self) -> u64 {
        self.total_uncompressed
    }
}

fn invalid(message: impl Into<String>) -> ProjectError {
    ProjectError::Invalid(message.into())
}

fn le_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("validated ZIP field"),
    )
}

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated ZIP field"),
    )
}

fn le_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("validated ZIP field"),
    )
}

/// Read ZIP directory size/count/name/size fields before `ZipArchive::new` allocates tables.
pub fn preflight_zip_directory<R: Read + Seek>(
    reader: &mut R,
) -> Result<ZipDirectory, ProjectError> {
    let file_len = reader.seek(SeekFrom::End(0))?;
    if file_len > MAX_ARCHIVE_BYTES {
        return Err(invalid("project archive exceeds the 1-GiB limit"));
    }
    let tail_len = file_len.min((EOCD_MIN_BYTES + u16::MAX as usize) as u64) as usize;
    if tail_len < EOCD_MIN_BYTES {
        return Err(invalid(
            "project ZIP is too short to contain an end-of-directory record",
        ));
    }
    let tail_start = file_len - tail_len as u64;
    reader.seek(SeekFrom::Start(tail_start))?;
    let mut tail = vec![0; tail_len];
    reader.read_exact(&mut tail)?;

    let eocd_index = (0..=tail.len().saturating_sub(EOCD_MIN_BYTES))
        .rev()
        .find(|&index| {
            le_u32(&tail, index) == EOCD_SIG
                && index + EOCD_MIN_BYTES + usize::from(le_u16(&tail, index + 20)) == tail.len()
        })
        .ok_or_else(|| invalid("project ZIP has no valid end-of-directory record"))?;
    let eocd_offset = tail_start + eocd_index as u64;
    let eocd = &tail[eocd_index..];
    let disk_number = le_u16(eocd, 4);
    let central_disk = le_u16(eocd, 6);
    let entries_on_disk = le_u16(eocd, 8);
    let total_entries = le_u16(eocd, 10);
    let classic_size = le_u32(eocd, 12);
    let classic_offset = le_u32(eocd, 16);
    let has_zip64_locator = if eocd_offset >= ZIP64_LOCATOR_BYTES as u64 {
        reader.seek(SeekFrom::Start(eocd_offset - ZIP64_LOCATOR_BYTES as u64))?;
        let mut locator = [0; ZIP64_LOCATOR_BYTES];
        reader.read_exact(&mut locator)?;
        le_u32(&locator, 0) == ZIP64_LOCATOR_SIG
    } else {
        false
    };

    let (entry_count_u64, central_size, central_offset, central_end) = if has_zip64_locator {
        reader.seek(SeekFrom::Start(eocd_offset - ZIP64_LOCATOR_BYTES as u64))?;
        let mut locator = [0; ZIP64_LOCATOR_BYTES];
        reader.read_exact(&mut locator)?;
        let locator_disk = le_u32(&locator, 4);
        let zip64_offset = le_u64(&locator, 8);
        let disk_count = le_u32(&locator, 16);
        if locator_disk != 0 || disk_count != 1 {
            return Err(invalid("multi-disk project ZIP archives are not supported"));
        }
        let locator_offset = eocd_offset - ZIP64_LOCATOR_BYTES as u64;
        if zip64_offset > locator_offset {
            return Err(invalid("invalid ZIP64 project directory offset"));
        }
        let footer_start = locator_offset.saturating_sub(12 + 44 + MAX_ZIP64_EOCD_AUX_BYTES);
        let footer_len = usize::try_from(locator_offset - footer_start)
            .map_err(|_| invalid("ZIP64 project directory footer is too large"))?;
        reader.seek(SeekFrom::Start(footer_start))?;
        let mut footer = vec![0; footer_len];
        reader.read_exact(&mut footer)?;
        let last_start = footer
            .len()
            .checked_sub(ZIP64_EOCD_MIN_BYTES)
            .ok_or_else(|| invalid("invalid ZIP64 project directory footer"))?;
        let index = (0..=last_start)
            .find(|&index| {
                if le_u32(&footer, index) != ZIP64_EOCD_SIG {
                    return false;
                }
                let size = le_u64(&footer, index + 4);
                size >= 44
                    && size <= 44 + MAX_ZIP64_EOCD_AUX_BYTES
                    && index as u64 + 12 + size == footer.len() as u64
            })
            .ok_or_else(|| invalid("invalid ZIP64 project directory footer"))?;
        let zip64_actual = footer_start + index as u64;
        let prefix = zip64_actual
            .checked_sub(zip64_offset)
            .ok_or_else(|| invalid("invalid ZIP64 project directory offset"))?;
        if prefix > MAX_ARCHIVE_PREFIX_BYTES {
            return Err(invalid(
                "ZIP64 project archive prefix exceeds its safety limit",
            ));
        }
        let zip64 = &footer[index..index + ZIP64_EOCD_MIN_BYTES];
        let size = le_u64(zip64, 4);
        if size < 44
            || size > 44 + MAX_ZIP64_EOCD_AUX_BYTES
            || zip64_actual
                .checked_add(12)
                .and_then(|v| v.checked_add(size))
                != Some(locator_offset)
        {
            return Err(invalid("invalid ZIP64 project directory size"));
        }
        let this_disk = le_u32(zip64, 16);
        let directory_disk = le_u32(zip64, 20);
        let disk_entries = le_u64(zip64, 24);
        let total_entries = le_u64(zip64, 32);
        if this_disk != 0 || directory_disk != 0 || disk_entries != total_entries {
            return Err(invalid("multi-disk project ZIP archives are not supported"));
        }
        (
            total_entries,
            le_u64(zip64, 40),
            le_u64(zip64, 48),
            zip64_actual,
        )
    } else {
        if [
            entries_on_disk == u16::MAX,
            total_entries == u16::MAX,
            classic_size == u32::MAX,
            classic_offset == u32::MAX,
        ]
        .into_iter()
        .any(|is_zip64| is_zip64)
        {
            return Err(invalid("ZIP64 project archive is missing its locator"));
        }
        if disk_number != 0 || central_disk != 0 || entries_on_disk != total_entries {
            return Err(invalid("multi-disk project ZIP archives are not supported"));
        }
        (
            u64::from(total_entries),
            u64::from(classic_size),
            u64::from(classic_offset),
            eocd_offset,
        )
    };

    if entry_count_u64 > MAX_ZIP_ENTRIES as u64 {
        return Err(invalid("project contains too many ZIP entries"));
    }
    if central_size > MAX_CENTRAL_DIRECTORY_BYTES {
        return Err(invalid(
            "project ZIP central directory exceeds its safety limit",
        ));
    }
    let prefix = central_end
        .checked_sub(central_size)
        .and_then(|end| end.checked_sub(central_offset))
        .ok_or_else(|| invalid("invalid project ZIP central directory offset"))?;
    if prefix > MAX_ARCHIVE_PREFIX_BYTES {
        return Err(invalid(
            "project ZIP archive prefix exceeds its safety limit",
        ));
    }
    let central_start = central_offset
        .checked_add(prefix)
        .ok_or_else(|| invalid("invalid project ZIP central directory offset"))?;
    let central_limit = central_start
        .checked_add(central_size)
        .ok_or_else(|| invalid("project ZIP central directory size overflow"))?;
    if central_limit > central_end || central_limit > file_len {
        return Err(invalid(
            "project ZIP central directory exceeds archive bounds",
        ));
    }

    reader.seek(SeekFrom::Start(central_start))?;
    let mut consumed = 0u64;
    let mut compressed_total = 0u64;
    let mut uncompressed_total = 0u64;
    let mut entries = BTreeMap::new();
    let entry_count =
        usize::try_from(entry_count_u64).map_err(|_| invalid("too many ZIP entries"))?;
    for _ in 0..entry_count {
        if consumed
            .checked_add(CENTRAL_HEADER_BYTES)
            .is_none_or(|next| next > central_size)
        {
            return Err(invalid(
                "project ZIP central directory entry exceeds declared bounds",
            ));
        }
        let mut header = [0; CENTRAL_HEADER_BYTES as usize];
        reader.read_exact(&mut header)?;
        if le_u32(&header, 0) != CENTRAL_HEADER_SIG {
            return Err(invalid("invalid project ZIP central directory entry"));
        }
        let name_len = le_u16(&header, 28);
        let extra_len = le_u16(&header, 30);
        let comment_len = le_u16(&header, 32);
        if name_len == 0 || name_len > MAX_ENTRY_NAME_BYTES {
            return Err(invalid("project ZIP entry name exceeds its safety limit"));
        }
        if extra_len > MAX_ENTRY_AUX_BYTES || comment_len > MAX_ENTRY_AUX_BYTES {
            return Err(invalid(
                "project ZIP entry metadata exceeds its safety limit",
            ));
        }
        let variable_len =
            usize::from(name_len) + usize::from(extra_len) + usize::from(comment_len);
        let mut variable = vec![0; variable_len];
        reader.read_exact(&mut variable)?;
        let name = std::str::from_utf8(&variable[..usize::from(name_len)])
            .map_err(|_| invalid("project ZIP entry name is not valid UTF-8"))?
            .to_owned();
        validate_entry_name(&name)?;
        let extra_start = usize::from(name_len);
        let extra_end = extra_start + usize::from(extra_len);
        let extra = &variable[extra_start..extra_end];
        let mut uncompressed = u64::from(le_u32(&header, 24));
        let mut compressed = u64::from(le_u32(&header, 20));
        if uncompressed == u32::MAX as u64 || compressed == u32::MAX as u64 {
            parse_zip64_sizes(extra, &mut uncompressed, &mut compressed)?;
        }
        if compressed > MAX_ARCHIVE_BYTES || uncompressed > MAX_ENTRY_UNCOMPRESSED_BYTES {
            return Err(invalid(format!(
                "project ZIP entry {name} exceeds its size limit"
            )));
        }
        compressed_total = compressed_total
            .checked_add(compressed)
            .ok_or_else(|| invalid("project ZIP compressed size overflow"))?;
        uncompressed_total = uncompressed_total
            .checked_add(uncompressed)
            .ok_or_else(|| invalid("project ZIP uncompressed size overflow"))?;
        if compressed_total > MAX_ARCHIVE_BYTES || uncompressed_total > MAX_TOTAL_UNCOMPRESSED_BYTES
        {
            return Err(invalid(
                "project ZIP aggregate entry sizes exceed their safety limit",
            ));
        }
        if entries
            .insert(
                name.clone(),
                ZipEntryMeta {
                    compressed_size: compressed,
                    uncompressed_size: uncompressed,
                },
            )
            .is_some()
        {
            return Err(invalid(format!(
                "project ZIP contains duplicate entry {name}"
            )));
        }
        consumed = consumed
            .checked_add(CENTRAL_HEADER_BYTES + variable_len as u64)
            .ok_or_else(|| invalid("project ZIP central directory size overflow"))?;
        if consumed > central_size {
            return Err(invalid(
                "project ZIP central directory entry exceeds declared bounds",
            ));
        }
    }
    if consumed != central_size {
        return Err(invalid(
            "project ZIP central directory has unexpected trailing data",
        ));
    }
    Ok(ZipDirectory {
        entry_count,
        entries,
    })
}

fn parse_zip64_sizes(
    extra: &[u8],
    uncompressed: &mut u64,
    compressed: &mut u64,
) -> Result<(), ProjectError> {
    let mut cursor = 0usize;
    while cursor + 4 <= extra.len() {
        let tag = le_u16(extra, cursor);
        let len = usize::from(le_u16(extra, cursor + 2));
        cursor += 4;
        let end = cursor
            .checked_add(len)
            .ok_or_else(|| invalid("ZIP extra field size overflow"))?;
        if end > extra.len() {
            return Err(invalid("truncated ZIP extra field"));
        }
        if tag == 0x0001 {
            let mut p = cursor;
            if *uncompressed == u32::MAX as u64 {
                if p + 8 > end {
                    return Err(invalid("truncated ZIP64 uncompressed size"));
                }
                *uncompressed = le_u64(extra, p);
                p += 8;
            }
            if *compressed == u32::MAX as u64 {
                if p + 8 > end {
                    return Err(invalid("truncated ZIP64 compressed size"));
                }
                *compressed = le_u64(extra, p);
            }
            return Ok(());
        }
        cursor = end;
    }
    Err(invalid("ZIP64 entry is missing its size fields"))
}

pub fn validate_entry_name(name: &str) -> Result<(), ProjectError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.contains('\\')
        || name.contains('\0')
        || name.contains(':')
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(invalid(format!("unsafe project ZIP entry path: {name:?}")));
    }
    Ok(())
}

pub fn read_entry_bounded<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    directory: &ZipDirectory,
    name: &str,
    entry_cap: u64,
    budget: &mut ReadBudget,
) -> Result<Vec<u8>, ProjectError> {
    validate_entry_name(name)?;
    let meta = directory
        .entries
        .get(name)
        .ok_or_else(|| invalid(format!("missing project entry {name}")))?;
    if meta.uncompressed_size > entry_cap || meta.uncompressed_size > MAX_ENTRY_UNCOMPRESSED_BYTES {
        return Err(invalid(format!(
            "project entry {name} exceeds its read limit"
        )));
    }
    let next_total = budget
        .total_uncompressed
        .checked_add(meta.uncompressed_size)
        .ok_or_else(|| invalid("project decompression budget overflow"))?;
    if next_total > MAX_TOTAL_UNCOMPRESSED_BYTES {
        return Err(invalid(
            "project exceeds the aggregate decompressed byte limit",
        ));
    }
    let file = zip
        .by_name(name)
        .map_err(|_| invalid(format!("missing project entry {name}")))?;
    if file.size() != meta.uncompressed_size || file.compressed_size() != meta.compressed_size {
        return Err(invalid(format!(
            "project entry {name} changed after ZIP preflight"
        )));
    }
    let capacity = usize::try_from(meta.uncompressed_size)
        .map_err(|_| invalid("project entry size does not fit memory"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| invalid(format!("could not reserve project entry {name}")))?;
    let read_limit = entry_cap
        .min(MAX_ENTRY_UNCOMPRESSED_BYTES)
        .saturating_add(1);
    file.take(read_limit).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != meta.uncompressed_size {
        return Err(invalid(format!(
            "project entry {name} did not match its declared size"
        )));
    }
    budget.total_uncompressed = next_total;
    Ok(bytes)
}

/// Limit JSON nesting and object/array count before serde allocates its value tree.
pub fn preflight_json(bytes: &[u8]) -> Result<(), ProjectError> {
    let (mut depth, mut structures) = (0usize, 0usize);
    let (mut in_string, mut escaped) = (false, false);
    for &byte in bytes {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                structures += 1;
                if depth > MAX_JSON_DEPTH || structures > MAX_JSON_STRUCTURES {
                    return Err(invalid(
                        "project JSON exceeds its nesting or structure limit",
                    ));
                }
            }
            b'}' | b']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("invalid project JSON nesting"))?;
            }
            _ => {}
        }
    }
    if in_string || depth != 0 {
        return Err(invalid("truncated project JSON"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn preflight_accepts_writer_archives_and_reports_entries() {
        let bytes = archive(&[("manifest.json", b"{}"), ("mesh.bin", b"mesh")]);
        let mut cursor = Cursor::new(bytes);
        let directory = preflight_zip_directory(&mut cursor).unwrap();
        assert_eq!(directory.entry_count, 2);
        assert_eq!(directory.entries["manifest.json"].uncompressed_size, 2);
    }

    #[test]
    fn safe_zip_paths_reject_escape_and_platform_aliases() {
        for path in [
            "../outside",
            "a/../outside",
            "/absolute",
            r"a\b",
            "C:/drive",
            "a//b",
            "a/./b",
        ] {
            assert!(validate_entry_name(path).is_err(), "{path:?}");
        }
        assert!(validate_entry_name("rasters/00001.tiles").is_ok());
    }

    #[test]
    fn bounded_reader_matches_sizes_and_budget() {
        let bytes = archive(&[("x", b"payload")]);
        let mut cursor = Cursor::new(bytes.clone());
        let directory = preflight_zip_directory(&mut cursor).unwrap();
        cursor.set_position(0);
        let mut zip = ZipArchive::new(cursor).unwrap();
        let mut budget = ReadBudget::default();
        assert_eq!(
            read_entry_bounded(&mut zip, &directory, "x", 7, &mut budget).unwrap(),
            b"payload"
        );
        assert!(
            read_entry_bounded(&mut zip, &directory, "x", 6, &mut ReadBudget::default()).is_err()
        );
        assert_eq!(budget.total_uncompressed(), 7);
    }

    #[test]
    fn json_preflight_caps_nesting_and_structural_count() {
        assert!(preflight_json(br#"{"x":[1,2]}"#).is_ok());
        let too_deep = format!(
            "{}0{}",
            "[".repeat(MAX_JSON_DEPTH + 1),
            "]".repeat(MAX_JSON_DEPTH + 1)
        );
        assert!(preflight_json(too_deep.as_bytes()).is_err());
        assert!(preflight_json(b"[[").is_err());
    }
}
