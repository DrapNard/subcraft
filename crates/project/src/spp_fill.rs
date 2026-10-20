//! Narrow semantic access to observed uniform Base Color fill sources.
//!
//! This module operates on decoded neutral document records. It does not parse a container or
//! import a complete Painter fill. Only the `DataSourceUniform` record shape and Base Color
//! channel token observed in the verified uniform-fill fixture are exposed here.

use std::collections::HashSet;

use crate::spp_document::{
    MAX_OBJECT_DEPTH, MAX_OBJECTS, SppDocumentError, SppDocumentRecords, SppObject, SppRecordValue,
};

const UNIFORM_SOURCE_CLASS: &str = "DataSourceUniform";
const BASE_COLOR_CHANNEL: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

#[derive(Clone, Debug, PartialEq)]
pub struct SppUniformBaseColorSourceInfo {
    /// Stable source UID. This is not a layer UID or a tree-order index.
    pub uid: u64,
    /// Linear RGB values decoded from the observed 12-byte color payload.
    pub color: [f32; 3],
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SppFillError {
    #[error(transparent)]
    Document(#[from] SppDocumentError),
    #[error("document has no root object")]
    MissingRoot,
    #[error("UID {0} occurs more than once")]
    DuplicateUid(u64),
    #[error("uniform source UID {0} was not found")]
    MissingUid(u64),
    #[error("UID {0} is not a uniform Base Color source")]
    NotBaseColorUniformSource(u64),
    #[error("unsupported or malformed observed uniform source schema: {0}")]
    UnsupportedSchema(&'static str),
    #[error("uniform source color must be finite and between 0 and 1")]
    InvalidColor,
    #[error("semantic traversal exceeds the {MAX_OBJECT_DEPTH}-level depth limit")]
    DepthLimit,
    #[error("semantic traversal exceeds the {MAX_OBJECTS}-object limit")]
    ObjectLimit,
}

type Result<T> = std::result::Result<T, SppFillError>;

/// Enumerate only uniform sources whose channel token exactly matches observed Base Color.
///
/// Other well-formed `DataSourceUniform` channel tokens are left unexposed. Unknown or malformed
/// uniform record schemas fail closed. Duplicate UIDs among all U64-UID objects also fail closed,
/// so returned source identifiers cannot silently become ambiguous elsewhere in the document.
pub fn enumerate_uniform_base_color_sources(
    records: &SppDocumentRecords,
) -> Result<Vec<SppUniformBaseColorSourceInfo>> {
    validate_records(records)?;
    let root = records.root.as_ref().ok_or(SppFillError::MissingRoot)?;
    let mut output = Vec::new();
    let mut visited = 0;
    walk(
        root,
        &records.classes,
        0,
        &mut visited,
        &mut Vec::new(),
        &mut |object, _| {
            if class_name(object, records)? != UNIFORM_SOURCE_CLASS {
                return Ok(());
            }
            let (uid, channel, color, _) = validate_uniform_source(object, records)?;
            if channel == BASE_COLOR_CHANNEL {
                output.push(SppUniformBaseColorSourceInfo {
                    uid,
                    color: decode_color(color)?,
                });
            }
            Ok(())
        },
    )?;
    Ok(output)
}

/// Change only the observed 12 color payload bytes for a stable uniform source UID.
///
/// The complete record tree and source schema are validated before mutation. On any error the
/// caller's `records` value remains byte-semantically unchanged.
pub fn set_uniform_base_color_source(
    records: &mut SppDocumentRecords,
    source_uid: u64,
    color: [f32; 3],
) -> Result<()> {
    validate_color(color)?;
    validate_records(records)?;

    let root = records.root.as_ref().ok_or(SppFillError::MissingRoot)?;
    let mut target_path = None;
    let mut visited = 0;
    walk(
        root,
        &records.classes,
        0,
        &mut visited,
        &mut Vec::new(),
        &mut |object, path| {
            if class_name(object, records)? != UNIFORM_SOURCE_CLASS {
                return Ok(());
            }
            let (uid, channel, _, _) = validate_uniform_source(object, records)?;
            if uid == source_uid {
                if channel != BASE_COLOR_CHANNEL {
                    return Err(SppFillError::NotBaseColorUniformSource(source_uid));
                }
                if target_path.is_some() {
                    return Err(SppFillError::DuplicateUid(source_uid));
                }
                target_path = Some(path.to_vec());
            }
            Ok(())
        },
    )?;

    let Some(path) = target_path else {
        return if contains_uid(records, source_uid)? {
            Err(SppFillError::NotBaseColorUniformSource(source_uid))
        } else {
            Err(SppFillError::MissingUid(source_uid))
        };
    };
    let object = object_at_path_mut(&mut records.root, &path)?;
    let color_index = property_index(object, &records.classes, "color")?;
    let mut payload = [0u8; 12];
    for (index, value) in color.into_iter().enumerate() {
        payload[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    match object.fields.get_mut(color_index) {
        Some(SppRecordValue::Opaque12(value)) => {
            *value = payload;
            Ok(())
        }
        _ => Err(SppFillError::UnsupportedSchema(
            "DataSourceUniform.color must use the observed 12-byte value",
        )),
    }
}

fn validate_records(records: &SppDocumentRecords) -> Result<()> {
    // The neutral record model is public and mutable; encode validates class/field consistency,
    // recursion, record counts, and the accepted primitive representation before traversal.
    records.encode()?;
    let root = records.root.as_ref().ok_or(SppFillError::MissingRoot)?;
    let mut uids = HashSet::new();
    let mut visited = 0;
    walk(
        root,
        &records.classes,
        0,
        &mut visited,
        &mut Vec::new(),
        &mut |object, _| {
            if let Some(uid) = optional_u64_uid(object, records)? {
                if !uids.insert(uid) {
                    return Err(SppFillError::DuplicateUid(uid));
                }
            }
            if class_name(object, records)? == UNIFORM_SOURCE_CLASS {
                validate_uniform_source(object, records)?;
            }
            Ok(())
        },
    )
}

fn validate_uniform_source(
    object: &SppObject,
    records: &SppDocumentRecords,
) -> Result<(u64, [u8; 16], [u8; 12], f32)> {
    let class = records
        .classes
        .get(object.class_index)
        .ok_or(SppFillError::UnsupportedSchema(
            "source class index is invalid",
        ))?;
    if class.properties.len() != 4 || object.fields.len() != 4 {
        return Err(SppFillError::UnsupportedSchema(
            "DataSourceUniform must have the observed four fields",
        ));
    }
    let uid_index = property_index(object, &records.classes, "uid")?;
    let channel_index = property_index(object, &records.classes, "channelTypes")?;
    let color_index = property_index(object, &records.classes, "color")?;
    let opacity_index = property_index(object, &records.classes, "opacity")?;
    let types = [
        (uid_index, &[12, 15, 21][..]),
        (channel_index, &[22][..]),
        (color_index, &[3][..]),
        (opacity_index, &[1][..]),
    ];
    for (index, accepted) in types {
        let property = class
            .properties
            .get(index)
            .ok_or(SppFillError::UnsupportedSchema("source property index"))?;
        if !accepted.contains(&property.type_code) {
            return Err(SppFillError::UnsupportedSchema(
                "DataSourceUniform property type is not observed",
            ));
        }
    }
    let uid = match object.fields.get(uid_index) {
        Some(SppRecordValue::U64(uid)) => *uid,
        _ => return Err(SppFillError::UnsupportedSchema("source UID must be U64")),
    };
    let channel = match object.fields.get(channel_index) {
        Some(SppRecordValue::Opaque16(channel)) => *channel,
        _ => {
            return Err(SppFillError::UnsupportedSchema(
                "DataSourceUniform.channelTypes must be 16 bytes",
            ));
        }
    };
    let color = match object.fields.get(color_index) {
        Some(SppRecordValue::Opaque12(color)) => *color,
        _ => {
            return Err(SppFillError::UnsupportedSchema(
                "DataSourceUniform.color must be 12 bytes",
            ));
        }
    };
    let opacity = match object.fields.get(opacity_index) {
        Some(SppRecordValue::F32Bits(bits)) => f32::from_bits(*bits),
        _ => {
            return Err(SppFillError::UnsupportedSchema(
                "source opacity must be F32",
            ));
        }
    };
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(SppFillError::UnsupportedSchema(
            "source opacity must be finite and between 0 and 1",
        ));
    }
    if channel == BASE_COLOR_CHANNEL {
        decode_color(color)?;
    }
    Ok((uid, channel, color, opacity))
}

fn decode_color(payload: [u8; 12]) -> Result<[f32; 3]> {
    let color = std::array::from_fn(|index| {
        let offset = index * 4;
        f32::from_le_bytes(
            payload[offset..offset + 4]
                .try_into()
                .expect("fixed color slice"),
        )
    });
    validate_color(color)?;
    Ok(color)
}

fn validate_color(color: [f32; 3]) -> Result<()> {
    if color
        .iter()
        .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(SppFillError::InvalidColor);
    }
    Ok(())
}

fn optional_u64_uid(object: &SppObject, records: &SppDocumentRecords) -> Result<Option<u64>> {
    let class = records
        .classes
        .get(object.class_index)
        .ok_or(SppFillError::UnsupportedSchema(
            "object class index is invalid",
        ))?;
    let mut indices = class
        .properties
        .iter()
        .enumerate()
        .filter(|(_, property)| property.name == "uid");
    let Some((index, property)) = indices.next() else {
        return Ok(None);
    };
    if indices.next().is_some() {
        return Err(SppFillError::UnsupportedSchema("duplicate uid property"));
    }
    if ![12, 15, 21].contains(&property.type_code) {
        return Ok(None);
    }
    match object.fields.get(index) {
        Some(SppRecordValue::U64(uid)) => Ok(Some(*uid)),
        _ => Err(SppFillError::UnsupportedSchema("uid field is not U64")),
    }
}

fn contains_uid(records: &SppDocumentRecords, wanted: u64) -> Result<bool> {
    let root = records.root.as_ref().ok_or(SppFillError::MissingRoot)?;
    let mut found = false;
    let mut visited = 0;
    walk(
        root,
        &records.classes,
        0,
        &mut visited,
        &mut Vec::new(),
        &mut |object, _| {
            if optional_u64_uid(object, records)? == Some(wanted) {
                found = true;
            }
            Ok(())
        },
    )?;
    Ok(found)
}

fn property_index(
    object: &SppObject,
    classes: &[crate::spp_document::SppClassDefinition],
    name: &str,
) -> Result<usize> {
    let class = classes
        .get(object.class_index)
        .ok_or(SppFillError::UnsupportedSchema(
            "object class index is invalid",
        ))?;
    let mut indices = class
        .properties
        .iter()
        .enumerate()
        .filter(|(_, property)| property.name == name);
    let index = indices
        .next()
        .map(|(index, _)| index)
        .ok_or(SppFillError::UnsupportedSchema(
            "required source property missing",
        ))?;
    if indices.next().is_some() || object.fields.len() != class.properties.len() {
        return Err(SppFillError::UnsupportedSchema(
            "source fields do not match class definition",
        ));
    }
    Ok(index)
}

fn class_name<'a>(object: &SppObject, records: &'a SppDocumentRecords) -> Result<&'a str> {
    records
        .classes
        .get(object.class_index)
        .map(|class| class.name.as_str())
        .ok_or(SppFillError::UnsupportedSchema(
            "object class index is invalid",
        ))
}

#[derive(Clone, Copy)]
enum ChildIndex {
    Single,
    Array(usize),
}

#[derive(Clone, Copy)]
struct PathStep {
    field_index: usize,
    child_index: ChildIndex,
}

fn walk(
    object: &SppObject,
    classes: &[crate::spp_document::SppClassDefinition],
    depth: usize,
    visited: &mut usize,
    path: &mut Vec<PathStep>,
    visitor: &mut impl FnMut(&SppObject, &[PathStep]) -> Result<()>,
) -> Result<()> {
    if depth > MAX_OBJECT_DEPTH {
        return Err(SppFillError::DepthLimit);
    }
    *visited += 1;
    if *visited > MAX_OBJECTS {
        return Err(SppFillError::ObjectLimit);
    }
    visitor(object, path)?;
    for (field_index, value) in object.fields.iter().enumerate() {
        match value {
            SppRecordValue::Object(Some(child)) => {
                path.push(PathStep {
                    field_index,
                    child_index: ChildIndex::Single,
                });
                walk(child, classes, depth + 1, visited, path, visitor)?;
                path.pop();
            }
            SppRecordValue::InlineObjects(children) => {
                for (index, child) in children.iter().enumerate() {
                    path.push(PathStep {
                        field_index,
                        child_index: ChildIndex::Array(index),
                    });
                    walk(child, classes, depth + 1, visited, path, visitor)?;
                    path.pop();
                }
            }
            SppRecordValue::PointerObjects(children) => {
                for (index, child) in children.iter().enumerate() {
                    if let Some(child) = child {
                        path.push(PathStep {
                            field_index,
                            child_index: ChildIndex::Array(index),
                        });
                        walk(child, classes, depth + 1, visited, path, visitor)?;
                        path.pop();
                    }
                }
            }
            _ => {}
        }
    }
    let _ = classes;
    Ok(())
}

fn object_at_path_mut<'a>(
    root: &'a mut Option<SppObject>,
    path: &[PathStep],
) -> Result<&'a mut SppObject> {
    let mut object = root.as_mut().ok_or(SppFillError::MissingRoot)?;
    for step in path {
        let value =
            object
                .fields
                .get_mut(step.field_index)
                .ok_or(SppFillError::UnsupportedSchema(
                    "source path field is invalid",
                ))?;
        object = match (value, step.child_index) {
            (SppRecordValue::Object(Some(child)), ChildIndex::Single) => child,
            (SppRecordValue::InlineObjects(children), ChildIndex::Array(index)) => children
                .get_mut(index)
                .ok_or(SppFillError::UnsupportedSchema(
                    "source path index is invalid",
                ))?,
            (SppRecordValue::PointerObjects(children), ChildIndex::Array(index)) => children
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or(SppFillError::UnsupportedSchema("source path is null"))?,
            _ => return Err(SppFillError::UnsupportedSchema("source path is invalid")),
        };
    }
    Ok(object)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spp_document::{SppClassDefinition, SppDocumentHeader, SppPropertyDefinition};

    fn prop(name: &str, type_code: u32) -> SppPropertyDefinition {
        SppPropertyDefinition {
            name: name.to_owned(),
            type_code,
        }
    }

    fn color_payload(color: [f32; 3]) -> [u8; 12] {
        let mut payload = [0; 12];
        for (index, value) in color.into_iter().enumerate() {
            payload[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        payload
    }

    fn source(uid: u64, channel: [u8; 16], color: [f32; 3]) -> SppObject {
        SppObject {
            class_index: 1,
            fields: vec![
                SppRecordValue::Opaque16(channel),
                SppRecordValue::Opaque12(color_payload(color)),
                SppRecordValue::F32Bits(1.0_f32.to_bits()),
                SppRecordValue::U64(uid),
            ],
        }
    }

    fn fixture() -> SppDocumentRecords {
        SppDocumentRecords {
            header: SppDocumentHeader {
                magic: [0xdd, 0x2f, 0x7c, 0x1b],
                version: 1,
                preserved_word_8: 0x1234,
                preserved_word_12: 0x5678,
            },
            classes: vec![
                SppClassDefinition {
                    name: "DataDocument".to_owned(),
                    properties: vec![prop("sources", 17), prop("other", 17)],
                },
                SppClassDefinition {
                    name: UNIFORM_SOURCE_CLASS.to_owned(),
                    properties: vec![
                        prop("channelTypes", 22),
                        prop("color", 3),
                        prop("opacity", 1),
                        prop("uid", 12),
                    ],
                },
                SppClassDefinition {
                    name: "DataOther".to_owned(),
                    properties: vec![prop("uid", 12), prop("preserved", 14)],
                },
            ],
            root: Some(SppObject {
                class_index: 0,
                fields: vec![
                    SppRecordValue::InlineObjects(vec![
                        source(93, BASE_COLOR_CHANNEL, [0.0143499, 0.116016, 0.34851]),
                        source(
                            94,
                            [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                            [0.2, 0.3, 0.4],
                        ),
                    ]),
                    SppRecordValue::InlineObjects(vec![SppObject {
                        class_index: 2,
                        fields: vec![
                            SppRecordValue::U64(900),
                            SppRecordValue::Opaque64([0xa5; 64]),
                        ],
                    }]),
                ],
            }),
        }
    }

    #[test]
    fn enumerates_only_the_observed_base_color_uniform_source() {
        let records = fixture();
        let sources = enumerate_uniform_base_color_sources(&records).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].uid, 93);
        assert_eq!(sources[0].color, [0.0143499, 0.116016, 0.34851]);
    }

    #[test]
    fn setter_changes_only_color_payload_and_roundtrips_unknown_data() {
        let mut records = fixture();
        let before = records.clone();
        set_uniform_base_color_source(&mut records, 93, [0.125, 0.375, 0.625]).unwrap();

        let SppRecordValue::InlineObjects(before_sources) =
            &before.root.as_ref().unwrap().fields[0]
        else {
            panic!("fixture source list");
        };
        let SppRecordValue::InlineObjects(after_sources) =
            &records.root.as_ref().unwrap().fields[0]
        else {
            panic!("fixture source list");
        };
        let mut expected = before_sources.clone();
        let SppRecordValue::Opaque12(expected_color) = &mut expected[0].fields[1] else {
            panic!("fixture color");
        };
        *expected_color = color_payload([0.125, 0.375, 0.625]);
        assert_eq!(after_sources, &expected);
        assert_eq!(
            records.root.as_ref().unwrap().fields[1],
            before.root.as_ref().unwrap().fields[1]
        );

        let encoded = records.encode().unwrap();
        let decoded = SppDocumentRecords::decode(&encoded).unwrap();
        assert_eq!(decoded, records);
        assert_eq!(
            enumerate_uniform_base_color_sources(&decoded).unwrap()[0].color,
            [0.125, 0.375, 0.625]
        );
    }

    #[test]
    fn duplicate_uid_anywhere_is_rejected_before_mutation() {
        let mut records = fixture();
        let SppRecordValue::InlineObjects(other) = &mut records.root.as_mut().unwrap().fields[1]
        else {
            panic!("fixture other list");
        };
        other[0].fields[0] = SppRecordValue::U64(93);
        let before = records.clone();

        assert_eq!(
            set_uniform_base_color_source(&mut records, 93, [0.1, 0.2, 0.3]),
            Err(SppFillError::DuplicateUid(93))
        );
        assert_eq!(records, before);
    }

    #[test]
    fn non_base_color_source_is_rejected_without_mutation() {
        let mut records = fixture();
        let before = records.clone();
        assert_eq!(
            set_uniform_base_color_source(&mut records, 94, [0.1, 0.2, 0.3]),
            Err(SppFillError::NotBaseColorUniformSource(94))
        );
        assert_eq!(records, before);
    }

    #[test]
    fn malformed_uniform_type_and_invalid_color_fail_without_mutation() {
        let mut records = fixture();
        records.classes[1].properties[1].type_code = 2;
        let before = records.clone();
        assert!(set_uniform_base_color_source(&mut records, 93, [0.1, 0.2, 0.3]).is_err());
        assert_eq!(records, before);

        let mut records = fixture();
        let before = records.clone();
        assert_eq!(
            set_uniform_base_color_source(&mut records, 93, [0.1, f32::NAN, 0.3]),
            Err(SppFillError::InvalidColor)
        );
        assert_eq!(records, before);
    }
}
