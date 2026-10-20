//! Typed semantic helpers for the narrow `DataLayerColor` SPP subset observed so far.
//!
//! This module operates on decoded neutral records. It does not parse an HDF5 container or claim
//! to describe Painter's full document model. Field names/types and the Base Color channel flag
//! below are limited observations; unsupported shapes fail closed and unrelated fields remain
//! untouched.

use std::collections::HashSet;

use crate::spp_document::{
    MAX_OBJECT_DEPTH, MAX_OBJECTS, MAX_TEXT_BYTES, SppClassDefinition, SppDocumentRecords,
    SppObject, SppPropertyDefinition, SppRecordValue,
};

const LAYER_CLASS: &str = "DataLayerColor";
const BLENDING_CLASS: &str = "DataBlending";
const BASE_COLOR_CHANNEL: u64 = 1;
const BASE_COLOR_CHANNEL_22: [u8; 16] = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const DEFAULT_BLENDING_MODE: u32 = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct SppLayerInfo {
    pub uid: u64,
    pub label: String,
    pub enabled: bool,
    /// Defaults to 1.0 when the observed Base Color blending record is absent.
    pub base_color_opacity: f32,
}

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum SppSemanticError {
    #[error("document has no root object")]
    MissingRoot,
    #[error("layer UID {0} was not found")]
    MissingUid(u64),
    #[error("UID {0} occurs more than once")]
    DuplicateUid(u64),
    #[error("UID {0} belongs to an object that is not a DataLayerColor layer")]
    NotLayerId(u64),
    #[error("unsupported or malformed observed SPP semantic schema: {0}")]
    UnsupportedSchema(String),
    #[error("opacity must be finite and between 0 and 1")]
    InvalidOpacity,
    #[error("label exceeds the {MAX_TEXT_BYTES}-byte text limit")]
    LabelTooLong,
    #[error("multiple Base Color blending records were found for one layer")]
    DuplicateBaseColor,
    #[error("semantic traversal exceeds the {MAX_OBJECT_DEPTH}-level depth limit")]
    DepthLimit,
    #[error("semantic traversal exceeds the {MAX_OBJECTS}-object limit")]
    ObjectLimit,
}

/// Enumerate all observed color layers in tree order.
pub fn enumerate_layers(
    records: &SppDocumentRecords,
) -> Result<Vec<SppLayerInfo>, SppSemanticError> {
    let root = records.root.as_ref().ok_or(SppSemanticError::MissingRoot)?;
    let mut output = Vec::new();
    let mut uids = HashSet::new();
    let mut visited = 0;
    walk(
        root,
        &records.classes,
        0,
        &mut visited,
        &mut Vec::new(),
        &mut |object, _| {
            if class_name(object, &records.classes)? == LAYER_CLASS {
                let layer = read_layer(object, &records.classes)?;
                if !uids.insert(layer.uid) {
                    return Err(SppSemanticError::DuplicateUid(layer.uid));
                }
                output.push(layer);
            }
            Ok(())
        },
    )?;
    Ok(output)
}

/// Rename the layer identified by its stable UID, changing only its `label` value.
pub fn rename_layer(
    records: &mut SppDocumentRecords,
    uid: u64,
    label: &str,
) -> Result<(), SppSemanticError> {
    validate_label(label)?;
    let path = resolve_layer_path(records, uid)?;
    let object = object_at_path_mut(&mut records.root, &path)?;
    let index = required_property_index(object, &records.classes, "label")?;
    match object.fields.get_mut(index) {
        Some(SppRecordValue::Utf8(value)) => {
            *value = label.to_owned();
            Ok(())
        }
        _ => Err(schema_error("DataLayerColor.label must be UTF-8")),
    }
}

/// Set the observed `enabled` flag for the layer identified by UID.
pub fn set_layer_visibility(
    records: &mut SppDocumentRecords,
    uid: u64,
    enabled: bool,
) -> Result<(), SppSemanticError> {
    let path = resolve_layer_path(records, uid)?;
    let object = object_at_path_mut(&mut records.root, &path)?;
    let index = required_property_index(object, &records.classes, "enabled")?;
    match object.fields.get_mut(index) {
        Some(SppRecordValue::Bool(value)) => {
            *value = enabled;
            Ok(())
        }
        _ => Err(schema_error("DataLayerColor.enabled must be boolean")),
    }
}

