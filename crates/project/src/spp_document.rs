//! Experimental clean-room codec for the reflective records observed in bounded SPP
//! `document.bin` research samples.
//!
//! This module does not parse HDF5, images, meshes, resources, project layers, or Painter's full
//! project format. It accepts only the explicitly implemented primitive type codes below and
//! rejects every unresolved value. In particular, type 11 is accepted only when its length is
//! zero. Float values are stored as raw IEEE-754 bits so re-encoding does not normalize NaNs,
//! signed zero, or other bit patterns.

const MAGIC: [u8; 4] = [0xdd, 0x2f, 0x7c, 0x1b];
const HEADER_BYTES: usize = 20;
const CLASS_DEFINITION: u32 = u32::MAX;

pub const MAX_DATASET_BYTES: usize = 65_536;
pub const MAX_TEXT_BYTES: usize = 4_096;
pub const MAX_DEFINITION_TEXT_BYTES: usize = 128;
pub const MAX_PROPERTIES: usize = 128;
pub const MAX_CLASSES: usize = 256;
pub const MAX_COUNT: usize = 4_096;
pub const MAX_OBJECT_DEPTH: usize = 32;
pub const MAX_OBJECTS: usize = 4_096;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SppDocumentError {
    #[error("unsupported reflective document header")]
    InvalidHeader,
    #[error("unsupported reflective document version {0}")]
    UnsupportedVersion(u32),
    #[error("reflective document exceeds the {MAX_DATASET_BYTES}-byte limit")]
    DatasetTooLarge,
    #[error("truncated reflective document")]
    Truncated,
    #[error("record count exceeds its limit")]
    CountLimit,
    #[error("record text exceeds its byte limit")]
    TextLimit,
    #[error("record text is not valid UTF-8")]
    InvalidUtf8,
    #[error("duplicate property definition")]
    DuplicateProperty,
    #[error("duplicate class definition")]
    DuplicateClass,
    #[error("class table count does not match the header")]
    ClassCountMismatch,
    #[error("record refers to unknown class ID {0}")]
    UnknownClass(u32),
    #[error("invalid absolute object end pointer")]
    InvalidEndPointer,
    #[error("object end pointer does not match the parsed record")]
    ObjectEndMismatch,
    #[error("reflective object nesting exceeds its limit")]
    ObjectDepthLimit,
    #[error("reflective object count exceeds its limit")]
    ObjectCountLimit,
    #[error("reflective document has trailing bytes")]
    TrailingBytes,
    #[error("unsupported reflective property type {0}")]
    UnknownType(u32),
    #[error("unresolved type 11 must have zero length, found {0}")]
    NonEmptyType11(u32),
    #[error("boolean byte must be 0 or 255, found {0}")]
    InvalidBoolean(u8),
    #[error("record does not match its class definition")]
    InvalidRecord,
    #[error("class IDs must be introduced in definition order")]
    ClassDefinitionOrder,
    #[error("encoded class table is incomplete")]
    IncompleteClassTable,
}

