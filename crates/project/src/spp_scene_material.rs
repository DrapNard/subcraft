//! Bounded, lossless codec for the observed Painter scene 1.7 material-list suffix.
//!
//! Material properties remain opaque byte payloads. Only the observed `?mat.name`
//! property spelling, type 3, semantic/index zero, and nested length-prefixed
//! UTF-8-with-NUL value are interpreted. This does not map materials to texture
//! sets or establish a general Painter/Assimp material schema.

use thiserror::Error;

const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_MATERIALS: usize = 4096;
const MAX_PROPERTIES: usize = 65_536;
const MAX_KEY_BYTES: usize = 64 * 1024;
const MATERIAL_MIN_BYTES: usize = 12;
const PROPERTY_MIN_BYTES: usize = 28;
const NAME_KEY: &[u8] = b"?mat.name";

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SppSceneMaterialError {
    #[error("malformed scene material suffix: {0}")]
    Invalid(&'static str),
    #[error("scene material suffix exceeds limit: {0}")]
    Limit(&'static str),
    #[error("material has multiple name properties")]
    AmbiguousName,
    #[error("material name uses an unrecognized encoding")]
    UnsupportedName,
}
type Result<T> = std::result::Result<T, SppSceneMaterialError>;

/// One raw scene material property. Values and any unknown per-property tail are preserved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppSceneMaterialProperty {
    pub key: Vec<u8>,
    pub semantic: u32,
    pub index: u32,
    pub type_code: u32,
    pub value: Vec<u8>,
    pub trailing: Vec<u8>,
}

impl SppSceneMaterialProperty {
    /// Interpret only the controlled `?mat.name` string schema; other properties stay opaque.
    pub fn name_value(&self) -> Result<Option<&str>> {
        if self.key != NAME_KEY {
            return Ok(None);
        }
        if self.semantic != 0 || self.index != 0 || self.type_code != 3 || !self.trailing.is_empty()
        {
            return Err(SppSceneMaterialError::UnsupportedName);
        }
        if self.value.len() < 5 || *self.value.last().unwrap() != 0 {
            return Err(SppSceneMaterialError::UnsupportedName);
        }
        let byte_len = u32::from_le_bytes(self.value[..4].try_into().unwrap()) as usize;
        if byte_len.checked_add(5) != Some(self.value.len()) {
            return Err(SppSceneMaterialError::UnsupportedName);
        }
        let text = std::str::from_utf8(&self.value[4..self.value.len() - 1])
            .map_err(|_| SppSceneMaterialError::UnsupportedName)?;
        if text.as_bytes().contains(&0) {
            return Err(SppSceneMaterialError::UnsupportedName);
        }
        Ok(Some(text))
    }
}

/// One material, in the suffix's source order, including unknown trailing bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppSceneMaterial {
    pub properties: Vec<SppSceneMaterialProperty>,
    pub trailing: Vec<u8>,
}

impl SppSceneMaterial {
    /// Return the unique observed name property, or `None` if no name is present.
    pub fn name(&self) -> Result<Option<&str>> {
        let mut name = None;
        for property in &self.properties {
            if property.key == NAME_KEY {
                if name.is_some() {
                    return Err(SppSceneMaterialError::AmbiguousName);
                }
                name = property.name_value()?;
            }
        }
        Ok(name)
    }
}

/// Lossless material table plus the opaque scene bytes following it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppSceneMaterials {
    pub materials: Vec<SppSceneMaterial>,
    pub opaque_tail: Vec<u8>,
}