/// Set Base Color opacity, preserving existing blend mode and all unrelated values.
///
/// If the channel record is absent, a `DataBlending` record is added only when the class table
/// contains the exact observed three-field definition. Unknown/additional fields are not guessed.
pub fn set_base_color_opacity(
    records: &mut SppDocumentRecords,
    uid: u64,
    opacity: f32,
) -> Result<(), SppSemanticError> {
    if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
        return Err(SppSemanticError::InvalidOpacity);
    }
    let path = resolve_layer_path(records, uid)?;
    let layer = object_at_path_mut(&mut records.root, &path)?;
    validate_layer_shape(layer, &records.classes)?;
    let blend_index = required_property_index(layer, &records.classes, "perChannelBlending")?;
    let blendings = match layer.fields.get_mut(blend_index) {
        Some(SppRecordValue::InlineObjects(values)) => values,
        _ => {
            return Err(schema_error(
                "DataLayerColor.perChannelBlending must be inline objects",
            ));
        }
    };

    let mut matching = None;
    for (index, blending) in blendings.iter().enumerate() {
        if class_name(blending, &records.classes)? != BLENDING_CLASS {
            return Err(schema_error(
                "perChannelBlending contains a record that is not DataBlending",
            ));
        }
        let (_, _, opacity_index) = validate_blending_shape(blending, &records.classes)?;
        if !is_base_color_blending(blending, &records.classes)? {
            continue;
        }
        if matching.is_some() {
            return Err(SppSemanticError::DuplicateBaseColor);
        }
        // The schema and channel token were validated above.
        let _ = opacity_index;
        matching = Some(index);
    }

    if let Some(index) = matching {
        let blending = blendings
            .get_mut(index)
            .ok_or_else(|| schema_error("Base Color blending index is out of range"))?;
        let opacity_index = required_property_index(blending, &records.classes, "opacity")?;
        match blending.fields.get_mut(opacity_index) {
            Some(SppRecordValue::F32Bits(bits)) => {
                let current = f32::from_bits(*bits);
                if !current.is_finite() || !(0.0..=1.0).contains(&current) {
                    return Err(SppSemanticError::InvalidOpacity);
                }
                *bits = opacity.to_bits();
            }
            _ => return Err(schema_error("DataBlending.opacity must be f32")),
        }
        return Ok(());
    }

    let class_index = records
        .classes
        .iter()
        .position(|class| class.name == BLENDING_CLASS)
        .ok_or_else(|| schema_error("DataBlending class definition is absent"))?;
    let new_blending =
        make_base_color_blending(&records.classes[class_index], class_index, opacity)?;
    blendings.push(new_blending);
    Ok(())
}

fn validate_label(label: &str) -> Result<(), SppSemanticError> {
    if label.len() > MAX_TEXT_BYTES {
        return Err(SppSemanticError::LabelTooLong);
    }
    Ok(())
}