/// Header fields currently understood by the clean-room record codec. The words at byte offsets
/// 8 and 12 have no assigned meaning here and are retained verbatim as integers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppDocumentHeader {
    pub magic: [u8; 4],
    pub version: u32,
    pub preserved_word_8: u32,
    pub preserved_word_12: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppPropertyDefinition {
    pub name: String,
    pub type_code: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppClassDefinition {
    pub name: String,
    pub properties: Vec<SppPropertyDefinition>,
}

/// Neutral value representation for the implemented primitive type codes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SppRecordValue {
    /// Exact little-endian IEEE-754 payload bits, without float conversion.
    F32Bits(u32),
    /// Fixed 16-byte primitive payload, preserved without assigning component semantics.
    Opaque16([u8; 16]),
    U32Pair([u32; 2]),
    U32(u32),
    Bool(bool),
    U64(u64),
    Utf8(String),
    /// Nullable pointer object (type 18 or 20).
    Object(Option<Box<SppObject>>),
    /// Inline object sequence without per-object end pointers (type 17).
    InlineObjects(Vec<SppObject>),
    /// Nullable pointer object sequence (type 19).
    PointerObjects(Vec<Option<SppObject>>),
    /// Empty value of unresolved type 11. Nonempty instances are rejected.
    EmptyType11,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppObject {
    pub class_index: usize,
    /// Ordered to match the referenced class definition, including unrecognized property names.
    pub fields: Vec<SppRecordValue>,
}

impl SppObject {
    pub fn field<'a>(
        &'a self,
        classes: &'a [SppClassDefinition],
        property: &str,
    ) -> Option<&'a SppRecordValue> {
        let class = classes.get(self.class_index)?;
        let index = class.properties.iter().position(|p| p.name == property)?;
        self.fields.get(index)
    }

    pub fn field_mut<'a>(
        &'a mut self,
        classes: &[SppClassDefinition],
        property: &str,
    ) -> Option<&'a mut SppRecordValue> {
        let index = classes
            .get(self.class_index)?
            .properties
            .iter()
            .position(|p| p.name == property)?;
        self.fields.get_mut(index)
    }
}

/// One reflective root record and its inline class table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppDocumentRecords {
    pub header: SppDocumentHeader,
    pub classes: Vec<SppClassDefinition>,
    pub root: Option<SppObject>,
}

impl SppDocumentRecords {
    /// Decode the accepted record subset from one already-extracted `document.bin` dataset.
    /// This function does not inspect or extract an HDF5 container.
    pub fn decode(bytes: &[u8]) -> Result<Self, SppDocumentError> {
        if bytes.len() > MAX_DATASET_BYTES {
            return Err(SppDocumentError::DatasetTooLarge);
        }
        if bytes.len() < HEADER_BYTES + 4 || bytes[..4] != MAGIC {
            return Err(SppDocumentError::InvalidHeader);
        }
        let version = read_u32_at(bytes, 4)?;
        if version != 1 {
            return Err(SppDocumentError::UnsupportedVersion(version));
        }
        let header = SppDocumentHeader {
            magic: MAGIC,
            version,
            preserved_word_8: read_u32_at(bytes, 8)?,
            preserved_word_12: read_u32_at(bytes, 12)?,
        };
        let declared_classes = read_u32_at(bytes, 16)? as usize;
        if declared_classes > MAX_CLASSES {
            return Err(SppDocumentError::CountLimit);
        }

        let mut reader = Reader {
            bytes,
            cursor: HEADER_BYTES,
            classes: Vec::new(),
            object_count: 0,
        };
        let root = reader.read_object(true, 0)?;
        if reader.cursor != bytes.len() {
            return Err(SppDocumentError::TrailingBytes);
        }
        if reader.classes.len() != declared_classes {
            return Err(SppDocumentError::ClassCountMismatch);
        }
        Ok(Self {
            header,
            classes: reader.classes,
            root,
        })
    }

    /// Re-encode accepted records. Pointer ends are recomputed as absolute output offsets, the
    /// class count is updated, and the two unassigned header words are preserved.
    pub fn encode(&self) -> Result<Vec<u8>, SppDocumentError> {
        if self.header.magic != MAGIC {
            return Err(SppDocumentError::InvalidHeader);
        }
        if self.header.version != 1 {
            return Err(SppDocumentError::UnsupportedVersion(self.header.version));
        }
        validate_class_table(&self.classes)?;
        if self.classes.len() > MAX_CLASSES {
            return Err(SppDocumentError::CountLimit);
        }

        let mut writer = Writer {
            bytes: Vec::with_capacity(1024),
            classes: &self.classes,
            emitted_classes: vec![false; self.classes.len()],
            next_definition: 0,
            object_count: 0,
        };
        writer.put(&self.header.magic)?;
        writer.put_u32(self.header.version)?;
        writer.put_u32(self.header.preserved_word_8)?;
        writer.put_u32(self.header.preserved_word_12)?;
        writer.put_u32(self.classes.len() as u32)?;
        writer.write_object(self.root.as_ref(), true, 0)?;
        if writer.emitted_classes.iter().any(|emitted| !emitted) {
            return Err(SppDocumentError::IncompleteClassTable);
        }
        Ok(writer.bytes)
    }
}

