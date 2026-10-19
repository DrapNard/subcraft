//! Versioned binary encoding of the canonical mesh (stored inside project files).
//!
//! Layout (little endian): magic `SCMESH`, u16 version, then length-prefixed sections.
//! The format is a product contract: bump `VERSION` and keep a reader for old versions.

use glam::{Vec2, Vec3, Vec4};

use crate::mesh::{Aabb, MaterialSlot, Mesh, MeshObject, Submesh};

const MAGIC: &[u8; 6] = b"SCMESH";
const VERSION: u16 = 2;
const MAX_INPUT_BYTES: usize = 256 * 1024 * 1024;
const MAX_VERTICES: usize = 4_000_000;
const MAX_INDICES: usize = 12_000_000;
const MAX_METADATA_ITEMS: usize = 65_536;
const MAX_STRING_BYTES: usize = 65_536;

fn invalid(message: &str) -> MeshDecodeError {
    MeshDecodeError::Invalid(message.into())
}

#[derive(Debug, thiserror::Error)]
pub enum MeshDecodeError {
    #[error("not a SubCraft mesh blob")]
    BadMagic,
    #[error("unsupported mesh blob version {0}")]
    Version(u16),
    #[error("truncated mesh blob")]
    Truncated,
    #[error("invalid mesh: {0}")]
    Invalid(String),
}

struct W(Vec<u8>);
impl W {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32s(&mut self, v: &[f32]) {
        self.u32(v.len() as u32);
        for x in v {
            self.0.extend_from_slice(&x.to_le_bytes());
        }
    }
    fn u32s(&mut self, v: &[u32]) {
        self.u32(v.len() as u32);
        for x in v {
            self.0.extend_from_slice(&x.to_le_bytes());
        }
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct R<'a>(&'a [u8]);
impl R<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], MeshDecodeError> {
        if self.0.len() < n {
            return Err(MeshDecodeError::Truncated);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u32(&mut self) -> Result<u32, MeshDecodeError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    fn f32s(&mut self) -> Result<Vec<f32>, MeshDecodeError> {
        let n = self.u32()? as usize;
        let b = self.take(n.checked_mul(4).ok_or(MeshDecodeError::Truncated)?)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(n)
            .map_err(|_| invalid("cannot allocate float section"))?;
        values.extend(
            b.chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().expect("4"))),
        );
        Ok(values)
    }
    fn u32s(&mut self) -> Result<Vec<u32>, MeshDecodeError> {
        let n = self.u32()? as usize;
        let b = self.take(n.checked_mul(4).ok_or(MeshDecodeError::Truncated)?)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(n)
            .map_err(|_| invalid("cannot allocate index section"))?;
        values.extend(
            b.chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().expect("4"))),
        );
        Ok(values)
    }
    fn str(&mut self) -> Result<String, MeshDecodeError> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec())
            .map_err(|e| MeshDecodeError::Invalid(e.to_string()))
    }
}

pub fn encode(mesh: &Mesh) -> Vec<u8> {
    let mut w = W(Vec::with_capacity(
        mesh.positions.len() * 64 + mesh.indices.len() * 4 + 256,
    ));
    w.0.extend_from_slice(MAGIC);
    w.0.extend_from_slice(&VERSION.to_le_bytes());
    w.str(&mesh.name);
    w.u32(mesh.materials.len() as u32);
    for m in &mesh.materials {
        w.str(&m.name);
    }
    w.f32s(bytemuck::cast_slice(
        &mesh
            .positions
            .iter()
            .map(|p| p.to_array())
            .collect::<Vec<_>>(),
    ));
    w.f32s(bytemuck::cast_slice(
        &mesh
            .normals
            .iter()
            .map(|p| p.to_array())
            .collect::<Vec<_>>(),
    ));
    w.f32s(bytemuck::cast_slice(
        &mesh.uvs.iter().map(|p| p.to_array()).collect::<Vec<_>>(),
    ));
    w.f32s(bytemuck::cast_slice(
        &mesh
            .tangents
            .iter()
            .map(|p| p.to_array())
            .collect::<Vec<_>>(),
    ));
    w.u32s(&mesh.indices);
    w.u32s(&mesh.tri_material);
    w.u32(mesh.submeshes.len() as u32);
    for s in &mesh.submeshes {
        w.u32(s.material);
        w.u32(s.first_triangle);
        w.u32(s.triangle_count);
    }
    w.u32(mesh.objects.len() as u32);
    for object in &mesh.objects {
        w.0.extend_from_slice(object.id.0.as_bytes());
        w.str(&object.name);
    }
    w.u32s(
        &mesh
            .tri_object
            .iter()
            .map(|o| o.unwrap_or(u32::MAX))
            .collect::<Vec<_>>(),
    );
    w.0
}