fn read_layer(
    object: &SppObject,
    classes: &[SppClassDefinition],
) -> Result<SppLayerInfo, SppSemanticError> {
    validate_layer_shape(object, classes)?;
    let uid = match required_value(object, classes, "uid")? {
        SppRecordValue::U64(value) => *value,
        _ => return Err(schema_error("DataLayerColor.uid must be u64")),
    };
    let label = match required_value(object, classes, "label")? {
        SppRecordValue::Utf8(value) => value.clone(),
        _ => return Err(schema_error("DataLayerColor.label must be UTF-8")),
    };
    let enabled = match required_value(object, classes, "enabled")? {
        SppRecordValue::Bool(value) => *value,
        _ => return Err(schema_error("DataLayerColor.enabled must be boolean")),
    };
    let blendings = match required_value(object, classes, "perChannelBlending")? {
        SppRecordValue::InlineObjects(values) => values,
        _ => {
            return Err(schema_error(
                "DataLayerColor.perChannelBlending must be inline objects",
            ));
        }
    };
    let mut opacity = None;
    for blending in blendings {
        if class_name(blending, classes)? != BLENDING_CLASS {
            return Err(schema_error(
                "perChannelBlending contains a record that is not DataBlending",
            ));
        }
        let (_, _, opacity_index) = validate_blending_shape(blending, classes)?;
        if !is_base_color_blending(blending, classes)? {
            continue;
        }
        if opacity.is_some() {
            return Err(SppSemanticError::DuplicateBaseColor);
        }
        let value = match blending.fields.get(opacity_index) {
            Some(SppRecordValue::F32Bits(bits)) => f32::from_bits(*bits),
            _ => return Err(schema_error("DataBlending.opacity must be f32")),
        };
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(SppSemanticError::InvalidOpacity);
        }
        opacity = Some(value);
    }
    Ok(SppLayerInfo {
        uid,
        label,
        enabled,
        base_color_opacity: opacity.unwrap_or(1.0),
    })
}

fn validate_layer_shape(
    object: &SppObject,
    classes: &[SppClassDefinition],
) -> Result<(), SppSemanticError> {
    if class_name(object, classes)? != LAYER_CLASS {
        return Err(schema_error("expected DataLayerColor"));
    }
    let expected = [
        ("enabled", 10),
        ("label", 16),
        ("perChannelBlending", 17),
        ("uid", 12),
    ];
    validate_named_fields(object, classes, &expected)
}

fn validate_blending_shape(
    object: &SppObject,
    classes: &[SppClassDefinition],
) -> Result<(usize, usize, usize), SppSemanticError> {
    if class_name(object, classes)? != BLENDING_CLASS {
        return Err(schema_error("expected DataBlending"));
    }
    let expected = [("blendingMode", 9), ("opacity", 1)];
    validate_named_fields(object, classes, &expected)?;
    let channel_types_index = required_property_index(object, classes, "channelTypes")?;
    let class = &classes[object.class_index];
    let channel_type_code = class.properties[channel_types_index].type_code;
    if !matches!(channel_type_code, 12 | 22) {
        return Err(schema_error(
            "DataBlending.channelTypes must use observed type 12 or 22",
        ));
    }
    Ok((
        required_property_index(object, classes, "blendingMode")?,
        channel_types_index,
        required_property_index(object, classes, "opacity")?,
    ))
}

fn is_base_color_blending(
    blending: &SppObject,
    classes: &[SppClassDefinition],
) -> Result<bool, SppSemanticError> {
    let (_, channel_types_index, _) = validate_blending_shape(blending, classes)?;
    let class = classes
        .get(blending.class_index)
        .ok_or_else(|| schema_error("DataBlending class index is out of range"))?;
    let channel_type_code = class.properties[channel_types_index].type_code;
    let value = blending
        .fields
        .get(channel_types_index)
        .ok_or_else(|| schema_error("DataBlending.channelTypes value is missing"))?;
    match (channel_type_code, value) {
        (12, SppRecordValue::U64(mask)) => {
            if mask & BASE_COLOR_CHANNEL != 0 && *mask != BASE_COLOR_CHANNEL {
                return Err(schema_error(
                    "combined channel masks containing Base Color are unsupported",
                ));
            }
            Ok(*mask == BASE_COLOR_CHANNEL)
        }
        (22, SppRecordValue::Opaque16(token)) if *token == BASE_COLOR_CHANNEL_22 => Ok(true),
        (22, SppRecordValue::Opaque16(_)) => Err(schema_error(
            "unrecognized type-22 channel token; only the observed Base Color value is supported",
        )),
        (12, _) => Err(schema_error("type-12 channelTypes must be u64")),
        (22, _) => Err(schema_error(
            "type-22 channelTypes must be an opaque 16-byte value",
        )),
        _ => Err(schema_error("unsupported DataBlending.channelTypes type")),
    }
}