fn read_u32_at(bytes: &[u8], offset: usize) -> Result<u32, SppDocumentError> {
    let end = offset.checked_add(4).ok_or(SppDocumentError::Truncated)?;
    let word = bytes.get(offset..end).ok_or(SppDocumentError::Truncated)?;
    Ok(u32::from_le_bytes(
        word.try_into().map_err(|_| SppDocumentError::Truncated)?,
    ))
}

fn validate_class_table(classes: &[SppClassDefinition]) -> Result<(), SppDocumentError> {
    let mut names = std::collections::HashSet::with_capacity(classes.len());
    for class in classes {
        validate_text(&class.name, MAX_DEFINITION_TEXT_BYTES)?;
        if !names.insert(class.name.as_str()) {
            return Err(SppDocumentError::DuplicateClass);
        }
        if class.properties.len() > MAX_PROPERTIES {
            return Err(SppDocumentError::CountLimit);
        }
        let mut properties = std::collections::HashSet::with_capacity(class.properties.len());
        for property in &class.properties {
            validate_text(&property.name, MAX_DEFINITION_TEXT_BYTES)?;
            if !properties.insert(property.name.as_str()) {
                return Err(SppDocumentError::DuplicateProperty);
            }
            check_supported_type(property.type_code)?;
        }
    }
    Ok(())
}

fn validate_text(value: &str, cap: usize) -> Result<(), SppDocumentError> {
    if value.len() > cap {
        return Err(SppDocumentError::TextLimit);
    }
    Ok(())
}