impl SppSceneMaterials {
    /// Decode the material list from the bytes following the scene's mesh records.
    pub fn decode(suffix: &[u8]) -> Result<Self> {
        if suffix.len() > MAX_BYTES {
            return Err(SppSceneMaterialError::Limit("suffix bytes"));
        }
        let mut input = Reader::new(suffix);
        let count = input.count(MAX_MATERIALS, MATERIAL_MIN_BYTES)?;
        let mut materials = Vec::with_capacity(count);
        let mut total_properties = 0usize;
        for _ in 0..count {
            let mut material = input.block(MATERIAL_MIN_BYTES)?;
            let property_count = material.count(MAX_PROPERTIES, PROPERTY_MIN_BYTES)?;
            total_properties = total_properties
                .checked_add(property_count)
                .filter(|&total| total <= MAX_PROPERTIES)
                .ok_or(SppSceneMaterialError::Limit("property count"))?;
            let mut properties = Vec::with_capacity(property_count);
            for _ in 0..property_count {
                let mut property = material.block(PROPERTY_MIN_BYTES)?;
                let key_len = property.count(MAX_KEY_BYTES, 1)?;
                let key = property.take(key_len)?.to_vec();
                let semantic = property.u32()?;
                let index = property.u32()?;
                let type_code = property.u32()?;
                let value_len = property.count(MAX_BYTES, 1)?;
                let value = property.take(value_len)?.to_vec();
                let trailing = property.remaining().to_vec();
                properties.push(SppSceneMaterialProperty {
                    key,
                    semantic,
                    index,
                    type_code,
                    value,
                    trailing,
                });
            }
            let trailing = material.remaining().to_vec();
            materials.push(SppSceneMaterial {
                properties,
                trailing,
            });
        }
        let opaque_tail = input.remaining().to_vec();
        Ok(Self {
            materials,
            opaque_tail,
        })
    }

    /// Encode all material records and opaque bytes in their original ordering.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.materials.len() > MAX_MATERIALS {
            return Err(SppSceneMaterialError::Limit("material count"));
        }
        let mut total_properties = 0usize;
        for material in &self.materials {
            total_properties = total_properties
                .checked_add(material.properties.len())
                .filter(|&total| total <= MAX_PROPERTIES)
                .ok_or(SppSceneMaterialError::Limit("property count"))?;
            for property in &material.properties {
                if property.key.len() > MAX_KEY_BYTES || property.key.len() > u32::MAX as usize {
                    return Err(SppSceneMaterialError::Limit("property key"));
                }
                if property.value.len() > MAX_BYTES || property.value.len() > u32::MAX as usize {
                    return Err(SppSceneMaterialError::Limit("property value"));
                }
            }
        }
        let mut output = Writer::default();
        output.u32(self.materials.len() as u32)?;
        for material in &self.materials {
            let start = output.begin()?;
            output.u32(material.properties.len() as u32)?;
            for property in &material.properties {
                let property_start = output.begin()?;
                output.u32(property.key.len() as u32)?;
                output.bytes(&property.key)?;
                output.u32(property.semantic)?;
                output.u32(property.index)?;
                output.u32(property.type_code)?;
                output.u32(property.value.len() as u32)?;
                output.bytes(&property.value)?;
                output.bytes(&property.trailing)?;
                output.finish(property_start)?;
            }
            output.bytes(&material.trailing)?;
            output.finish(start)?;
        }
        output.bytes(&self.opaque_tail)?;
        Ok(output.0)
    }

    /// Return names where the uniquely observed name property is present and valid.
    pub fn names(&self) -> Result<Vec<Option<&str>>> {
        self.materials.iter().map(SppSceneMaterial::name).collect()
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.position..]
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(len)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(SppSceneMaterialError::Invalid("truncated record"))?;
        let bytes = &self.bytes[self.position..end];
        self.position = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn count(&mut self, max: usize, min_width: usize) -> Result<usize> {
        let count = self.u32()? as usize;
        if count > max {
            return Err(SppSceneMaterialError::Limit("record count"));
        }
        if min_width > 0 && count > self.remaining().len() / min_width {
            return Err(SppSceneMaterialError::Invalid(
                "count exceeds enclosing block",
            ));
        }
        Ok(count)
    }
    fn block(&mut self, minimum: usize) -> Result<Reader<'a>> {
        let length = usize::try_from(self.u64()?)
            .map_err(|_| SppSceneMaterialError::Limit("block length"))?;
        if length < minimum || length < 8 {
            return Err(SppSceneMaterialError::Invalid("block length"));
        }
        Ok(Reader::new(self.take(length - 8)?))
    }
}

