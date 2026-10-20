//! Bounded clean-room codec for the observed Painter scene 1.7 node subset.
//!
//! Node metadata remains opaque. Matrix components are retained as their raw
//! little-endian IEEE-754 bit patterns; this module does not apply transforms.

use crate::spp_scene::{MAX_SCENE_BYTES, SppSceneError};

const MAX_NODE_DEPTH: usize = 64;
const MAX_NODE_COUNT: usize = 4096;
const MAX_MESH_REFERENCES: usize = 1_000_000;
const MAX_NAME_BYTES: usize = 64 * 1024;

type Result<T> = std::result::Result<T, SppSceneError>;

/// One observed scene node, retaining unrecognized metadata byte-for-byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SppSceneNode {
    pub name: String,
    /// Sixteen matrix components as raw little-endian `f32` bits.
    pub matrix_bits: [u32; 16],
    /// Indices into the scene mesh array supplied to [`Self::decode`].
    pub mesh_indices: Vec<u32>,
    pub children: Vec<Self>,
    /// Opaque node metadata suffix. Its internal fields are not interpreted.
    pub opaque_suffix: Vec<u8>,
}

impl SppSceneNode {
    /// Decode exactly one node block, validating every mesh reference against `mesh_count`.
    pub fn decode(raw: &[u8], mesh_count: usize) -> Result<Self> {
        if raw.len() > MAX_SCENE_BYTES {
            return Err(SppSceneError::Limit("node bytes"));
        }
        let mut reader = Reader::new(raw);
        let mut budget = Budget::default();
        let node = decode_node(&mut reader, mesh_count, 0, &mut budget)?;
        if !reader.is_empty() {
            return Err(SppSceneError::Invalid("trailing node bytes"));
        }
        Ok(node)
    }

    /// Encode this node using the observed 1.7 lengths and duplicate vector counts.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut writer = Writer::default();
        let mut budget = Budget::default();
        encode_node(self, &mut writer, 0, &mut budget)?;
        Ok(writer.bytes)
    }
}

#[derive(Default)]
struct Budget {
    nodes: usize,
    mesh_references: usize,
}

fn decode_node(
    parent: &mut Reader<'_>,
    mesh_count: usize,
    depth: usize,
    budget: &mut Budget,
) -> Result<SppSceneNode> {
    if depth >= MAX_NODE_DEPTH {
        return Err(SppSceneError::Limit("node depth"));
    }
    budget.nodes = budget
        .nodes
        .checked_add(1)
        .ok_or(SppSceneError::Limit("node count"))?;
    if budget.nodes > MAX_NODE_COUNT {
        return Err(SppSceneError::Limit("node count"));
    }

    let mut block = parent.block()?;
    let name = block.text()?;
    let mut matrix_bits = [0; 16];
    for component in &mut matrix_bits {
        *component = block.u32()?;
    }

    let child_count = block.count(MAX_NODE_COUNT, 1)?;
    let mesh_reference_count = block.count(MAX_MESH_REFERENCES, 4)?;
    let mesh_array_count = block.count(MAX_MESH_REFERENCES, 4)?;
    if mesh_array_count != mesh_reference_count {
        return Err(SppSceneError::Invalid("mesh reference array count"));
    }
    budget.mesh_references = budget
        .mesh_references
        .checked_add(mesh_reference_count)
        .ok_or(SppSceneError::Limit("mesh reference count"))?;
    if budget.mesh_references > MAX_MESH_REFERENCES {
        return Err(SppSceneError::Limit("mesh reference count"));
    }
    let mut mesh_indices = Vec::with_capacity(mesh_reference_count);
    for _ in 0..mesh_reference_count {
        let index = block.u32()?;
        if index as usize >= mesh_count {
            return Err(SppSceneError::Invalid("mesh reference"));
        }
        mesh_indices.push(index);
    }

    let child_array_count = block.count(MAX_NODE_COUNT, 8)?;
    if child_array_count != child_count {
        return Err(SppSceneError::Invalid("child array count"));
    }
    if child_count > MAX_NODE_COUNT.saturating_sub(budget.nodes) {
        return Err(SppSceneError::Limit("node count"));
    }
    let mut children = Vec::with_capacity(child_count);
    for _ in 0..child_count {
        children.push(decode_node(&mut block, mesh_count, depth + 1, budget)?);
    }
    let opaque_suffix = block.remaining().to_vec();

    Ok(SppSceneNode {
        name,
        matrix_bits,
        mesh_indices,
        children,
        opaque_suffix,
    })
}

