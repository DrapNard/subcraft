//! Clean-room, lossless codec for the observed Painter scene 1.7 triangle subset.
//!
//! This is not standard Assimp 1.0. Controlled original OBJ fixtures establish the
//! field boundaries here. Node hierarchy and material/resource sections remain
//! opaque and are retained exactly; this module does not create an editor mesh
//! or establish coordinate transforms, UV orientation or material assignment.

use crate::spp_scene_material::{SppSceneMaterialError, SppSceneMaterials};
use crate::spp_scene_node::SppSceneNode;
use thiserror::Error;

pub const MAX_SCENE_BYTES: usize = 64 * 1024 * 1024;
const MAX_MESHES: usize = 4096;
const MAX_VERTICES: usize = 1_000_000;
const MAX_TRIANGLES: usize = 2_000_000;
const MAX_NAME_BYTES: usize = 64 * 1024;
const HEADER_BYTES: usize = 512;
const COMPONENTS: u32 = 0x107;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SppSceneError {
    #[error("scene material table: {0}")]
    Material(#[from] SppSceneMaterialError),
    #[error("malformed SPP scene: {0}")]
    Invalid(&'static str),
    #[error("unsupported SPP scene subset: {0}")]
    Unsupported(&'static str),
    #[error("SPP scene exceeds resource limit: {0}")]
    Limit(&'static str),
}
type Result<T> = std::result::Result<T, SppSceneError>;

#[derive(Clone, Debug, PartialEq)]
pub struct SppSceneMesh {
    pub name: String,
    /// Raw observed material index. Association with Painter texture sets is unproven.
    pub material_index: u32,
    /// Painter's stored coordinates, before any SubCraft conversion.
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub tangents: Vec<[f32; 3]>,
    pub bitangents: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SppScene {
    header: [u8; HEADER_BYTES],
    root_node: Vec<u8>,
    suffix: Vec<u8>,
    pub meshes: Vec<SppSceneMesh>,
}

impl SppScene {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_SCENE_BYTES {
            return Err(SppSceneError::Limit("encoded bytes"));
        }
        let mut input = Reader::new(bytes);
        let header: [u8; HEADER_BYTES] = input.take(HEADER_BYTES)?.try_into().unwrap();
        if !header.starts_with(b"ASSIMP.binary-dump.") {
            return Err(SppSceneError::Invalid("signature"));
        }
        if u32::from_le_bytes(header[44..48].try_into().unwrap()) != 1
            || u32::from_le_bytes(header[48..52].try_into().unwrap()) != 7
        {
            return Err(SppSceneError::Unsupported("only observed version 1.7"));
        }
        if u32::from_le_bytes(header[56..60].try_into().unwrap()) != 0x11
            || header[60..64] != [0, 0, 0, 0]
        {
            return Err(SppSceneError::Unsupported(
                "compile, shortened or compression flags",
            ));
        }
        let mut body = input.block()?;
        input.end()?;
        if body.u32()? != 8 {
            return Err(SppSceneError::Unsupported("scene flags"));
        }
        let root_node = body.raw_block()?.to_vec();
        let count = body.count(MAX_MESHES, 8)?;
        if count == 0 {
            return Err(SppSceneError::Invalid("no meshes"));
        }
        let mut meshes = Vec::with_capacity(count);
        let mut retained = 0usize;
        for _ in 0..count {
            let mut mesh = body.block()?;
            let name = mesh.text()?;
            if mesh.u32()? != 4 {
                return Err(SppSceneError::Unsupported("non-triangle primitive"));
            }
            let material_index = mesh.u32()?;
            if mesh.u32()? != COMPONENTS {
                return Err(SppSceneError::Unsupported("vertex component mask"));
            }
            let positions = mesh.vectors(&mut retained)?;
            let normals = mesh.vectors(&mut retained)?;
            let tangents = mesh.vectors(&mut retained)?;
            let bitangents = mesh.vectors(&mut retained)?;
            if mesh.u32()? != 2 {
                return Err(SppSceneError::Unsupported("UV component count"));
            }
            let uvs = mesh.vectors(&mut retained)?;
            let n = positions.len();
            if n == 0 || [normals.len(), tangents.len(), bitangents.len(), uvs.len()] != [n; 4] {
                return Err(SppSceneError::Invalid("vertex array lengths"));
            }
            let faces = mesh.count(MAX_TRIANGLES, 20)?;
            budget(&mut retained, faces, 12)?;
            if faces == 0 {
                return Err(SppSceneError::Invalid("no triangles"));
            }
            let mut triangles = Vec::with_capacity(faces);
            for _ in 0..faces {
                // The first word is an observed marker, not a guessed semantic flag.
                if mesh.u32()? != 1 || mesh.u32()? != 3 {
                    return Err(SppSceneError::Unsupported(
                        "triangle record marker or arity",
                    ));
                }
                let face = [mesh.u32()?, mesh.u32()?, mesh.u32()?];
                if face.iter().any(|&index| index as usize >= n) {
                    return Err(SppSceneError::Invalid("triangle index"));
                }
                triangles.push(face);
            }
            if mesh.u32()? != 0 {
                return Err(SppSceneError::Unsupported("mesh trailing section"));
            }
            mesh.end()?;
            meshes.push(SppSceneMesh {
                name,
                material_index,
                positions,
                normals,
                tangents,
                bitangents,
                uvs,
                triangles,
            });
        }
        // Preserve the entire remaining scene; do not infer materials/resources from names.
        if body.remaining().len() < 4 {
            return Err(SppSceneError::Invalid("missing scene suffix"));
        }
        SppSceneNode::decode(&root_node, meshes.len())?;
        let materials = SppSceneMaterials::decode(body.remaining())?;
        if meshes
            .iter()
            .any(|mesh| mesh.material_index as usize >= materials.materials.len())
        {
            return Err(SppSceneError::Invalid("mesh material reference"));
        }
        let suffix = body.remaining().to_vec();
        Ok(Self {
            header,
            root_node,
            suffix,
            meshes,
        })
    }

    /// Read the observed hierarchy and mesh references, retaining metadata without applying transforms.
    pub fn hierarchy(&self) -> Result<SppSceneNode> {
        SppSceneNode::decode(&self.root_node, self.meshes.len())
    }

    /// Validate hierarchy edits against the current mesh table before replacing retained bytes.
    pub fn set_hierarchy(&mut self, hierarchy: &SppSceneNode) -> Result<()> {
        let encoded = hierarchy.encode()?;
        SppSceneNode::decode(&encoded, self.meshes.len())?;
        self.root_node = encoded;
        Ok(())
    }

    /// Decode material records in their serialized order, preserving unknown property payloads.
    pub fn materials(&self) -> Result<SppSceneMaterials> {
        Ok(SppSceneMaterials::decode(&self.suffix)?)
    }

    pub fn set_materials(&mut self, materials: &SppSceneMaterials) -> Result<()> {
        if self
            .meshes
            .iter()
            .any(|mesh| mesh.material_index as usize >= materials.materials.len())
        {
            return Err(SppSceneError::Invalid("mesh material reference"));
        }
        let bytes = materials.encode()?;
        SppSceneMaterials::decode(&bytes)?;
        self.suffix = bytes;
        Ok(())
    }

    /// Encode understood geometry and retain every other byte. Does not write a container.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.meshes.is_empty() || self.meshes.len() > MAX_MESHES {
            return Err(SppSceneError::Limit("mesh count"));
        }
        let mut out = Writer(Vec::new());
        out.bytes(&self.header)?;
        let body = out.begin()?;
        out.u32(8)?;
        out.bytes(&self.root_node)?;
        out.u32(self.meshes.len() as u32)?;
        for mesh in &self.meshes {
            let start = out.begin()?;
            out.text(&mesh.name)?;
            out.u32(4)?;
            out.u32(mesh.material_index)?;
            out.u32(COMPONENTS)?;
            for values in [
                &mesh.positions,
                &mesh.normals,
                &mesh.tangents,
                &mesh.bitangents,
            ] {
                out.vectors(values)?;
            }
            out.u32(2)?;
            out.vectors(&mesh.uvs)?;
            if mesh.triangles.len() > MAX_TRIANGLES {
                return Err(SppSceneError::Limit("triangle count"));
            }
            out.u32(mesh.triangles.len() as u32)?;
            for face in &mesh.triangles {
                out.u32(1)?;
                out.u32(3)?;
                for &index in face {
                    out.u32(index)?;
                }
            }
            out.u32(0)?;
            out.finish(start);
        }
        out.bytes(&self.suffix)?;
        out.finish(body);
        // Apply all the same aggregate, finite-value and reference checks to edits.
        Self::decode(&out.0)?;
        Ok(out.0)
    }
}

fn budget(retained: &mut usize, count: usize, width: usize) -> Result<()> {
    *retained = retained
        .checked_add(
            count
                .checked_mul(width)
                .ok_or(SppSceneError::Limit("array size"))?,
        )
        .ok_or(SppSceneError::Limit("aggregate arrays"))?;
    if *retained > MAX_SCENE_BYTES {
        return Err(SppSceneError::Limit("aggregate arrays"));
    }
    Ok(())
}
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&v| v <= self.bytes.len())
            .ok_or(SppSceneError::Invalid("truncated section"))?;
        let bytes = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn raw_block(&mut self) -> Result<&'a [u8]> {
        let start = self.pos;
        let len = u64::from_le_bytes(self.take(8)?.try_into().unwrap());
        let len = usize::try_from(len).map_err(|_| SppSceneError::Limit("section length"))?;
        if len < 8 {
            return Err(SppSceneError::Invalid("section length"));
        }
        self.take(len - 8)?;
        Ok(&self.bytes[start..self.pos])
    }
    fn block(&mut self) -> Result<Reader<'a>> {
        Ok(Reader::new(&self.raw_block()?[8..]))
    }
    fn count(&mut self, max: usize, width: usize) -> Result<usize> {
        let n = self.u32()? as usize;
        if n > max {
            return Err(SppSceneError::Limit("array count"));
        }
        if n > self.remaining().len() / width {
            return Err(SppSceneError::Invalid("array exceeds section"));
        }
        Ok(n)
    }
    fn text(&mut self) -> Result<String> {
        let n = self.count(MAX_NAME_BYTES, 1)?;
        Ok(std::str::from_utf8(self.take(n)?)
            .map_err(|_| SppSceneError::Invalid("UTF-8 name"))?
            .to_owned())
    }
    fn vectors(&mut self, retained: &mut usize) -> Result<Vec<[f32; 3]>> {
        let count = self.count(MAX_VERTICES, 12)?;
        budget(retained, count, 12)?;
        let bytes = self.take(count * 12)?;
        let mut out = Vec::with_capacity(count);
        for xyz in bytes.chunks_exact(12) {
            let value = std::array::from_fn(|i| {
                f32::from_le_bytes(xyz[i * 4..i * 4 + 4].try_into().unwrap())
            });
            if value.iter().any(|v| !v.is_finite()) {
                return Err(SppSceneError::Invalid("nonfinite vector"));
            }
            out.push(value);
        }
        Ok(out)
    }
    fn end(self) -> Result<()> {
        if self.pos != self.bytes.len() {
            return Err(SppSceneError::Invalid("trailing section bytes"));
        }
        Ok(())
    }
}
struct Writer(Vec<u8>);
impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_SCENE_BYTES.saturating_sub(self.0.len()) {
            return Err(SppSceneError::Limit("encoded bytes"));
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn u32(&mut self, value: u32) -> Result<()> {
        self.bytes(&value.to_le_bytes())
    }
    fn begin(&mut self) -> Result<usize> {
        let pos = self.0.len();
        self.bytes(&[0; 8])?;
        Ok(pos)
    }
    fn finish(&mut self, start: usize) {
        let len = (self.0.len() - start) as u64;
        self.0[start..start + 8].copy_from_slice(&len.to_le_bytes());
    }
    fn text(&mut self, text: &str) -> Result<()> {
        if text.len() > MAX_NAME_BYTES {
            return Err(SppSceneError::Limit("name bytes"));
        }
        self.u32(text.len() as u32)?;
        self.bytes(text.as_bytes())
    }
    fn vectors(&mut self, values: &[[f32; 3]]) -> Result<()> {
        if values.len() > MAX_VERTICES {
            return Err(SppSceneError::Limit("vertex count"));
        }
        self.u32(values.len() as u32)?;
        for vector in values {
            for value in vector {
                self.bytes(&value.to_le_bytes())?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scene() -> SppScene {
        let mut header = [0; HEADER_BYTES];
        header[..19].copy_from_slice(b"ASSIMP.binary-dump.");
        header[44..48].copy_from_slice(&1_u32.to_le_bytes());
        header[48..52].copy_from_slice(&7_u32.to_le_bytes());
        header[56..60].copy_from_slice(&0x11_u32.to_le_bytes());
        header[448] = 97; // unknown header byte must survive
        let root_node = SppSceneNode {
            name: "Root".into(),
            matrix_bits: std::array::from_fn(|i| if i % 5 == 0 { 1_f32.to_bits() } else { 0 }),
            mesh_indices: vec![0],
            children: vec![],
            opaque_suffix: vec![43; 4],
        }
        .encode()
        .unwrap();
        SppScene {
            header,
            root_node,
            suffix: SppSceneMaterials {
                materials: (0..3)
                    .map(|i| {
                        let name = format!("Material {i}");
                        let mut value = (name.len() as u32).to_le_bytes().to_vec();
                        value.extend_from_slice(name.as_bytes());
                        value.push(0);
                        crate::spp_scene_material::SppSceneMaterial {
                            properties: vec![crate::spp_scene_material::SppSceneMaterialProperty {
                                key: b"?mat.name".to_vec(),
                                semantic: 0,
                                index: 0,
                                type_code: 3,
                                value,
                                trailing: vec![],
                            }],
                            trailing: vec![97],
                        }
                    })
                    .collect(),
                opaque_tail: vec![79, 0, 0, 0, 93],
            }
            .encode()
            .unwrap(),
            meshes: vec![SppSceneMesh {
                name: "Original triangle".into(),
                material_index: 2,
                positions: vec![[0., 0., -0.], [2., 0., 0.], [0., 3., 0.]],
                normals: vec![[0., 0., 1.]; 3],
                tangents: vec![[1., 0., 0.]; 3],
                bitangents: vec![[0., 1., 0.]; 3],
                uvs: vec![[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
                triangles: vec![[0, 1, 2]],
            }],
        }
    }
    #[test]
    fn reference_mesh_preserves_geometry_objects_material_mapping_and_flips_uv() {
        let original = scene();
        let converted = crate::spp_scene_mesh::build_reference_mesh(&original).unwrap();
        assert!(converted.mesh.validate().is_empty());
        assert_eq!(converted.mesh.triangle_count(), 1);
        assert_eq!(converted.mesh.materials[0].name, "Material 2");
        assert_eq!(
            converted.material_slot_by_source_index,
            [None, None, Some(0)]
        );
        assert_eq!(converted.mesh.objects[0].name, "Root");
        assert_eq!(converted.object_index_by_source_mesh, [0]);
        for i in 0..3 {
            let vertex = converted.mesh.indices[i] as usize;
            assert_eq!(
                converted.mesh.positions[vertex].to_array(),
                original.meshes[0].positions[i]
            );
            let uv = original.meshes[0].uvs[i];
            assert_eq!(converted.mesh.uvs[vertex].to_array(), [uv[0], 1. - uv[1]]);
        }
    }
    #[test]
    fn reference_conversion_rejects_geometry_loss_and_unverified_transforms() {
        use crate::spp_scene_mesh::build_reference_mesh;
        let mut value = scene();
        value.meshes[0].normals[0] = [0.; 3];
        assert!(build_reference_mesh(&value).is_err());
        let mut value = scene();
        value.meshes[0].uvs[0][2] = 1.;
        assert!(build_reference_mesh(&value).is_err());
        let mut value = scene();
        value.meshes[0].triangles[0] = [0, 0, 1];
        assert!(build_reference_mesh(&value).is_err());
        let mut value = scene();
        let mut node = value.hierarchy().unwrap();
        node.matrix_bits[0] = 2_f32.to_bits();
        value.set_hierarchy(&node).unwrap();
        assert!(build_reference_mesh(&value).is_err());
        let mut value = scene();
        let mut node = value.hierarchy().unwrap();
        let child = node.clone();
        node.mesh_indices.clear();
        node.children = vec![child.clone(), child];
        value.set_hierarchy(&node).unwrap();
        assert!(build_reference_mesh(&value).is_err());
        let mut value = scene();
        value.meshes[0].triangles = vec![[0, 1, 2]; 250_001];
        assert!(build_reference_mesh(&value).is_err());
    }
    #[test]
    fn exact_roundtrip_preserves_unknown_sections_and_float_bits() {
        let scene = scene();
        let bytes = scene.encode().unwrap();
        let decoded = SppScene::decode(&bytes).unwrap();
        assert_eq!(decoded, scene);
        assert_eq!(decoded.encode().unwrap(), bytes);
        assert_eq!(
            decoded.meshes[0].positions[0][2].to_bits(),
            (-0_f32).to_bits()
        );
    }
    #[test]
    fn every_truncation_and_overlong_body_fails() {
        let bytes = scene().encode().unwrap();
        for n in 0..bytes.len() {
            assert!(SppScene::decode(&bytes[..n]).is_err(), "length {n}");
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(SppScene::decode(&trailing).is_err());
        for len in [0_u64, 7, u64::MAX] {
            let mut bad = bytes.clone();
            bad[512..520].copy_from_slice(&len.to_le_bytes());
            assert!(SppScene::decode(&bad).is_err());
        }
    }
    #[test]
    fn unsupported_versions_compression_and_components_fail() {
        let bytes = scene().encode().unwrap();
        for (offset, value) in [(48, 0), (56, 0), (60, 1), (62, 1)] {
            let mut bad = bytes.clone();
            bad[offset] = value;
            assert!(SppScene::decode(&bad).is_err());
        }
        let mut bad = bytes;
        let mesh = 524 + scene().root_node.len() + 4;
        let mask = mesh + 8 + 4 + "Original triangle".len() + 8;
        bad[mask..mask + 4].copy_from_slice(&0x103_u32.to_le_bytes());
        assert!(SppScene::decode(&bad).is_err());
    }
    #[test]
    fn edits_validate_arrays_finiteness_indices_and_limits() {
        let mut value = scene();
        value.meshes[0].positions[0][0] = f32::NAN;
        assert!(value.encode().is_err());
        let mut value = scene();
        value.meshes[0].uvs.pop();
        assert!(value.encode().is_err());
        let mut value = scene();
        value.meshes[0].triangles[0][2] = 3;
        assert!(value.encode().is_err());
        let mut value = scene();
        value.meshes[0].triangles.clear();
        assert!(value.encode().is_err());
        let mut value = scene();
        value.meshes[0].name = "x".repeat(MAX_NAME_BYTES + 1);
        assert!(value.encode().is_err());
        let mut budgeted = MAX_SCENE_BYTES;
        assert!(budget(&mut budgeted, 1, 12).is_err());
        assert!(budget(&mut 0, usize::MAX, 12).is_err());
    }
    #[test]
    fn edited_mesh_and_node_references_cannot_dangle() {
        let mut value = scene();
        value.meshes[0].material_index = 3;
        assert!(matches!(
            value.encode(),
            Err(SppSceneError::Invalid("mesh material reference"))
        ));
        let mut value = scene();
        let mut hierarchy = value.hierarchy().unwrap();
        hierarchy.mesh_indices = vec![1];
        assert!(value.set_hierarchy(&hierarchy).is_err());
        let mut value = scene();
        let mut materials = value.materials().unwrap();
        materials.materials.clear();
        assert!(value.set_materials(&materials).is_err());
    }
    #[test]
    fn counts_are_checked_before_allocating() {
        let mut bytes = scene().encode().unwrap();
        let mesh = 524 + scene().root_node.len() + 4;
        let count = mesh + 8 + 4 + "Original triangle".len() + 12;
        bytes[count..count + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            SppScene::decode(&bytes),
            Err(SppSceneError::Limit(_))
        ));
    }
    #[test]
    fn deterministic_mutations_never_panic_or_silently_rewrite_accepted_bytes() {
        let source = scene().encode().unwrap();
        let mut seed = 0x7e31_a9d5_u32;
        for _ in 0..2000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let index = seed as usize % source.len();
            let mut mutated = source.clone();
            mutated[index] ^= 1 << ((seed >> 16) % 8);
            if let Ok(decoded) = SppScene::decode(&mutated) {
                assert_eq!(decoded.encode().unwrap(), mutated);
            }
        }
    }
    #[test]
    fn understood_geometry_edits_leave_opaque_sections_unchanged() {
        let source = scene();
        let mut edit = SppScene::decode(&source.encode().unwrap()).unwrap();
        edit.meshes[0].positions[1][0] = 1.125;
        edit.meshes[0].uvs[0][0] = 0.875;
        let returned = SppScene::decode(&edit.encode().unwrap()).unwrap();
        assert_eq!(returned.meshes[0].positions[1][0], 1.125);
        assert_eq!(returned.meshes[0].uvs[0][0], 0.875);
        assert_eq!(returned.header, source.header);
        assert_eq!(returned.root_node, source.root_node);
        assert_eq!(returned.suffix, source.suffix);
    }
}