fn validate_named_fields(
    object: &SppObject,
    classes: &[SppClassDefinition],
    expected: &[(&str, u32)],
) -> Result<(), SppSemanticError> {
    let class = classes
        .get(object.class_index)
        .ok_or_else(|| schema_error("object class index is out of range"))?;
    if object.fields.len() != class.properties.len() {
        return Err(schema_error(
            "object field count differs from its class definition",
        ));
    }
    for (name, type_code) in expected {
        let property = class
            .properties
            .iter()
            .find(|property| property.name == *name)
            .ok_or_else(|| schema_error(format!("missing property {name}")))?;
        if property.type_code != *type_code {
            return Err(schema_error(format!("{name} has an unexpected type code")));
        }
        if class
            .properties
            .iter()
            .filter(|candidate| candidate.name == *name)
            .count()
            != 1
        {
            return Err(schema_error(format!("duplicate property {name}")));
        }
    }
    Ok(())
}

fn make_base_color_blending(
    class: &SppClassDefinition,
    class_index: usize,
    opacity: f32,
) -> Result<SppObject, SppSemanticError> {
    let expected = [("blendingMode", 9), ("opacity", 1)];
    if class.name != BLENDING_CLASS || class.properties.len() != 3 {
        return Err(schema_error(
            "cannot create blending record from an unrecognized definition",
        ));
    }
    validate_definition_fields(&class.properties, &expected)?;
    let channel_types: Vec<_> = class
        .properties
        .iter()
        .filter(|property| property.name == "channelTypes")
        .collect();
    if channel_types.len() != 1 || !matches!(channel_types[0].type_code, 12 | 22) {
        return Err(schema_error(
            "cannot create blending record from an unrecognized channelTypes definition",
        ));
    }
    let fields = class
        .properties
        .iter()
        .map(|property| match property.name.as_str() {
            "blendingMode" => Ok(SppRecordValue::U32(DEFAULT_BLENDING_MODE)),
            "channelTypes" if property.type_code == 12 => {
                Ok(SppRecordValue::U64(BASE_COLOR_CHANNEL))
            }
            "channelTypes" if property.type_code == 22 => {
                Ok(SppRecordValue::Opaque16(BASE_COLOR_CHANNEL_22))
            }
            "opacity" => Ok(SppRecordValue::F32Bits(opacity.to_bits())),
            _ => Err(schema_error("unrecognized DataBlending property")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SppObject {
        class_index,
        fields,
    })
}

fn validate_definition_fields(
    properties: &[SppPropertyDefinition],
    expected: &[(&str, u32)],
) -> Result<(), SppSemanticError> {
    for (name, type_code) in expected {
        let matches: Vec<_> = properties.iter().filter(|p| p.name == *name).collect();
        if matches.len() != 1 || matches[0].type_code != *type_code {
            return Err(schema_error(format!(
                "unsupported DataBlending definition for {name}"
            )));
        }
    }
    Ok(())
}

fn resolve_layer_path(
    records: &SppDocumentRecords,
    uid: u64,
) -> Result<Vec<PathStep>, SppSemanticError> {
    let root = records.root.as_ref().ok_or(SppSemanticError::MissingRoot)?;
    let mut found: Option<(Vec<PathStep>, bool)> = None;
    let mut visited = 0;
    walk(
        root,
        &records.classes,
        0,
        &mut visited,
        &mut Vec::new(),
        &mut |object, path| {
            if let Some(SppRecordValue::U64(found_uid)) =
                optional_value(object, &records.classes, "uid")?
            {
                if *found_uid == uid {
                    if found.is_some() {
                        return Err(SppSemanticError::DuplicateUid(uid));
                    }
                    found = Some((
                        path.to_vec(),
                        class_name(object, &records.classes)? == LAYER_CLASS,
                    ));
                }
            }
            Ok(())
        },
    )?;
    match found {
        None => Err(SppSemanticError::MissingUid(uid)),
        Some((_, false)) => Err(SppSemanticError::NotLayerId(uid)),
        Some((path, true)) => Ok(path),
    }
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

fn walk<'a>(
    object: &'a SppObject,
    classes: &'a [SppClassDefinition],
    depth: usize,
    visited: &mut usize,
    path: &mut Vec<PathStep>,
    visitor: &mut impl FnMut(&'a SppObject, &[PathStep]) -> Result<(), SppSemanticError>,
) -> Result<(), SppSemanticError> {
    if depth > MAX_OBJECT_DEPTH {
        return Err(SppSemanticError::DepthLimit);
    }
    *visited += 1;
    if *visited > MAX_OBJECTS {
        return Err(SppSemanticError::ObjectLimit);
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
    Ok(())
}

fn object_at_path_mut<'a>(
    root: &'a mut Option<SppObject>,
    path: &[PathStep],
) -> Result<&'a mut SppObject, SppSemanticError> {
    let mut object = root.as_mut().ok_or(SppSemanticError::MissingRoot)?;
    for step in path {
        let value = object
            .fields
            .get_mut(step.field_index)
            .ok_or_else(|| schema_error("layer path field is out of range"))?;
        object = match (value, step.child_index) {
            (SppRecordValue::Object(Some(child)), ChildIndex::Single) => child,
            (SppRecordValue::InlineObjects(children), ChildIndex::Array(index)) => children
                .get_mut(index)
                .ok_or_else(|| schema_error("layer path array index is out of range"))?,
            (SppRecordValue::PointerObjects(children), ChildIndex::Array(index)) => children
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| schema_error("layer path points to a null object"))?,
            _ => return Err(schema_error("layer path does not match the record value")),
        };
    }
    Ok(object)
}

fn class_name<'a>(
    object: &SppObject,
    classes: &'a [SppClassDefinition],
) -> Result<&'a str, SppSemanticError> {
    classes
        .get(object.class_index)
        .map(|class| class.name.as_str())
        .ok_or_else(|| schema_error("object class index is out of range"))
}

