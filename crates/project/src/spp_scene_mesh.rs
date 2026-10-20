//! Explicit conversion from the observed scene 1.7 geometry subset into the
//! canonical reference mesh. The original [`SppScene`] remains the source of
//! truth; Painter-specific data is not represented by this geometry result.

use std::collections::HashSet;

use glam::{Vec2, Vec3};
use subcraft_mesh::{BuildError, Corner, Mesh, MeshBuilder};
use thiserror::Error;

use crate::spp_scene::{SppScene, SppSceneError};
use crate::spp_scene_node::SppSceneNode;

const MAX_REFERENCE_TRIANGLES: usize = 250_000;
const MIN_AREA: f32 = 1e-12;

#[derive(Debug, Error)]
pub enum SppReferenceMeshError {
    #[error(transparent)]
    Scene(#[from] SppSceneError),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error("unsupported SPP reference geometry: {0}")]
    Unsupported(&'static str),
    #[error("invalid SPP reference geometry: {0}")]
    Invalid(&'static str),
    #[error("SPP reference geometry exceeds limit: {0}")]
    Limit(&'static str),
}

type Result<T> = std::result::Result<T, SppReferenceMeshError>;

/// Canonical geometry plus explicit indices back to the retained scene data.
#[derive(Debug)]
pub struct SppReferenceMesh {
    pub mesh: Mesh,
    /// Source material-table index to compacted canonical material slot.
    /// `None` means that source material was unused and the builder removed it.
    pub material_slot_by_source_index: Vec<Option<u32>>,
    /// Source mesh index to canonical object index, preserving scene ownership.
    pub object_index_by_source_mesh: Vec<u32>,
    /// Source mesh names in original scene order.
    pub source_mesh_names: Vec<String>,
}

/// Build canonical reference geometry from the supported scene subset.
///
/// Stored positions are used directly as reference coordinates; node matrices
/// must be identity. Painter OBJ UVs are bottom-left-origin, so V is converted
/// to SubCraft's top-left convention with `1 - v`, as in the native OBJ path.
/// Tangents are recomputed by `MeshBuilder`; the source scene should be retained
/// to preserve every Painter-specific field.
pub fn build_reference_mesh(scene: &SppScene) -> Result<SppReferenceMesh> {
    // Validate public mutable mesh fields and all retained scene records before allocating corners.
    scene.encode()?;
    let hierarchy = scene.hierarchy()?;
    let source_materials = scene.materials()?;
    let source_names = source_materials.names().map_err(SppSceneError::from)?;
    if source_names.len() != source_materials.materials.len() {
        return Err(SppReferenceMeshError::Invalid("material name table length"));
    }

    let mut unique_names = HashSet::with_capacity(source_names.len());
    let mut material_names = Vec::with_capacity(source_names.len());
    for name in source_names {
        let name = name.ok_or(SppReferenceMeshError::Unsupported(
            "material without an observed name",
        ))?;
        if name.trim().is_empty() {
            return Err(SppReferenceMeshError::Unsupported("empty material name"));
        }
        if !unique_names.insert(name.to_owned()) {
            return Err(SppReferenceMeshError::Unsupported(
                "duplicate material names",
            ));
        }
        material_names.push(name.to_owned());
    }
    if material_names.is_empty() {
        return Err(SppReferenceMeshError::Invalid("empty material table"));
    }

    let mut expected_triangles = 0usize;
    for mesh in &scene.meshes {
        expected_triangles = expected_triangles
            .checked_add(mesh.triangles.len())
            .filter(|&count| count <= MAX_REFERENCE_TRIANGLES)
            .ok_or(SppReferenceMeshError::Limit("triangle count"))?;
    }
    if expected_triangles == 0 {
        return Err(SppReferenceMeshError::Invalid("empty geometry"));
    }

    validate_source_geometry(scene)?;

    let mut mesh_seen = vec![false; scene.meshes.len()];
    let mut object_index_by_source_mesh = vec![u32::MAX; scene.meshes.len()];
    let mut geometry_nodes = Vec::new();
    let mut next_object_index = 0u32;
    collect_geometry_nodes(
        &hierarchy,
        true,
        scene.meshes.len(),
        &mut mesh_seen,
        &mut object_index_by_source_mesh,
        &mut next_object_index,
        &mut geometry_nodes,
    )?;
    if mesh_seen.iter().any(|&seen| !seen) {
        return Err(SppReferenceMeshError::Invalid("unreferenced source mesh"));
    }

    let mut builder = MeshBuilder::new("SPP reference mesh");
    // Register all verified names up front. MeshBuilder may remove unused slots;
    // the final public mapping is reconstructed by exact name after build.
    let source_to_builder: Vec<u32> = material_names
        .iter()
        .map(|name| builder.material(name))
        .collect();
    let mut builder_triangles = 0usize;
    for geometry_node in geometry_nodes {
        builder.object(&geometry_node.name);
        for source_index in geometry_node.mesh_indices {
            let source_mesh = &scene.meshes[source_index];
            let source_material = source_mesh.material_index as usize;
            let material_slot = *source_to_builder
                .get(source_material)
                .ok_or(SppReferenceMeshError::Invalid("mesh material index"))?;

            for triangle in &source_mesh.triangles {
                let mut corners = [Corner {
                    position: Vec3::ZERO,
                    normal: None,
                    uv: None,
                }; 3];
                for (corner, &vertex_index) in corners.iter_mut().zip(triangle) {
                    let vertex_index = vertex_index as usize;
                    let position = source_mesh.positions[vertex_index];
                    let normal = Vec3::from_array(source_mesh.normals[vertex_index]);
                    let uv = source_mesh.uvs[vertex_index];
                    corner.position = Vec3::from_array(position);
                    corner.normal = Some(normal);
                    corner.uv = Some(Vec2::new(uv[0], 1.0 - uv[1]));
                }
                builder.push_triangle(material_slot, corners);
                builder_triangles += 1;
            }
        }
    }
    if builder_triangles != expected_triangles || builder.triangle_count() != expected_triangles {
        return Err(SppReferenceMeshError::Invalid(
            "triangle count changed during conversion",
        ));
    }
    let stats = builder.stats;
    if stats.degenerate_dropped != 0
        || stats.non_finite_dropped != 0
        || stats.missing_uv_triangles != 0
        || stats.generated_normals
    {
        return Err(SppReferenceMeshError::Invalid(
            "MeshBuilder dropped or regenerated source geometry",
        ));
    }

    let mesh = builder.build()?;
    if mesh.triangle_count() != expected_triangles {
        return Err(SppReferenceMeshError::Invalid(
            "triangle count changed during mesh build",
        ));
    }
    let problems = mesh.validate();
    if !problems.is_empty() {
        return Err(SppReferenceMeshError::Invalid(
            "canonical mesh validation failed",
        ));
    }

    let mut compacted_by_name = std::collections::HashMap::with_capacity(mesh.materials.len());
    for (slot, material) in mesh.materials.iter().enumerate() {
        if compacted_by_name
            .insert(material.name.as_str(), slot as u32)
            .is_some()
        {
            return Err(SppReferenceMeshError::Invalid(
                "canonical material names are not unique",
            ));
        }
    }
    let material_slot_by_source_index = material_names
        .iter()
        .map(|name| compacted_by_name.get(name.as_str()).copied())
        .collect();
    let source_mesh_names = scene.meshes.iter().map(|mesh| mesh.name.clone()).collect();

    Ok(SppReferenceMesh {
        mesh,
        material_slot_by_source_index,
        object_index_by_source_mesh,
        source_mesh_names,
    })
}

struct GeometryNode {
    name: String,
    mesh_indices: Vec<usize>,
}

fn collect_geometry_nodes(
    node: &SppSceneNode,
    is_root: bool,
    mesh_count: usize,
    mesh_seen: &mut [bool],
    object_index_by_source_mesh: &mut [u32],
    next_object_index: &mut u32,
    geometry_nodes: &mut Vec<GeometryNode>,
) -> Result<()> {
    if node.matrix_bits != identity_matrix_bits() {
        return Err(SppReferenceMeshError::Unsupported(
            "non-identity node transform",
        ));
    }
    if !node.mesh_indices.is_empty() && !node.children.is_empty() {
        return Err(SppReferenceMeshError::Unsupported(
            "node with both meshes and children",
        ));
    }
    if node.mesh_indices.is_empty() {
        if node.children.is_empty() {
            if is_root {
                return Err(SppReferenceMeshError::Invalid("no referenced geometry"));
            }
            return Err(SppReferenceMeshError::Unsupported("empty scene node"));
        }
        if !is_root {
            return Err(SppReferenceMeshError::Unsupported(
                "nested grouping nodes are not represented by Mesh",
            ));
        }
        for child in &node.children {
            collect_geometry_nodes(
                child,
                false,
                mesh_count,
                mesh_seen,
                object_index_by_source_mesh,
                next_object_index,
                geometry_nodes,
            )?;
        }
        return Ok(());
    }

    if node.name.trim().is_empty() {
        return Err(SppReferenceMeshError::Unsupported(
            "geometry node without a name",
        ));
    }
    let object_index = *next_object_index;
    *next_object_index = next_object_index
        .checked_add(1)
        .ok_or(SppReferenceMeshError::Limit("object count"))?;

    let mut mesh_indices = Vec::with_capacity(node.mesh_indices.len());
    for &mesh_index in &node.mesh_indices {
        let source_index = usize::try_from(mesh_index)
            .map_err(|_| SppReferenceMeshError::Invalid("mesh reference"))?;
        let Some(mesh_seen_slot) = mesh_seen.get_mut(source_index) else {
            return Err(SppReferenceMeshError::Invalid("mesh reference"));
        };
        if *mesh_seen_slot {
            return Err(SppReferenceMeshError::Unsupported("instanced source mesh"));
        }
        if source_index >= mesh_count {
            return Err(SppReferenceMeshError::Invalid("mesh reference"));
        }
        *mesh_seen_slot = true;
        object_index_by_source_mesh[source_index] = object_index;
        mesh_indices.push(source_index);
    }
    geometry_nodes.push(GeometryNode {
        name: node.name.clone(),
        mesh_indices,
    });
    Ok(())
}

fn validate_source_geometry(scene: &SppScene) -> Result<()> {
    for mesh in &scene.meshes {
        for normal in &mesh.normals {
            let normal = Vec3::from_array(*normal);
            let length_squared = normal.length_squared();
            if !normal.is_finite() || !length_squared.is_finite() || length_squared <= 1e-12 {
                return Err(SppReferenceMeshError::Invalid("invalid vertex normal"));
            }
        }
        if mesh.uvs.iter().any(|uv| uv[2] != 0.0) {
            return Err(SppReferenceMeshError::Unsupported(
                "nonzero third UV component",
            ));
        }
        for triangle in &mesh.triangles {
            let points = triangle.map(|index| mesh.positions[index as usize]);
            let twice_area = (Vec3::from_array(points[1]) - Vec3::from_array(points[0]))
                .cross(Vec3::from_array(points[2]) - Vec3::from_array(points[0]))
                .length();
            if !twice_area.is_finite() || twice_area <= MIN_AREA {
                return Err(SppReferenceMeshError::Invalid("degenerate triangle"));
            }
        }
    }
    Ok(())
}

fn identity_matrix_bits() -> [u32; 16] {
    let zero = 0.0_f32.to_bits();
    let one = 1.0_f32.to_bits();
    [
        one, zero, zero, zero, zero, one, zero, zero, zero, zero, one, zero, zero, zero, zero, one,
    ]
}