fn check_supported_type(type_code: u32) -> Result<(), SppDocumentError> {
    match type_code {
        1 | 4 | 6 | 9 | 10 | 11 | 12 | 15 | 16 | 17 | 18 | 19 | 20 | 21 | 22 => Ok(()),
        other => Err(SppDocumentError::UnknownType(other)),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
    classes: Vec<SppClassDefinition>,
    object_count: usize,
}

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], SppDocumentError> {
        let end = self
            .cursor
            .checked_add(count)
            .ok_or(SppDocumentError::Truncated)?;
        let data = self
            .bytes
            .get(self.cursor..end)
            .ok_or(SppDocumentError::Truncated)?;
        self.cursor = end;
        Ok(data)
    }

    fn u8(&mut self) -> Result<u8, SppDocumentError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, SppDocumentError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes(
            bytes.try_into().map_err(|_| SppDocumentError::Truncated)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, SppDocumentError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes(
            bytes.try_into().map_err(|_| SppDocumentError::Truncated)?,
        ))
    }

    fn count(&mut self, cap: usize) -> Result<usize, SppDocumentError> {
        let count = self.u32()? as usize;
        if count > cap {
            return Err(SppDocumentError::CountLimit);
        }
        Ok(count)
    }

    fn text(&mut self, cap: usize) -> Result<String, SppDocumentError> {
        let count = self.count(cap)?;
        let bytes = self.take(count)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| SppDocumentError::InvalidUtf8)
    }

    fn read_class_definition(&mut self) -> Result<usize, SppDocumentError> {
        if self.classes.len() >= MAX_CLASSES {
            return Err(SppDocumentError::CountLimit);
        }
        let name = self.text(MAX_DEFINITION_TEXT_BYTES)?;
        if self.classes.iter().any(|class| class.name == name) {
            return Err(SppDocumentError::DuplicateClass);
        }
        let count = self.count(MAX_PROPERTIES)?;
        let mut properties = Vec::with_capacity(count);
        let mut property_names = std::collections::HashSet::with_capacity(count);
        for _ in 0..count {
            let property_name = self.text(MAX_DEFINITION_TEXT_BYTES)?;
            if !property_names.insert(property_name.clone()) {
                return Err(SppDocumentError::DuplicateProperty);
            }
            let type_code = self.u32()?;
            check_supported_type(type_code)?;
            properties.push(SppPropertyDefinition {
                name: property_name,
                type_code,
            });
        }
        let index = self.classes.len();
        self.classes.push(SppClassDefinition { name, properties });
        Ok(index)
    }

    fn read_object(
        &mut self,
        pointer: bool,
        depth: usize,
    ) -> Result<Option<SppObject>, SppDocumentError> {
        if depth > MAX_OBJECT_DEPTH {
            return Err(SppDocumentError::ObjectDepthLimit);
        }
        self.object_count = self
            .object_count
            .checked_add(1)
            .ok_or(SppDocumentError::ObjectCountLimit)?;
        if self.object_count > MAX_OBJECTS {
            return Err(SppDocumentError::ObjectCountLimit);
        }

        let start = self.cursor;
        let expected_end = if pointer {
            let end = self.u32()? as usize;
            if end == 0 {
                return Ok(None);
            }
            if end < self.cursor || end > self.bytes.len() {
                return Err(SppDocumentError::InvalidEndPointer);
            }
            Some(end)
        } else {
            None
        };
        let class_id = self.u32()?;
        let class_index = if class_id == CLASS_DEFINITION {
            self.read_class_definition()?
        } else {
            let index = class_id as usize;
            if index >= self.classes.len() {
                return Err(SppDocumentError::UnknownClass(class_id));
            }
            index
        };
        let class = self
            .classes
            .get(class_index)
            .ok_or(SppDocumentError::UnknownClass(class_id))?;
        let definitions = class.properties.clone();
        let mut fields = Vec::with_capacity(definitions.len());
        for property in &definitions {
            fields.push(self.read_value(property.type_code, depth + 1)?);
        }
        if let Some(end) = expected_end {
            if self.cursor != end {
                return Err(SppDocumentError::ObjectEndMismatch);
            }
        }
        if self.cursor <= start {
            return Err(SppDocumentError::Truncated);
        }
        Ok(Some(SppObject {
            class_index,
            fields,
        }))
    }

    fn read_value(
        &mut self,
        type_code: u32,
        depth: usize,
    ) -> Result<SppRecordValue, SppDocumentError> {
        Ok(match type_code {
            1 => SppRecordValue::F32Bits(self.u32()?),
            4 | 22 => {
                let bytes = self.take(16)?;
                SppRecordValue::Opaque16(bytes.try_into().map_err(|_| SppDocumentError::Truncated)?)
            }
            6 => SppRecordValue::U32Pair([self.u32()?, self.u32()?]),
            9 => SppRecordValue::U32(self.u32()?),
            10 => match self.u8()? {
                0 => SppRecordValue::Bool(false),
                255 => SppRecordValue::Bool(true),
                other => return Err(SppDocumentError::InvalidBoolean(other)),
            },
            12 | 15 | 21 => SppRecordValue::U64(self.u64()?),
            16 => SppRecordValue::Utf8(self.text(MAX_TEXT_BYTES)?),
            17 => {
                let count = self.count(MAX_COUNT)?;
                let mut objects = Vec::with_capacity(count);
                for _ in 0..count {
                    objects.push(
                        self.read_object(false, depth)?
                            .ok_or(SppDocumentError::InvalidRecord)?,
                    );
                }
                SppRecordValue::InlineObjects(objects)
            }
            18 | 20 => SppRecordValue::Object(self.read_object(true, depth)?.map(Box::new)),
            19 => {
                let count = self.count(MAX_COUNT)?;
                let mut objects = Vec::with_capacity(count);
                for _ in 0..count {
                    objects.push(self.read_object(true, depth)?);
                }
                SppRecordValue::PointerObjects(objects)
            }
            11 => {
                let count = self.u32()?;
                if count != 0 {
                    return Err(SppDocumentError::NonEmptyType11(count));
                }
                SppRecordValue::EmptyType11
            }
            other => return Err(SppDocumentError::UnknownType(other)),
        })
    }
}