// Check the entire wire layout without constructing any mesh vectors. Counts
// must have matching bytes before they are allowed to control allocations.
fn preflight(bytes: &[u8]) -> Result<(), MeshDecodeError> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(invalid("mesh blob exceeds 256 MiB"));
    }
    let mut r = R(bytes);
    if r.take(6)? != MAGIC {
        return Err(MeshDecodeError::BadMagic);
    }
    let version = u16::from_le_bytes(r.take(2)?.try_into().expect("2 bytes"));
    if version != 1 && version != VERSION {
        return Err(MeshDecodeError::Version(version));
    }
    fn string(r: &mut R<'_>) -> Result<(), MeshDecodeError> {
        let count = r.u32()? as usize;
        if count > MAX_STRING_BYTES {
            return Err(invalid("mesh string exceeds 64 KiB"));
        }
        std::str::from_utf8(r.take(count)?).map_err(|_| invalid("mesh string is not UTF-8"))?;
        Ok(())
    }
    fn section(
        r: &mut R<'_>,
        width: usize,
        limit: usize,
        floats: bool,
    ) -> Result<(), MeshDecodeError> {
        let count = r.u32()? as usize;
        if !count.is_multiple_of(width) || count / width > limit {
            return Err(invalid("invalid mesh section count"));
        }
        let raw = r.take(
            count
                .checked_mul(4)
                .ok_or_else(|| invalid("mesh section size overflow"))?,
        )?;
        if floats
            && raw
                .chunks_exact(4)
                .any(|c| !f32::from_le_bytes(c.try_into().expect("4 bytes")).is_finite())
        {
            return Err(invalid("non-finite mesh attribute"));
        }
        Ok(())
    }
    string(&mut r)?;
    let materials = r.u32()? as usize;
    if materials > MAX_METADATA_ITEMS || materials > r.0.len() / 4 {
        return Err(invalid("material count exceeds limits"));
    }
    for _ in 0..materials {
        string(&mut r)?;
    }
    for width in [3, 3, 2, 4] {
        section(&mut r, width, MAX_VERTICES, true)?;
    }
    section(&mut r, 3, MAX_INDICES / 3, false)?;
    section(&mut r, 1, MAX_INDICES / 3, false)?;
    let submeshes = r.u32()? as usize;
    if submeshes > MAX_METADATA_ITEMS {
        return Err(invalid("submesh count exceeds limits"));
    }
    r.take(
        submeshes
            .checked_mul(12)
            .ok_or_else(|| invalid("submesh size overflow"))?,
    )?;
    if version >= 2 {
        let objects = r.u32()? as usize;
        if objects > MAX_METADATA_ITEMS || objects > r.0.len() / 20 {
            return Err(invalid("object count exceeds limits"));
        }
        for _ in 0..objects {
            r.take(16)?;
            string(&mut r)?;
        }
        section(&mut r, 1, MAX_INDICES / 3, false)?;
    }
    if !r.0.is_empty() {
        return Err(invalid("trailing mesh bytes"));
    }
    Ok(())
}