fn required_property_index(
    object: &SppObject,
    classes: &[SppClassDefinition],
    name: &str,
) -> Result<usize, SppSemanticError> {
    let class = classes
        .get(object.class_index)
        .ok_or_else(|| schema_error("object class index is out of range"))?;
    let mut matches = class
        .properties
        .iter()
        .enumerate()
        .filter(|(_, p)| p.name == name);
    let index = matches
        .next()
        .map(|(index, _)| index)
        .ok_or_else(|| schema_error(format!("missing property {name}")))?;
    if matches.next().is_some() {
        return Err(schema_error(format!("duplicate property {name}")));
    }
    if object.fields.len() != class.properties.len() {
        return Err(schema_error(
            "object field count differs from its class definition",
        ));
    }
    Ok(index)
}

fn required_value<'a>(
    object: &'a SppObject,
    classes: &[SppClassDefinition],
    name: &str,
) -> Result<&'a SppRecordValue, SppSemanticError> {
    let index = required_property_index(object, classes, name)?;
    object
        .fields
        .get(index)
        .ok_or_else(|| schema_error(format!("missing value for {name}")))
}

fn optional_value<'a>(
    object: &'a SppObject,
    classes: &[SppClassDefinition],
    name: &str,
) -> Result<Option<&'a SppRecordValue>, SppSemanticError> {
    let class = classes
        .get(object.class_index)
        .ok_or_else(|| schema_error("object class index is out of range"))?;
    let mut matches = class
        .properties
        .iter()
        .enumerate()
        .filter(|(_, p)| p.name == name);
    let Some((index, _)) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(schema_error(format!("duplicate property {name}")));
    }
    Ok(object.fields.get(index))
}