struct Writer<'a> {
    bytes: Vec<u8>,
    classes: &'a [SppClassDefinition],
    emitted_classes: Vec<bool>,
    next_definition: usize,
    object_count: usize,
}

impl Writer<'_> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), SppDocumentError> {
        let len = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(SppDocumentError::DatasetTooLarge)?;
        if len > MAX_DATASET_BYTES {
            return Err(SppDocumentError::DatasetTooLarge);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn put_u8(&mut self, value: u8) -> Result<(), SppDocumentError> {
        self.put(&[value])
    }

    fn put_u32(&mut self, value: u32) -> Result<(), SppDocumentError> {
        self.put(&value.to_le_bytes())
    }

    fn put_u64(&mut self, value: u64) -> Result<(), SppDocumentError> {
        self.put(&value.to_le_bytes())
    }

    fn put_text(&mut self, value: &str, cap: usize) -> Result<(), SppDocumentError> {
        validate_text(value, cap)?;
        let len = u32::try_from(value.len()).map_err(|_| SppDocumentError::TextLimit)?;
        self.put_u32(len)?;
        self.put(value.as_bytes())
    }

    fn patch_u32(&mut self, offset: usize, value: u32) -> Result<(), SppDocumentError> {
        let bytes = value.to_le_bytes();
        let end = offset
            .checked_add(4)
            .ok_or(SppDocumentError::InvalidRecord)?;
        let slot = self
            .bytes
            .get_mut(offset..end)
            .ok_or(SppDocumentError::InvalidRecord)?;
        slot.copy_from_slice(&bytes);
        Ok(())
    }

    fn write_object(
        &mut self,
        object: Option<&SppObject>,
        pointer: bool,
        depth: usize,
    ) -> Result<(), SppDocumentError> {
        if depth > MAX_OBJECT_DEPTH {
            return Err(SppDocumentError::ObjectDepthLimit);
        }
        self.object_count = self
            .object_count
            .checked_add(1)
            .ok_or(SppDocumentError::ObjectCountLimit)?;
        if self.object_count > MAX_OBJECTS {
            return Err(SppDocumentError::ObjectCountLimit);
        }

        let end_pointer = if pointer {
            let position = self.bytes.len();
            self.put_u32(0)?;
            if object.is_none() {
                return Ok(());
            }
            Some(position)
        } else {
            None
        };
        let object = object.ok_or(SppDocumentError::InvalidRecord)?;
        let properties = self
            .classes
            .get(object.class_index)
            .ok_or(SppDocumentError::InvalidRecord)?
            .properties
            .clone();
        if object.fields.len() != properties.len() {
            return Err(SppDocumentError::InvalidRecord);
        }
        self.write_class_reference(object.class_index)?;
        // The small, capped schema is owned here so nested writes can mutably borrow the writer.
        for (property, value) in properties.iter().zip(&object.fields) {
            self.write_value(property.type_code, value, depth + 1)?;
        }
        if let Some(offset) = end_pointer {
            let end =
                u32::try_from(self.bytes.len()).map_err(|_| SppDocumentError::DatasetTooLarge)?;
            self.patch_u32(offset, end)?;
        }
        Ok(())
    }

    fn write_class_reference(&mut self, class_index: usize) -> Result<(), SppDocumentError> {
        let class = self
            .classes
            .get(class_index)
            .ok_or(SppDocumentError::InvalidRecord)?
            .clone();
        if !self.emitted_classes[class_index] {
            if class_index != self.next_definition {
                return Err(SppDocumentError::ClassDefinitionOrder);
            }
            self.put_u32(CLASS_DEFINITION)?;
            self.put_text(&class.name, MAX_DEFINITION_TEXT_BYTES)?;
            self.put_u32(class.properties.len() as u32)?;
            for property in &class.properties {
                self.put_text(&property.name, MAX_DEFINITION_TEXT_BYTES)?;
                self.put_u32(property.type_code)?;
            }
            self.emitted_classes[class_index] = true;
            self.next_definition += 1;
        } else {
            self.put_u32(class_index as u32)?;
        }
        Ok(())
    }

    fn write_value(
        &mut self,
        type_code: u32,
        value: &SppRecordValue,
        depth: usize,
    ) -> Result<(), SppDocumentError> {
        match (type_code, value) {
            (1, SppRecordValue::F32Bits(bits)) => self.put_u32(*bits),
            (4 | 22, SppRecordValue::Opaque16(bytes)) => self.put(bytes),
            (6, SppRecordValue::U32Pair(values)) => {
                self.put_u32(values[0])?;
                self.put_u32(values[1])
            }
            (9, SppRecordValue::U32(value)) => self.put_u32(*value),
            (10, SppRecordValue::Bool(value)) => self.put_u8(if *value { 255 } else { 0 }),
            (12 | 15 | 21, SppRecordValue::U64(value)) => self.put_u64(*value),
            (16, SppRecordValue::Utf8(value)) => self.put_text(value, MAX_TEXT_BYTES),
            (17, SppRecordValue::InlineObjects(objects)) => {
                if objects.len() > MAX_COUNT {
                    return Err(SppDocumentError::CountLimit);
                }
                self.put_u32(objects.len() as u32)?;
                for object in objects {
                    self.write_object(Some(object), false, depth)?;
                }
                Ok(())
            }
            (18 | 20, SppRecordValue::Object(object)) => {
                self.write_object(object.as_deref(), true, depth)
            }
            (19, SppRecordValue::PointerObjects(objects)) => {
                if objects.len() > MAX_COUNT {
                    return Err(SppDocumentError::CountLimit);
                }
                self.put_u32(objects.len() as u32)?;
                for object in objects {
                    self.write_object(object.as_ref(), true, depth)?;
                }
                Ok(())
            }
            (11, SppRecordValue::EmptyType11) => self.put_u32(0),
            (11, _) => Err(SppDocumentError::NonEmptyType11(1)),
            (type_code, _) => Err(SppDocumentError::UnknownType(type_code)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> SppDocumentRecords {
        SppDocumentRecords {
            header: SppDocumentHeader {
                magic: MAGIC,
                version: 1,
                preserved_word_8: 88,
                preserved_word_12: 18,
            },
            classes: vec![SppClassDefinition {
                name: "DataTest".into(),
                properties: vec![
                    SppPropertyDefinition {
                        name: "name".into(),
                        type_code: 16,
                    },
                    SppPropertyDefinition {
                        name: "flag".into(),
                        type_code: 10,
                    },
                    SppPropertyDefinition {
                        name: "value".into(),
                        type_code: 1,
                    },
                    SppPropertyDefinition {
                        name: "child".into(),
                        type_code: 18,
                    },
                ],
            }],
            root: Some(SppObject {
                class_index: 0,
                fields: vec![
                    SppRecordValue::Utf8("Original".into()),
                    SppRecordValue::Bool(true),
                    SppRecordValue::F32Bits(0x8000_0000), // negative zero remains bit-exact
                    SppRecordValue::Object(None),
                ],
            }),
        }
    }

    fn raw_fixture() -> Vec<u8> {
        fixture().encode().unwrap()
    }

    #[test]
    fn exact_roundtrip_preserves_header_words_and_float_bits_and_supports_edits() {
        let bytes = raw_fixture();
        let mut decoded = SppDocumentRecords::decode(&bytes).unwrap();
        assert_eq!(decoded.header.preserved_word_8, 88);
        assert_eq!(decoded.header.preserved_word_12, 18);
        assert_eq!(decoded.classes.len(), 1);
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert_eq!(
            decoded
                .root
                .as_ref()
                .unwrap()
                .field(&decoded.classes, "value"),
            Some(&SppRecordValue::F32Bits(0x8000_0000))
        );

        *decoded
            .root
            .as_mut()
            .unwrap()
            .field_mut(&decoded.classes, "name")
            .unwrap() = SppRecordValue::Utf8("A longer record label".into());
        *decoded
            .root
            .as_mut()
            .unwrap()
            .field_mut(&decoded.classes, "flag")
            .unwrap() = SppRecordValue::Bool(false);
        let edited = decoded.encode().unwrap();
        assert_eq!(
            u32::from_le_bytes(edited[20..24].try_into().unwrap()) as usize,
            edited.len()
        );
        assert_eq!(&edited[8..16], &bytes[8..16]);
        let reopened = SppDocumentRecords::decode(&edited).unwrap();
        assert_eq!(
            reopened
                .root
                .as_ref()
                .unwrap()
                .field(&reopened.classes, "name"),
            Some(&SppRecordValue::Utf8("A longer record label".into()))
        );
        assert_eq!(
            reopened
                .root
                .as_ref()
                .unwrap()
                .field(&reopened.classes, "flag"),
            Some(&SppRecordValue::Bool(false))
        );
    }

    #[test]
    fn every_truncated_prefix_is_rejected() {
        let bytes = raw_fixture();
        for end in 0..bytes.len() {
            assert!(
                SppDocumentRecords::decode(&bytes[..end]).is_err(),
                "accepted prefix length {end}"
            );
        }
    }

    #[test]
    fn rejects_oversize_bad_absolute_pointers_and_header_class_count_mismatch() {
        assert_eq!(
            SppDocumentRecords::decode(&vec![0; MAX_DATASET_BYTES + 1]),
            Err(SppDocumentError::DatasetTooLarge)
        );
        let bytes = raw_fixture();
        let mut bad_pointer = bytes.clone();
        bad_pointer[20..24].copy_from_slice(&((bytes.len() as u32) + 1).to_le_bytes());
        assert_eq!(
            SppDocumentRecords::decode(&bad_pointer),
            Err(SppDocumentError::InvalidEndPointer)
        );
        let mut bad_class_count = bytes;
        bad_class_count[16..20].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(
            SppDocumentRecords::decode(&bad_class_count),
            Err(SppDocumentError::ClassCountMismatch)
        );

        let mut excessive_classes = raw_fixture();
        excessive_classes[16..20].copy_from_slice(&((MAX_CLASSES as u32) + 1).to_le_bytes());
        assert_eq!(
            SppDocumentRecords::decode(&excessive_classes),
            Err(SppDocumentError::CountLimit)
        );
    }

    #[test]
    fn rejects_duplicate_properties_and_duplicate_class_names() {
        let bytes = raw_fixture();
        // Both names are four bytes in the synthetic class definition.
        let mut duplicate_property = bytes.clone();
        let property = duplicate_property
            .windows(4)
            .position(|part| part == b"flag")
            .unwrap();
        duplicate_property[property..property + 4].copy_from_slice(b"name");
        assert_eq!(
            SppDocumentRecords::decode(&duplicate_property),
            Err(SppDocumentError::DuplicateProperty)
        );

        let nested = SppDocumentRecords {
            header: fixture().header,
            classes: vec![
                SppClassDefinition {
                    name: "RootX".into(),
                    properties: vec![SppPropertyDefinition {
                        name: "child".into(),
                        type_code: 18,
                    }],
                },
                SppClassDefinition {
                    name: "Child".into(),
                    properties: vec![],
                },
            ],
            root: Some(SppObject {
                class_index: 0,
                fields: vec![SppRecordValue::Object(Some(Box::new(SppObject {
                    class_index: 1,
                    fields: vec![],
                })))],
            }),
        };
        let mut duplicate_class = nested.encode().unwrap();
        let child_name = duplicate_class
            .windows(5)
            .position(|part| part == b"Child")
            .unwrap();
        duplicate_class[child_name..child_name + 5].copy_from_slice(b"RootX");
        assert_eq!(
            SppDocumentRecords::decode(&duplicate_class),
            Err(SppDocumentError::DuplicateClass)
        );
    }

    #[test]
    fn rejects_unknown_types_nonempty_type_11_and_invalid_bool_encodings() {
        let mut unknown = fixture();
        unknown.classes[0].properties[0].type_code = 23;
        assert_eq!(unknown.encode(), Err(SppDocumentError::UnknownType(23)));

        let mut unknown_input = raw_fixture();
        // Synthetic layout: first property type word follows the name at bytes 48..52.
        unknown_input[52..56].copy_from_slice(&23u32.to_le_bytes());
        assert_eq!(
            SppDocumentRecords::decode(&unknown_input),
            Err(SppDocumentError::UnknownType(23))
        );

        let mut nonempty = fixture();
        nonempty.classes[0].properties[0] = SppPropertyDefinition {
            name: "opaque".into(),
            type_code: 11,
        };
        nonempty.root.as_mut().unwrap().fields[0] = SppRecordValue::Utf8("bad".into());
        assert!(matches!(
            nonempty.encode(),
            Err(SppDocumentError::NonEmptyType11(_))
        ));

        let mut nonempty_input = raw_fixture();
        nonempty_input[52..56].copy_from_slice(&11u32.to_le_bytes());
        assert_eq!(
            SppDocumentRecords::decode(&nonempty_input),
            Err(SppDocumentError::NonEmptyType11(8))
        );

        let mut bad_bool = raw_fixture();
        let bool_offset = bad_bool
            .windows(8)
            .position(|part| part == b"Original")
            .unwrap()
            + 8;
        bad_bool[bool_offset] = 1;
        assert_eq!(
            SppDocumentRecords::decode(&bad_bool),
            Err(SppDocumentError::InvalidBoolean(1))
        );
    }

    #[test]
    fn count_caps_are_checked_before_allocating_record_vectors() {
        let mut list = fixture();
        list.classes[0].properties = vec![SppPropertyDefinition {
            name: "items".into(),
            type_code: 17,
        }];
        list.root.as_mut().unwrap().fields = vec![SppRecordValue::InlineObjects(Vec::new())];
        let mut bytes = list.encode().unwrap();
        let count_offset = bytes.len() - 4;
        bytes[count_offset..].copy_from_slice(&((MAX_COUNT as u32) + 1).to_le_bytes());
        assert_eq!(
            SppDocumentRecords::decode(&bytes),
            Err(SppDocumentError::CountLimit)
        );
    }

    #[test]
    fn opaque_16_byte_types_four_and_twenty_two_roundtrip_exactly() {
        let value4 = [0xA5; 16];
        let value22 = [0x5A; 16];
        let records = SppDocumentRecords {
            header: fixture().header,
            classes: vec![SppClassDefinition {
                name: "DataOpaque".into(),
                properties: vec![
                    SppPropertyDefinition {
                        name: "value4".into(),
                        type_code: 4,
                    },
                    SppPropertyDefinition {
                        name: "value22".into(),
                        type_code: 22,
                    },
                ],
            }],
            root: Some(SppObject {
                class_index: 0,
                fields: vec![
                    SppRecordValue::Opaque16(value4),
                    SppRecordValue::Opaque16(value22),
                ],
            }),
        };
        let encoded = records.encode().unwrap();
        assert_eq!(SppDocumentRecords::decode(&encoded).unwrap(), records);
        assert_eq!(
            encoded
                .windows(16)
                .filter(|window| window.iter().copied().eq(value4))
                .count(),
            1
        );
        assert_eq!(
            encoded
                .windows(16)
                .filter(|window| window.iter().copied().eq(value22))
                .count(),
            1
        );
    }

    #[test]
    fn encoding_rejects_duplicate_classes_properties_and_mismatched_fields() {
        let mut duplicate_property = fixture();
        duplicate_property.classes[0].properties[1].name = "name".into();
        assert_eq!(
            duplicate_property.encode(),
            Err(SppDocumentError::DuplicateProperty)
        );

        let mut duplicate_class = fixture();
        duplicate_class
            .classes
            .push(duplicate_class.classes[0].clone());
        assert_eq!(
            duplicate_class.encode(),
            Err(SppDocumentError::DuplicateClass)
        );

        let mut bad_fields = fixture();
        bad_fields.root.as_mut().unwrap().fields.pop();
        assert_eq!(bad_fields.encode(), Err(SppDocumentError::InvalidRecord));
    }
}