pub fn decode(bytes: &[u8]) -> Result<Mesh, MeshDecodeError> {
    preflight(bytes)?;
    let mut r = R(bytes);
    if r.take(6)? != MAGIC {
        return Err(MeshDecodeError::BadMagic);
    }
    let version = u16::from_le_bytes(r.take(2)?.try_into().expect("2"));
    if version != 1 && version != VERSION {
        return Err(MeshDecodeError::Version(version));
    }
    let name = r.str()?;
    let n_mat = r.u32()? as usize;
    let mut materials = Vec::new();
    materials
        .try_reserve_exact(n_mat)
        .map_err(|_| invalid("cannot allocate material table"))?;
    for _ in 0..n_mat {
        materials.push(MaterialSlot { name: r.str()? });
    }
    let v3 = |v: Vec<f32>| {
        v.chunks_exact(3)
            .map(|c| Vec3::new(c[0], c[1], c[2]))
            .collect::<Vec<_>>()
    };
    let positions = v3(r.f32s()?);
    let normals = v3(r.f32s()?);
    let uvs: Vec<Vec2> = r
        .f32s()?
        .chunks_exact(2)
        .map(|c| Vec2::new(c[0], c[1]))
        .collect();
    let tangents: Vec<Vec4> = r
        .f32s()?
        .chunks_exact(4)
        .map(|c| Vec4::new(c[0], c[1], c[2], c[3]))
        .collect();
    let indices = r.u32s()?;
    let tri_material = r.u32s()?;
    let n_sub = r.u32()? as usize;
    let mut submeshes = Vec::new();
    submeshes
        .try_reserve_exact(n_sub)
        .map_err(|_| invalid("cannot allocate submesh table"))?;
    for _ in 0..n_sub {
        submeshes.push(Submesh {
            material: r.u32()?,
            first_triangle: r.u32()?,
            triangle_count: r.u32()?,
        });
    }
    let bounds = Aabb::from_points(positions.iter().copied());
    let mut objects = Vec::new();
    let mut tri_object = Vec::new();
    if version >= 2 {
        let count = r.u32()?;
        objects
            .try_reserve_exact(count as usize)
            .map_err(|_| invalid("cannot allocate object table"))?;
        for _ in 0..count {
            let id = subcraft_core::ObjectId(uuid::Uuid::from_bytes(
                r.take(16)?.try_into().expect("16 bytes"),
            ));
            objects.push(MeshObject { id, name: r.str()? });
        }
        tri_object = r
            .u32s()?
            .into_iter()
            .map(|o| (o != u32::MAX).then_some(o))
            .collect();
    }
    let mesh = Mesh {
        name,
        positions,
        normals,
        uvs,
        tangents,
        indices,
        tri_material,
        tri_object,
        objects,
        materials,
        submeshes,
        bounds,
    };
    let problems = mesh.validate();
    if !problems.is_empty() {
        return Err(MeshDecodeError::Invalid(problems.join("; ")));
    }
    Ok(mesh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    fn geometry_offset(bytes: &[u8]) -> usize {
        let mut r = R(bytes);
        r.take(8).unwrap();
        r.str().unwrap();
        let materials = r.u32().unwrap();
        for _ in 0..materials {
            r.str().unwrap();
        }
        bytes.len() - r.0.len()
    }

    #[test]
    fn rejects_residual_float_components_and_trailing_bytes() {
        let bytes = encode(&primitives::cube(1.0));
        let mut at = geometry_offset(&bytes);
        for width in [3, 3, 2, 4] {
            let count = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
            assert_eq!(count % width, 0);
            let mut malformed = bytes.clone();
            malformed[at..at + 4].copy_from_slice(&(count + 1).to_le_bytes());
            assert!(matches!(
                decode(&malformed),
                Err(MeshDecodeError::Invalid(_))
            ));
            at += 4 + count as usize * 4;
        }
        let mut trailing = bytes;
        trailing.extend_from_slice(&[0, 0, 0, 0]);
        assert!(matches!(
            decode(&trailing),
            Err(MeshDecodeError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_metadata_and_geometry_count_bombs_before_allocation() {
        let bytes = encode(&primitives::cube(1.0));
        let geometry = geometry_offset(&bytes);
        let mut excessive = bytes.clone();
        excessive[geometry..geometry + 4]
            .copy_from_slice(&((MAX_VERTICES as u32 + 1) * 3).to_le_bytes());
        assert!(matches!(
            decode(&excessive),
            Err(MeshDecodeError::Invalid(_))
        ));
        let mut r = R(&bytes);
        r.take(8).unwrap();
        r.str().unwrap();
        let materials = bytes.len() - r.0.len();
        excessive = bytes.clone();
        excessive[materials..materials + 4].copy_from_slice(&65537_u32.to_le_bytes());
        assert!(matches!(
            decode(&excessive),
            Err(MeshDecodeError::Invalid(_))
        ));
        excessive = bytes;
        excessive[8..12].copy_from_slice(&65537_u32.to_le_bytes());
        assert!(matches!(
            decode(&excessive),
            Err(MeshDecodeError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_nonfinite_geometry_in_every_float_section() {
        let bytes = encode(&primitives::cube(1.0));
        let mut at = geometry_offset(&bytes);
        for _ in 0..4 {
            let count = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            if count != 0 {
                let mut malformed = bytes.clone();
                malformed[at + 4..at + 8].copy_from_slice(&f32::NAN.to_le_bytes());
                assert!(matches!(
                    decode(&malformed),
                    Err(MeshDecodeError::Invalid(_))
                ));
            }
            at += 4 + count * 4;
        }
    }

    #[test]
    fn accepts_string_limit_and_rejects_short_declared_sections() {
        let mut mesh = primitives::cube(1.0);
        mesh.name = "x".repeat(MAX_STRING_BYTES);
        let bytes = encode(&mesh);
        assert_eq!(decode(&bytes).unwrap().name, mesh.name);
        let at = geometry_offset(&bytes);
        let mut truncated = bytes[..at + 4].to_vec();
        truncated[at..at + 4].copy_from_slice(&12_u32.to_le_bytes());
        assert!(matches!(
            decode(&truncated),
            Err(MeshDecodeError::Truncated)
        ));
        mesh.name.push('x');
        assert!(matches!(
            decode(&encode(&mesh)),
            Err(MeshDecodeError::Invalid(_))
        ));
    }

    #[test]
    fn roundtrip_exact() {
        let m = primitives::sample_crate();
        let bytes = encode(&m);
        let back = decode(&bytes).unwrap();
        assert_eq!(back.positions, m.positions);
        assert_eq!(back.indices, m.indices);
        assert_eq!(back.tangents, m.tangents);
        assert_eq!(back.materials, m.materials);
        assert_eq!(back.submeshes, m.submeshes);
        assert_eq!(back.objects, m.objects);
        assert_eq!(back.tri_object, m.tri_object);
    }

    #[test]
    fn rejects_truncated_and_garbage() {
        let bytes = encode(&primitives::cube(1.0));
        assert!(decode(&bytes[..bytes.len() / 2]).is_err());
        assert!(matches!(
            decode(b"NOTMESH!"),
            Err(MeshDecodeError::BadMagic)
        ));
    }

    #[test]
    fn version_one_loads_without_object_metadata_and_invalid_indices_are_rejected() {
        let mesh = primitives::cube(1.0);
        let mut old = encode(&mesh);
        old.truncate(old.len() - 8 - mesh.tri_object.len() * 4);
        old[6..8].copy_from_slice(&1u16.to_le_bytes());
        let back = decode(&old).unwrap();
        assert!(back.objects.is_empty() && back.tri_object.is_empty());
        assert_eq!(back.indices, mesh.indices);
        let mut broken = mesh;
        broken.tri_object[0] = Some(100);
        assert!(matches!(
            decode(&encode(&broken)),
            Err(MeshDecodeError::Invalid(_))
        ));
    }
}