fn schema_error(message: impl Into<String>) -> SppSemanticError {
    SppSemanticError::UnsupportedSchema(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spp_document::{SppDocumentHeader, SppPropertyDefinition};

    fn property(name: &str, type_code: u32) -> SppPropertyDefinition {
        SppPropertyDefinition {
            name: name.to_owned(),
            type_code,
        }
    }

    fn fixture() -> SppDocumentRecords {
        let classes = vec![
            SppClassDefinition {
                name: "DataDocument".into(),
                properties: vec![property("uid", 12), property("layers", 17)],
            },
            SppClassDefinition {
                name: LAYER_CLASS.into(),
                properties: vec![
                    property("enabled", 10),
                    property("label", 16),
                    property("perChannelBlending", 17),
                    property("uid", 12),
                    property("futureField", 9),
                ],
            },
            SppClassDefinition {
                name: BLENDING_CLASS.into(),
                properties: vec![
                    property("blendingMode", 9),
                    property("channelTypes", 12),
                    property("opacity", 1),
                ],
            },
        ];
        let blending = SppObject {
            class_index: 2,
            fields: vec![
                SppRecordValue::U32(2),
                SppRecordValue::U64(BASE_COLOR_CHANNEL),
                SppRecordValue::F32Bits(0.5f32.to_bits()),
            ],
        };
        let layer = SppObject {
            class_index: 1,
            fields: vec![
                SppRecordValue::Bool(true),
                SppRecordValue::Utf8("Original".into()),
                SppRecordValue::InlineObjects(vec![blending]),
                SppRecordValue::U64(7),
                SppRecordValue::U32(0xAABB_CCDD),
            ],
        };
        SppDocumentRecords {
            header: SppDocumentHeader {
                magic: [0xdd, 0x2f, 0x7c, 0x1b],
                version: 1,
                preserved_word_8: 0x1234,
                preserved_word_12: 0x5678,
            },
            classes,
            root: Some(SppObject {
                class_index: 0,
                fields: vec![
                    SppRecordValue::U64(900),
                    SppRecordValue::InlineObjects(vec![layer]),
                ],
            }),
        }
    }

    fn layer_without_blend() -> SppDocumentRecords {
        let mut records = fixture();
        let root = records.root.as_mut().unwrap();
        let SppRecordValue::InlineObjects(layers) = &mut root.fields[1] else {
            unreachable!()
        };
        let SppRecordValue::InlineObjects(blends) = &mut layers[0].fields[2] else {
            unreachable!()
        };
        blends.clear();
        records
    }

    fn fixture_with_opaque_channel(token: [u8; 16]) -> SppDocumentRecords {
        let mut records = fixture();
        records.classes[2].properties[1].type_code = 22;
        let root = records.root.as_mut().unwrap();
        let SppRecordValue::InlineObjects(layers) = &mut root.fields[1] else {
            unreachable!()
        };
        let SppRecordValue::InlineObjects(blends) = &mut layers[0].fields[2] else {
            unreachable!()
        };
        blends[0].fields[1] = SppRecordValue::Opaque16(token);
        records
    }

    #[test]
    fn inspection_is_read_only_and_reports_observed_values() {
        let records = fixture();
        let original = records.clone();
        assert_eq!(
            enumerate_layers(&records).unwrap(),
            vec![SppLayerInfo {
                uid: 7,
                label: "Original".into(),
                enabled: true,
                base_color_opacity: 0.5,
            }]
        );
        assert_eq!(records, original);
        let encoded = records.encode().unwrap();
        assert_eq!(SppDocumentRecords::decode(&encoded).unwrap(), original);
    }

    #[test]
    fn edits_touch_only_layer_semantics_and_preserve_other_records() {
        let mut records = fixture();
        rename_layer(&mut records, 7, "Changed").unwrap();
        set_layer_visibility(&mut records, 7, false).unwrap();
        set_base_color_opacity(&mut records, 7, 0.375).unwrap();
        assert_eq!(
            enumerate_layers(&records).unwrap()[0],
            SppLayerInfo {
                uid: 7,
                label: "Changed".into(),
                enabled: false,
                base_color_opacity: 0.375,
            }
        );
        let root = records.root.as_ref().unwrap();
        let SppRecordValue::InlineObjects(layers) = &root.fields[1] else {
            unreachable!()
        };
        assert_eq!(layers[0].fields[4], SppRecordValue::U32(0xAABB_CCDD));
        let SppRecordValue::InlineObjects(blends) = &layers[0].fields[2] else {
            unreachable!()
        };
        assert_eq!(blends[0].fields[0], SppRecordValue::U32(2));
        assert_eq!(blends[0].fields[1], SppRecordValue::U64(BASE_COLOR_CHANNEL));
        let encoded = records.encode().unwrap();
        assert_eq!(SppDocumentRecords::decode(&encoded).unwrap(), records);
    }

    #[test]
    fn missing_base_color_record_is_added_from_the_exact_observed_class() {
        let mut records = layer_without_blend();
        set_base_color_opacity(&mut records, 7, 0.25).unwrap();
        assert_eq!(
            enumerate_layers(&records).unwrap()[0].base_color_opacity,
            0.25
        );
        let root = records.root.as_ref().unwrap();
        let SppRecordValue::InlineObjects(layers) = &root.fields[1] else {
            unreachable!()
        };
        let SppRecordValue::InlineObjects(blends) = &layers[0].fields[2] else {
            unreachable!()
        };
        assert_eq!(blends.len(), 1);
        assert_eq!(
            blends[0].fields[0],
            SppRecordValue::U32(DEFAULT_BLENDING_MODE)
        );
    }

    #[test]
    fn recognizes_only_the_observed_type_22_base_color_token() {
        let mut records = fixture_with_opaque_channel(BASE_COLOR_CHANNEL_22);
        assert_eq!(
            enumerate_layers(&records).unwrap()[0].base_color_opacity,
            0.5
        );
        set_base_color_opacity(&mut records, 7, 0.25).unwrap();
        let root = records.root.as_ref().unwrap();
        let SppRecordValue::InlineObjects(layers) = &root.fields[1] else {
            unreachable!()
        };
        let SppRecordValue::InlineObjects(blends) = &layers[0].fields[2] else {
            unreachable!()
        };
        assert_eq!(
            blends[0].fields[1],
            SppRecordValue::Opaque16(BASE_COLOR_CHANNEL_22)
        );
        let encoded = records.encode().unwrap();
        assert_eq!(SppDocumentRecords::decode(&encoded).unwrap(), records);

        let mut unknown = fixture_with_opaque_channel([0xff; 16]);
        let before = unknown.clone();
        assert!(matches!(
            enumerate_layers(&unknown),
            Err(SppSemanticError::UnsupportedSchema(_))
        ));
        assert!(matches!(
            set_base_color_opacity(&mut unknown, 7, 0.125),
            Err(SppSemanticError::UnsupportedSchema(_))
        ));
        assert_eq!(unknown, before);
    }

    #[test]
    fn inserts_only_when_type_22_schema_has_the_known_channel_token() {
        let mut records = layer_without_blend();
        records.classes[2].properties[1].type_code = 22;
        set_base_color_opacity(&mut records, 7, 0.25).unwrap();
        let root = records.root.as_ref().unwrap();
        let SppRecordValue::InlineObjects(layers) = &root.fields[1] else {
            unreachable!()
        };
        let SppRecordValue::InlineObjects(blends) = &layers[0].fields[2] else {
            unreachable!()
        };
        assert_eq!(
            blends[0].fields[1],
            SppRecordValue::Opaque16(BASE_COLOR_CHANNEL_22)
        );
        assert_eq!(
            enumerate_layers(&records).unwrap()[0].base_color_opacity,
            0.25
        );
    }

    #[test]
    fn duplicate_missing_and_non_layer_uids_are_distinguished() {
        let mut duplicate = fixture();
        let root = duplicate.root.as_mut().unwrap();
        let SppRecordValue::InlineObjects(layers) = &mut root.fields[1] else {
            unreachable!()
        };
        layers.push(layers[0].clone());
        assert_eq!(
            enumerate_layers(&duplicate),
            Err(SppSemanticError::DuplicateUid(7))
        );
        assert_eq!(
            rename_layer(&mut duplicate, 7, "ambiguous"),
            Err(SppSemanticError::DuplicateUid(7))
        );

        let mut records = fixture();
        assert_eq!(
            set_layer_visibility(&mut records, 404, false),
            Err(SppSemanticError::MissingUid(404))
        );
        assert_eq!(
            set_layer_visibility(&mut records, 900, false),
            Err(SppSemanticError::NotLayerId(900))
        );
    }

    #[test]
    fn rejects_invalid_opacity_and_overlong_labels() {
        for value in [f32::NAN, f32::INFINITY, -0.001, 1.001] {
            assert_eq!(
                set_base_color_opacity(&mut fixture(), 7, value),
                Err(SppSemanticError::InvalidOpacity)
            );
        }
        assert_eq!(
            rename_layer(&mut fixture(), 7, &"x".repeat(MAX_TEXT_BYTES + 1)),
            Err(SppSemanticError::LabelTooLong)
        );
    }

    #[test]
    fn traversal_enforces_the_document_depth_budget() {
        let class = SppClassDefinition {
            name: "Node".into(),
            properties: vec![property("next", 18)],
        };
        let mut root = SppObject {
            class_index: 0,
            fields: vec![SppRecordValue::Object(None)],
        };
        let mut cursor = &mut root;
        for _ in 0..=MAX_OBJECT_DEPTH {
            cursor.fields[0] = SppRecordValue::Object(Some(Box::new(SppObject {
                class_index: 0,
                fields: vec![SppRecordValue::Object(None)],
            })));
            let SppRecordValue::Object(Some(next)) = &mut cursor.fields[0] else {
                unreachable!()
            };
            cursor = next;
        }
        let records = SppDocumentRecords {
            header: SppDocumentHeader {
                magic: [0xdd, 0x2f, 0x7c, 0x1b],
                version: 1,
                preserved_word_8: 0,
                preserved_word_12: 0,
            },
            classes: vec![class],
            root: Some(root),
        };
        assert_eq!(
            enumerate_layers(&records),
            Err(SppSemanticError::DepthLimit)
        );
    }

    #[test]
    fn combined_base_color_masks_are_rejected_without_mutation() {
        let mut records = fixture();
        let root = records.root.as_mut().unwrap();
        let SppRecordValue::InlineObjects(layers) = &mut root.fields[1] else {
            unreachable!()
        };
        let SppRecordValue::InlineObjects(blends) = &mut layers[0].fields[2] else {
            unreachable!()
        };
        blends[0].fields[1] = SppRecordValue::U64(3);
        let before = records.clone();
        assert!(matches!(
            enumerate_layers(&records),
            Err(SppSemanticError::UnsupportedSchema(_))
        ));
        assert!(matches!(
            set_base_color_opacity(&mut records, 7, 0.25),
            Err(SppSemanticError::UnsupportedSchema(_))
        ));
        assert_eq!(records, before);
    }

    #[test]
    fn unknown_per_channel_records_are_rejected_without_mutation() {
        let mut records = fixture();
        records.classes.push(SppClassDefinition {
            name: "DataOtherBlend".into(),
            properties: vec![property("value", 9)],
        });
        let root = records.root.as_mut().unwrap();
        let SppRecordValue::InlineObjects(layers) = &mut root.fields[1] else {
            unreachable!()
        };
        let SppRecordValue::InlineObjects(blends) = &mut layers[0].fields[2] else {
            unreachable!()
        };
        blends.push(SppObject {
            class_index: 3,
            fields: vec![SppRecordValue::U32(42)],
        });
        let before = records.clone();
        assert!(matches!(
            enumerate_layers(&records),
            Err(SppSemanticError::UnsupportedSchema(_))
        ));
        assert!(matches!(
            set_base_color_opacity(&mut records, 7, 0.25),
            Err(SppSemanticError::UnsupportedSchema(_))
        ));
        assert_eq!(records, before);
    }

    #[test]
    fn invalid_stored_opacity_is_rejected_without_mutation() {
        for value in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            let mut records = fixture();
            let root = records.root.as_mut().unwrap();
            let SppRecordValue::InlineObjects(layers) = &mut root.fields[1] else {
                unreachable!()
            };
            let SppRecordValue::InlineObjects(blends) = &mut layers[0].fields[2] else {
                unreachable!()
            };
            blends[0].fields[2] = SppRecordValue::F32Bits(value.to_bits());
            let before = records.clone();
            assert_eq!(
                enumerate_layers(&records),
                Err(SppSemanticError::InvalidOpacity)
            );
            assert_eq!(
                set_base_color_opacity(&mut records, 7, 0.25),
                Err(SppSemanticError::InvalidOpacity)
            );
            assert_eq!(records, before);
        }
    }
}