fn encode_node(
    node: &SppSceneNode,
    writer: &mut Writer,
    depth: usize,
    budget: &mut Budget,
) -> Result<()> {
    if depth >= MAX_NODE_DEPTH {
        return Err(SppSceneError::Limit("node depth"));
    }
    budget.nodes = budget
        .nodes
        .checked_add(1)
        .ok_or(SppSceneError::Limit("node count"))?;
    if budget.nodes > MAX_NODE_COUNT {
        return Err(SppSceneError::Limit("node count"));
    }
    if node.children.len() > MAX_NODE_COUNT || node.mesh_indices.len() > MAX_MESH_REFERENCES {
        return Err(SppSceneError::Limit("node arrays"));
    }
    budget.mesh_references = budget
        .mesh_references
        .checked_add(node.mesh_indices.len())
        .ok_or(SppSceneError::Limit("mesh reference count"))?;
    if budget.mesh_references > MAX_MESH_REFERENCES {
        return Err(SppSceneError::Limit("mesh reference count"));
    }

    let start = writer.begin_block()?;
    writer.text(&node.name)?;
    for &component in &node.matrix_bits {
        writer.u32(component)?;
    }
    writer.count(node.children.len())?;
    writer.count(node.mesh_indices.len())?;
    // Version 1.7 writes a count for the mesh-index vector as well as its field count.
    writer.count(node.mesh_indices.len())?;
    for &index in &node.mesh_indices {
        writer.u32(index)?;
    }
    // The serialized child vector carries a second count, corroborated equal to child_count.
    writer.count(node.children.len())?;
    for child in &node.children {
        encode_node(child, writer, depth + 1, budget)?;
    }
    writer.bytes(&node.opaque_suffix)?;
    writer.finish_block(start);
    Ok(())
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

    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .filter(|&end| end <= self.bytes.len())
            .ok_or(SppSceneError::Invalid("truncated node"))?;
        let value = &self.bytes[self.position..end];
        self.position = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn count(&mut self, max: usize, minimum_bytes_per_item: usize) -> Result<usize> {
        let count = usize::try_from(self.u32()?).map_err(|_| SppSceneError::Limit("node count"))?;
        if count > max {
            return Err(SppSceneError::Limit("node array count"));
        }
        let required = count
            .checked_mul(minimum_bytes_per_item)
            .ok_or(SppSceneError::Limit("node array size"))?;
        if required > self.remaining().len() {
            return Err(SppSceneError::Invalid("node array exceeds block"));
        }
        Ok(count)
    }

    fn text(&mut self) -> Result<String> {
        let length =
            usize::try_from(self.u32()?).map_err(|_| SppSceneError::Limit("name bytes"))?;
        if length > MAX_NAME_BYTES {
            return Err(SppSceneError::Limit("name bytes"));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| SppSceneError::Invalid("node name UTF-8"))
    }

    fn block(&mut self) -> Result<Reader<'a>> {
        let length = usize::try_from(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
            .map_err(|_| SppSceneError::Limit("node block length"))?;
        if length < 8 {
            return Err(SppSceneError::Invalid("node block length"));
        }
        Ok(Reader::new(self.take(length - 8)?))
    }
}

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > MAX_SCENE_BYTES.saturating_sub(self.bytes.len()) {
            return Err(SppSceneError::Limit("node bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn u32(&mut self, value: u32) -> Result<()> {
        self.bytes(&value.to_le_bytes())
    }

    fn count(&mut self, count: usize) -> Result<()> {
        self.u32(u32::try_from(count).map_err(|_| SppSceneError::Limit("node array count"))?)
    }

    fn text(&mut self, text: &str) -> Result<()> {
        if text.len() > MAX_NAME_BYTES {
            return Err(SppSceneError::Limit("name bytes"));
        }
        self.count(text.len())?;
        self.bytes(text.as_bytes())
    }

    fn begin_block(&mut self) -> Result<usize> {
        let start = self.bytes.len();
        self.bytes(&[0; 8])?;
        Ok(start)
    }

    fn finish_block(&mut self, start: usize) {
        let length = (self.bytes.len() - start) as u64;
        self.bytes[start..start + 8].copy_from_slice(&length.to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> [u32; 16] {
        let mut matrix = [0; 16];
        matrix[0] = 1.0_f32.to_bits();
        matrix[5] = 1.0_f32.to_bits();
        matrix[10] = 1.0_f32.to_bits();
        matrix[15] = 1.0_f32.to_bits();
        matrix
    }

    fn sample() -> SppSceneNode {
        SppSceneNode {
            name: "root".into(),
            matrix_bits: identity(),
            mesh_indices: vec![],
            children: vec![SppSceneNode {
                name: "object".into(),
                matrix_bits: identity(),
                mesh_indices: vec![0, 1],
                children: vec![],
                opaque_suffix: vec![1, 0, 2, 0, 255, 0],
            }],
            opaque_suffix: vec![4, 0, 0, 0, 42, 43],
        }
    }

    fn raw_nested_node(depth: usize) -> Vec<u8> {
        let mut writer = Writer::default();
        let start = writer.begin_block().unwrap();
        writer.text("nested").unwrap();
        for _ in 0..16 {
            writer.u32(0).unwrap();
        }
        let child_count = usize::from(depth > 0);
        writer.count(child_count).unwrap();
        writer.count(0).unwrap();
        writer.count(0).unwrap();
        if depth > 0 {
            writer.count(1).unwrap();
            let child = raw_nested_node(depth - 1);
            writer.bytes(&child).unwrap();
        } else {
            writer.count(0).unwrap();
        }
        writer.finish_block(start);
        writer.bytes
    }

    #[test]
    fn roundtrip_preserves_tree_refs_matrix_bits_and_opaque_suffixes() {
        let mut node = sample();
        node.children[0].matrix_bits[2] = f32::from_bits(0x7fc0_1234).to_bits();
        let bytes = node.encode().unwrap();
        let decoded = SppSceneNode::decode(&bytes, 2).unwrap();
        assert_eq!(decoded, node);
        assert_eq!(decoded.encode().unwrap(), bytes);
    }

    #[test]
    fn rejects_truncated_length_mismatched_counts_and_bad_mesh_refs() {
        let node = sample();
        let bytes = node.encode().unwrap();
        assert!(SppSceneNode::decode(&bytes[..bytes.len() - 1], 2).is_err());

        let mut bad_count = bytes.clone();
        let child = bad_count
            .windows(6)
            .position(|window| window == b"object")
            .unwrap();
        let matrix_end = child + 6 + 64;
        // child_count=0, mesh-ref count=2, then the repeated mesh-array count.
        bad_count[matrix_end + 8..matrix_end + 12].copy_from_slice(&3_u32.to_le_bytes());
        assert!(SppSceneNode::decode(&bad_count, 2).is_err());

        let mut bad_ref = bytes;
        let refs = matrix_end + 12;
        bad_ref[refs..refs + 4].copy_from_slice(&2_u32.to_le_bytes());
        assert!(SppSceneNode::decode(&bad_ref, 2).is_err());
    }

    #[test]
    fn enforces_recursion_node_and_reference_limits() {
        let too_deep = raw_nested_node(MAX_NODE_DEPTH);
        assert!(matches!(
            SppSceneNode::decode(&too_deep, 0),
            Err(SppSceneError::Limit("node depth"))
        ));

        let mut too_many_refs = sample();
        too_many_refs.children[0].mesh_indices = vec![0; MAX_MESH_REFERENCES + 1];
        assert!(matches!(
            too_many_refs.encode(),
            Err(SppSceneError::Limit("node arrays"))
                | Err(SppSceneError::Limit("mesh reference count"))
        ));
    }
}