#[derive(Default)]
struct Writer(Vec<u8>);
impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(SppSceneMaterialError::Limit("encoded suffix"));
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn u32(&mut self, value: u32) -> Result<()> {
        self.bytes(&value.to_le_bytes())
    }
    fn begin(&mut self) -> Result<usize> {
        let start = self.0.len();
        self.bytes(&[0; 8])?;
        Ok(start)
    }
    fn finish(&mut self, start: usize) -> Result<()> {
        let length = self
            .0
            .len()
            .checked_sub(start)
            .ok_or(SppSceneMaterialError::Invalid("block start"))?;
        let length =
            u64::try_from(length).map_err(|_| SppSceneMaterialError::Limit("block length"))?;
        self.0[start..start + 8].copy_from_slice(&length.to_le_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn string_property(key: &[u8], text: &str) -> SppSceneMaterialProperty {
        let mut value = Vec::new();
        value.extend_from_slice(&(text.len() as u32).to_le_bytes());
        value.extend_from_slice(text.as_bytes());
        value.push(0);
        SppSceneMaterialProperty {
            key: key.into(),
            semantic: 0,
            index: 0,
            type_code: 3,
            value,
            trailing: Vec::new(),
        }
    }

    fn table() -> SppSceneMaterials {
        SppSceneMaterials {
            materials: vec![SppSceneMaterial {
                properties: vec![
                    string_property(NAME_KEY, "RedSlot"),
                    SppSceneMaterialProperty {
                        key: b"$future.payload".to_vec(),
                        semantic: 17,
                        index: 2,
                        type_code: 0xf00d,
                        value: vec![0, 1, 0xff, 7],
                        trailing: vec![0xee],
                    },
                ],
                trailing: vec![0xa1, 0xb2],
            }],
            opaque_tail: vec![9, 8, 7, 6],
        }
    }

    #[test]
    fn records_names_and_unknown_bytes_roundtrip_exactly() {
        let original = table();
        let bytes = original.encode().unwrap();
        let decoded = SppSceneMaterials::decode(&bytes).unwrap();
        assert_eq!(decoded, original);
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert_eq!(decoded.names().unwrap(), [Some("RedSlot")]);
        assert_eq!(decoded.opaque_tail, [9, 8, 7, 6]);
    }

    #[test]
    fn name_interpretation_rejects_ambiguous_or_unverified_schema() {
        let mut value = table();
        value.materials[0]
            .properties
            .push(string_property(NAME_KEY, "Second"));
        assert_eq!(
            value.materials[0].name(),
            Err(SppSceneMaterialError::AmbiguousName)
        );

        let mut value = table();
        value.materials[0].properties[0].type_code = 4;
        assert_eq!(
            value.materials[0].name(),
            Err(SppSceneMaterialError::UnsupportedName)
        );

        let mut value = table();
        value.materials[0].properties[0].value[0] = 0xff;
        assert_eq!(
            value.materials[0].name(),
            Err(SppSceneMaterialError::UnsupportedName)
        );
    }

    #[test]
    fn truncation_and_unbounded_counts_fail_before_allocation() {
        let mut strict = table();
        strict.materials[0].trailing.clear();
        strict.materials[0].properties[1].trailing.clear();
        strict.opaque_tail.clear();
        let bytes = strict.encode().unwrap();
        for end in 0..bytes.len() {
            assert!(SppSceneMaterials::decode(&bytes[..end]).is_err());
        }
        let mut count_overflow = Vec::new();
        count_overflow.extend_from_slice(&((MAX_MATERIALS as u32) + 1).to_le_bytes());
        assert!(matches!(
            SppSceneMaterials::decode(&count_overflow),
            Err(SppSceneMaterialError::Limit(_))
        ));
        assert!(matches!(
            SppSceneMaterials::decode(&vec![0; MAX_BYTES + 1]),
            Err(SppSceneMaterialError::Limit(_))
        ));
    }
}
