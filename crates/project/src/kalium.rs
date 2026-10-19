//! Clean-room importer for Kalium v9 project archives.
//!
//! Kalium and SubCraft have different editable document models. This module maps only semantics
//! represented by both models and fails closed when an archive contains unknown metadata or a
//! source feature that SubCraft cannot retain. It never treats `.spp` as an importable format.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use glam::{Vec2, Vec3, Vec4};
use serde::Deserialize;
use serde_json::{Map as JsonMap, Value as JsonValue};
use subcraft_core::{ChannelKind, ChannelValue, LayerId, TextureSetId};
use subcraft_doc::{
    BlendMode, ChannelBlend, ChannelDef, Document, FillChannel, FillContent, FillSource,
    GraphEdge, GroupContent, Layer, LayerContent, Mask, PaintContent, ProceduralGraph,
    ProceduralNode, ProceduralNodeKind, TextureSet,
};
use subcraft_image::{BitmapData, TiledImage};
use subcraft_mesh::{Aabb, MaterialSlot, Mesh, MeshObject, Submesh};
use uuid::Uuid;
use zip::ZipArchive;

use crate::{MeshMapSet, ProjectData, ProjectError, ResourceStore, limits};

const KALIUM_FORMAT: &str = "Kalium Project";
const KALIUM_V9: u32 = 9;
const IMPORT_RASTER_BUDGET: u64 = 512 * 1024 * 1024;
const MAX_KALIUM_SETS: usize = 16;
const MAX_KALIUM_NODES: usize = 4096;
const MAX_KALIUM_DEPTH: usize = 32;
const PNG_OVERHEAD: u64 = 4 * 1024 * 1024;

#[derive(Deserialize)]
struct SourceManifest {
    format: String,
    version: u32,
    project_id: Option<Uuid>,
    mesh: SourceMesh,
    size: u32,
    #[serde(default)]
    selected: usize,
    #[serde(default)]
    next_layer_id: u32,
    #[serde(default)]
    layers: Vec<JsonValue>,
    active_set_id: u32,
    active_set_name: String,
    next_set_id: u32,
    material_bindings: Vec<Option<u32>>,
    layer_tree: Option<SourceTree>,
    #[serde(default)]
    texture_sets: Vec<SourceSet>,
    #[serde(flatten)]
    extensions: JsonMap<String, JsonValue>,
}

#[derive(Deserialize)]
struct SourceMesh {
    name: String,
    vertices: Vec<SourceVertex>,
    indices: Vec<u32>,
    #[serde(default)]
    materials: Vec<String>,
    #[serde(default)]
    triangle_materials: Vec<u32>,
    #[serde(default)]
    extra_uv_sets: BTreeMap<u32, Vec<Option<[f32; 2]>>>,
    #[serde(flatten)]
    extensions: JsonMap<String, JsonValue>,
}

#[derive(Deserialize)]
struct SourceVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    #[serde(default)]
    tangent: Option<[f32; 4]>,
    #[serde(flatten)]
    extensions: JsonMap<String, JsonValue>,
}

#[derive(Deserialize)]
struct SourceSet {
    id: u32,
    name: String,
    size: u32,
    #[serde(default)]
    selected: usize,
    #[serde(default)]
    next_layer_id: u32,
    #[serde(default)]
    layers: Vec<JsonValue>,
    layer_tree: Option<SourceTree>,
    #[serde(flatten)]
    extensions: JsonMap<String, JsonValue>,
}

#[derive(Deserialize)]
struct SourceTree {
    selected_node_id: u32,
    next_node_id: u32,
    roots: Vec<SourceNode>,
    #[serde(flatten)]
    extensions: JsonMap<String, JsonValue>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SourceNode {
    Paint {
        #[serde(flatten)]
        properties: SourceProperties,
        #[serde(default)]
        channels: Vec<SourceChannel>,
        #[serde(default)]
        procedural_graph: Option<SourceProceduralGraph>,
    },
    Fill {
        #[serde(flatten)]
        properties: SourceProperties,
        values: Vec<SourceFillValue>,
    },
    Group {
        #[serde(flatten)]
        properties: SourceProperties,
        children: Vec<SourceNode>,
    },
}

#[derive(Deserialize)]
struct SourceProperties {
    id: u32,
    name: String,
    visible: bool,
    opacity: f32,
    blend: crate::kalium::source_blend::Mode,
    #[serde(default)]
    disabled_channels: Vec<SourceChannel>,
    #[serde(flatten)]
    extensions: JsonMap<String, JsonValue>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "PascalCase")]
enum SourceChannel {
    BaseColor,
    Roughness,
    Metallic,
    Normal,
    Height,
    Emissive,
    Opacity,
    Occlusion,
}

impl SourceChannel {
    fn target(self) -> ChannelKind {
        match self {
            Self::BaseColor => ChannelKind::BaseColor,
            Self::Roughness => ChannelKind::Roughness,
            Self::Metallic => ChannelKind::Metallic,
            Self::Normal => ChannelKind::Normal,
            Self::Height => ChannelKind::Height,
            Self::Emissive => ChannelKind::Emissive,
            Self::Opacity => ChannelKind::Opacity,
            Self::Occlusion => ChannelKind::AmbientOcclusion,
        }
    }

    fn filename(self) -> &'static str {
        match self {
            Self::BaseColor => "BaseColor",
            Self::Roughness => "Roughness",
            Self::Metallic => "Metallic",
            Self::Normal => "Normal",
            Self::Height => "Height",
            Self::Emissive => "Emissive",
            Self::Opacity => "Opacity",
            Self::Occlusion => "AmbientOcclusion",
        }
    }
}

#[derive(Deserialize)]
struct SourceFillValue {
    channel: SourceChannel,
    value: [u8; 4],
    #[serde(flatten)]
    extensions: JsonMap<String, JsonValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceProceduralGraph {
    nodes: Vec<SourceProceduralNode>,
    edges: Vec<GraphEdge>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceProceduralNode {
    id: u32,
    kind: SourceProceduralNodeKind,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum SourceProceduralNodeKind {
    Uv,
    ValueNoise { seed: u64, scale: f32 },
    OcclusionInput { source_layer_id: u32 },
    Multiply,
    RoughnessOutput,
}

// Keep the source enum private so the import contract is independent of Kalium's Rust crate.
mod source_blend {
    use serde::Deserialize;
    #[derive(Deserialize, Clone, Copy, Debug)]
    pub(super) enum Mode {
        Normal,
        Multiply,
        Add,
        Detail,
        Divide,
        InverseDivide,
        Darken,
        Lighten,
        Subtract,
        InverseSubtract,
        Difference,
        Exclusion,
        SignedAddition,
        Overlay,
        Screen,
        LinearBurn,
        ColorBurn,
        ColorDodge,
        SoftLight,
        HardLight,
        VividLight,
        LinearLight,
        PinLight,
        Tint,
        Saturation,
        Color,
        Value,
        NormalMapCombine,
        NormalMapDetail,
        NormalMapInverseDetail,
    }
}

impl source_blend::Mode {
    fn target(self) -> BlendMode {
        use source_blend::Mode as S;
        match self {
            S::Normal => BlendMode::Normal,
            S::Multiply => BlendMode::Multiply,
            S::Add => BlendMode::LinearDodge,
            S::Detail => BlendMode::NormalMapDetail,
            S::Divide => BlendMode::Divide,
            S::InverseDivide => BlendMode::InverseDivide,
            S::Darken => BlendMode::Darken,
            S::Lighten => BlendMode::Lighten,
            S::Subtract => BlendMode::Subtract,
            S::InverseSubtract => BlendMode::InverseSubtract,
            S::Difference => BlendMode::Difference,
            S::Exclusion => BlendMode::Exclusion,
            S::SignedAddition => BlendMode::SignedAddition,
            S::Overlay => BlendMode::Overlay,
            S::Screen => BlendMode::Screen,
            S::LinearBurn => BlendMode::LinearBurn,
            S::ColorBurn => BlendMode::ColorBurn,
            S::ColorDodge => BlendMode::ColorDodge,
            S::SoftLight => BlendMode::SoftLight,
            S::HardLight => BlendMode::HardLight,
            S::VividLight => BlendMode::VividLight,
            S::LinearLight => BlendMode::LinearLight,
            S::PinLight => BlendMode::PinLight,
            S::Tint => BlendMode::Tint,
            S::Saturation => BlendMode::Saturation,
            S::Color => BlendMode::Color,
            S::Value => BlendMode::Value,
            S::NormalMapCombine => BlendMode::NormalMapCombine,
            S::NormalMapDetail => BlendMode::NormalMapDetail,
            S::NormalMapInverseDetail => BlendMode::NormalMapInverseDetail,
        }
    }
}

/// Load a Kalium `.kalium` project from disk for an explicit import operation.
pub fn load_kalium_project(path: &Path) -> Result<ProjectData, ProjectError> {
    read_kalium_project(std::io::BufReader::new(File::open(path)?))
}

/// Read and convert a supported Kalium project. Unknown fields and archive entries are rejected
/// so saving the result cannot silently discard source data.
pub fn read_kalium_project<R: Read + Seek>(mut reader: R) -> Result<ProjectData, ProjectError> {
    let directory = limits::preflight_zip_directory(&mut reader)?;
    reader.seek(SeekFrom::Start(0))?;
    let mut zip = ZipArchive::new(reader).map_err(|e| ProjectError::Container(e.to_string()))?;
    if zip.len() != directory.entry_count {
        return invalid("ZIP directory changed after preflight");
    }
    let mut budget = limits::ReadBudget::default();
    let manifest_bytes = limits::read_entry_bounded(
        &mut zip,
        &directory,
        "manifest.json",
        // Kalium embeds mesh geometry in manifest.json instead of a separate mesh entry.
        // Use the mesh cap (rather than SubCraft's tiny manifest-only cap) for real scenes.
        limits::MAX_MESH_BYTES,
        &mut budget,
    )?;
    let manifest: SourceManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| ProjectError::Invalid(format!("Kalium manifest: {e}")))?;
    if manifest.format != KALIUM_FORMAT {
        return Err(ProjectError::NotAProject);
    }
    if manifest.version != KALIUM_V9 {
        return invalid(format!(
            "Kalium v{} is not supported; import a v9 project",
            manifest.version
        ));
    }
    let project_id = manifest
        .project_id
        .ok_or_else(|| ProjectError::Invalid("Kalium v9 project is missing project_id".into()))?;
    ensure_empty("project", &manifest.extensions)?;
    if !manifest.layers.is_empty() {
        return invalid("Kalium v9 contains legacy root layers; refusing ambiguous data");
    }
    if manifest.selected != 0 || manifest.next_layer_id == 0 || manifest.next_set_id == 0 {
        return invalid("Kalium v9 contains invalid legacy counters");
    }
    if !manifest.mesh.extra_uv_sets.is_empty() {
        return invalid("Kalium mesh has additional UV sets which SubCraft cannot retain");
    }
    ensure_empty("mesh", &manifest.mesh.extensions)?;
    for vertex in &manifest.mesh.vertices {
        ensure_empty("mesh vertex", &vertex.extensions)?;
    }
    let mesh = convert_mesh(manifest.mesh)?;

    if manifest.texture_sets.len() > MAX_KALIUM_SETS {
        return invalid(format!(
            "Kalium project exceeds the {MAX_KALIUM_SETS}-set import limit"
        ));
    }
    let active_size = validate_resolution(manifest.size, "active texture set")?;
    let active_tree = manifest.layer_tree.ok_or_else(|| {
        ProjectError::Invalid("Kalium v9 is missing its active layer tree".into())
    })?;
    validate_tree_metadata(&active_tree, "active texture set")?;
    validate_tree_selection(&active_tree)?;

    let mut source_sets = HashMap::new();
    source_sets
        .try_reserve(manifest.texture_sets.len())
        .map_err(|_| ProjectError::Invalid("texture-set allocation failed".into()))?;
    for set in manifest.texture_sets {
        if set.id == 0 || set.id == manifest.active_set_id || source_sets.contains_key(&set.id) {
            return invalid("Kalium texture sets contain a zero, duplicate, or active-set ID");
        }
        ensure_empty("texture set", &set.extensions)?;
        if !set.layers.is_empty() || set.selected != 0 || set.next_layer_id == 0 {
            return invalid("Kalium v9 texture set contains ambiguous legacy layer data");
        }
        let size = validate_resolution(set.size, &set.name)?;
        let tree = set.layer_tree.ok_or_else(|| {
            ProjectError::Invalid(format!(
                "Kalium texture set '{}' is missing its v9 layer tree",
                set.name
            ))
        })?;
        validate_tree_metadata(&tree, &set.name)?;
        validate_tree_selection(&tree)?;
        source_sets.insert(set.id, (set.name, size, tree));
    }
    if manifest.active_set_id == 0 || source_sets.contains_key(&manifest.active_set_id) {
        return invalid("Kalium active texture-set ID is invalid");
    }

    let mut expected_entries = BTreeSet::new();
    expected_entries.insert("manifest.json".to_owned());
    let active_key = SourceSetKey {
        id: manifest.active_set_id,
        prefix: None,
        size: active_size,
    };
    let mut node_count = 0usize;
    collect_entries(
        &active_tree.roots,
        &active_key,
        project_id,
        1,
        &mut node_count,
        &mut expected_entries,
    )?;
    let mut set_keys = HashMap::new();
    for (&id, (name, size, tree)) in &source_sets {
        let key = SourceSetKey {
            id,
            prefix: Some(id),
            size: *size,
        };
        collect_entries(
            &tree.roots,
            &key,
            project_id,
            1,
            &mut node_count,
            &mut expected_entries,
        )
            .map_err(|e| ProjectError::Invalid(format!("texture set '{name}': {e}")))?;
        set_keys.insert(id, key);
    }
    let archive_entries: BTreeSet<_> = directory.entries.keys().cloned().collect();
    if archive_entries != expected_entries {
        let unexpected: Vec<_> = archive_entries
            .difference(&expected_entries)
            .take(3)
            .cloned()
            .collect();
        let missing: Vec<_> = expected_entries
            .difference(&archive_entries)
            .take(3)
            .cloned()
            .collect();
        return invalid(format!(
            "Kalium archive entries do not match its v9 manifest (unexpected: {unexpected:?}; missing: {missing:?}); refusing lossy import"
        ));
    }

    let mut decoded_budget = 0u64;
    let mut active_nodes = convert_nodes(
        &active_tree.roots,
        &active_key,
        project_id,
        &mut zip,
        &directory,
        &mut budget,
        &mut decoded_budget,
    )?;
    let active_channels = collect_channels(&active_nodes);
    let active_background = background_layer(
        project_id,
        manifest.active_set_id,
        active_size,
        &active_channels,
    );
    active_nodes.insert(0, std::sync::Arc::new(active_background));
    let mut converted_sets = HashMap::new();
    for (id, (name, size, tree)) in source_sets {
        let key = set_keys
            .remove(&id)
            .expect("key was built for each source set");
        let mut roots = convert_nodes(
            &tree.roots,
            &key,
            project_id,
            &mut zip,
            &directory,
            &mut budget,
            &mut decoded_budget,
        )?;
        let channels = collect_channels(&roots);
        roots.insert(
            0,
            std::sync::Arc::new(background_layer(project_id, id, size, &channels)),
        );
        converted_sets.insert(id, (name, size, roots));
    }

    let slot_count = mesh.materials.len();
    if manifest.material_bindings.len() != slot_count {
        return invalid("Kalium material binding count does not match mesh materials");
    }
    let mut seen_bound_sets = HashSet::new();
    let mut resolved_bindings = Vec::new();
    resolved_bindings
        .try_reserve_exact(slot_count)
        .map_err(|_| ProjectError::Invalid("material-binding allocation failed".into()))?;
    for binding in manifest.material_bindings {
        let id = binding.unwrap_or(manifest.active_set_id);
        if !seen_bound_sets.insert(id) {
            return invalid(
                "One Kalium texture set is bound to multiple material slots; SubCraft cannot preserve shared-set routing without duplicating editable data",
            );
        }
        if id == manifest.active_set_id {
            resolved_bindings.push((
                id,
                manifest.active_set_name.clone(),
                active_size,
                active_nodes.clone(),
            ));
        } else {
            let Some((name, size, roots)) = converted_sets.get(&id) else {
                return invalid(format!(
                    "Kalium material binding references unknown texture set {id}"
                ));
            };
            resolved_bindings.push((id, name.clone(), *size, roots.clone()));
        }
    }
    for id in converted_sets.keys() {
        if !seen_bound_sets.contains(id) {
            return invalid(format!(
                "Kalium texture set {id} is not bound to a mesh material slot; SubCraft cannot preserve its routing"
            ));
        }
    }

    let mut texture_sets = Vec::new();
    texture_sets
        .try_reserve_exact(slot_count)
        .map_err(|_| ProjectError::Invalid("texture-set allocation failed".into()))?;
    for (slot, (source_id, name, resolution, roots)) in resolved_bindings.into_iter().enumerate() {
        let material_slot = u32::try_from(slot)
            .map_err(|_| ProjectError::Invalid("too many mesh material slots".into()))?;
        let channels = collect_channels(&roots);
        let mut set = TextureSet::new(name, material_slot, resolution);
        set.id = stable_set_id(project_id, source_id, material_slot);
        set.channels = if channels.is_empty() {
            ChannelKind::METAL_ROUGH
                .into_iter()
                .map(ChannelDef::default_for)
                .collect()
        } else {
            channels.into_iter().map(ChannelDef::default_for).collect()
        };
        set.layers = roots;
        texture_sets.push(set);
    }
    let document = Document { texture_sets };
    Ok(ProjectData {
        mesh,
        document,
        resources: ResourceStore::default(),
        mesh_maps: MeshMapSet::new(),
    })
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ProjectError> {
    Err(ProjectError::Invalid(message.into()))
}

fn ensure_empty(where_: &str, extensions: &JsonMap<String, JsonValue>) -> Result<(), ProjectError> {
    if let Some((name, _)) = extensions.iter().next() {
        return invalid(format!(
            "unknown Kalium {where_} metadata field '{name}' cannot be retained by SubCraft"
        ));
    }
    Ok(())
}

fn validate_resolution(size: u32, label: &str) -> Result<u32, ProjectError> {
    if !subcraft_doc::RESOLUTIONS.contains(&size) {
        return invalid(format!(
            "{label} resolution {size} is not supported by SubCraft (supported: {:?})",
            subcraft_doc::RESOLUTIONS
        ));
    }
    Ok(size)
}

fn validate_tree_metadata(tree: &SourceTree, label: &str) -> Result<(), ProjectError> {
    ensure_empty("layer tree", &tree.extensions)?;
    if tree.roots.is_empty() || tree.selected_node_id == 0 || tree.next_node_id == 0 {
        return invalid(format!("{label} has invalid layer-tree counters"));
    }
    Ok(())
}

fn validate_tree_selection(tree: &SourceTree) -> Result<(), ProjectError> {
    let mut ids = HashSet::new();
    let mut max_id = 0u32;
    let mut stack: Vec<_> = tree.roots.iter().map(|node| (node, 1usize)).collect();
    let mut count = 0usize;
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        if count > MAX_KALIUM_NODES || depth > MAX_KALIUM_DEPTH {
            return invalid("Kalium layer tree exceeds its node or nesting limit");
        }
        let props = properties(node);
        if !ids.insert(props.id) {
            return invalid("Kalium layer tree contains duplicate node IDs");
        }
        max_id = max_id.max(props.id);
        if let SourceNode::Group { children, .. } = node {
            stack.extend(children.iter().map(|child| (child, depth + 1)));
        }
    }
    if !ids.contains(&tree.selected_node_id) || tree.next_node_id <= max_id {
        return invalid("Kalium layer tree selection or next-node counter is invalid");
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct SourceSetKey {
    id: u32,
    prefix: Option<u32>,
    size: u32,
}

fn collect_entries(
    nodes: &[SourceNode],
    set: &SourceSetKey,
    project_id: Uuid,
    depth: usize,
    count: &mut usize,
    entries: &mut BTreeSet<String>,
) -> Result<(), ProjectError> {
    if depth > MAX_KALIUM_DEPTH {
        return invalid(format!("layer tree exceeds {MAX_KALIUM_DEPTH} levels"));
    }
    for node in nodes {
        *count = count
            .checked_add(1)
            .ok_or_else(|| ProjectError::Invalid("layer node count overflow".into()))?;
        if *count > limits::MAX_ZIP_ENTRIES {
            return invalid("Kalium project layer nodes exceed the ZIP entry limit");
        }
        let props = properties(node);
        ensure_empty("layer node", &props.extensions)?;
        if props.id == 0
            || props.name.is_empty()
            || props.name.len() > 1024
            || !props.opacity.is_finite()
            || !(0.0..=1.0).contains(&props.opacity)
        {
            return invalid("Kalium layer node has invalid ID, name, or opacity");
        }
        let mask = entry_name(set, props.id, None);
        if !entries.insert(mask) {
            return invalid("Kalium layer tree has duplicate node image paths");
        }
        match node {
            SourceNode::Paint {
                channels,
                procedural_graph,
                ..
            } => {
                if let Some(source_graph) = procedural_graph {
                    // Validate graph size, node vocabulary, topology and finite parameters before
                    // any source PNG is decoded into its much larger editable raster form.
                    convert_procedural_graph(source_graph, project_id, set.id)?;
                }
                let mut seen = HashSet::new();
                for &channel in channels {
                    if !seen.insert(channel) {
                        return invalid("Kalium Paint node contains duplicate channels");
                    }
                    if !entries.insert(entry_name(set, props.id, Some(channel))) {
                        return invalid("Kalium Paint node has duplicate channel image paths");
                    }
                }
            }
            SourceNode::Fill { values, .. } => {
                if values.is_empty() {
                    return invalid("Kalium Fill node has no channel values");
                }
                let mut seen = HashSet::new();
                for value in values {
                    ensure_empty("fill value", &value.extensions)?;
                    if !seen.insert(value.channel) {
                        return invalid("Kalium Fill node contains duplicate channels");
                    }
                    if value.value[3] != 255 {
                        return invalid(
                            "Kalium Fill alpha is not fully opaque; SubCraft Fill has no per-channel alpha storage",
                        );
                    }
                    if value.channel.target().components() == 1
                        && !(value.value[0] == value.value[1] && value.value[1] == value.value[2])
                    {
                        return invalid(
                            "Kalium scalar Fill contains non-grayscale RGB data; SubCraft stores scalar channels as one component",
                        );
                    }
                }
            }
            SourceNode::Group { children, .. } => {
                collect_entries(children, set, project_id, depth + 1, count, entries)?
            }
        }
        if props
            .disabled_channels
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len()
            != props.disabled_channels.len()
        {
            return invalid("Kalium layer contains duplicate disabled channels");
        }
        if entries.len() > limits::MAX_ZIP_ENTRIES {
            return invalid("Kalium project has more referenced images than the ZIP entry limit");
        }
    }
    Ok(())
}

fn properties(node: &SourceNode) -> &SourceProperties {
    match node {
        SourceNode::Paint { properties, .. }
        | SourceNode::Fill { properties, .. }
        | SourceNode::Group { properties, .. } => properties,
    }
}

fn entry_name(set: &SourceSetKey, id: u32, channel: Option<SourceChannel>) -> String {
    let prefix = match set.prefix {
        Some(set_id) => format!("texture_sets/{set_id}/layers"),
        None => "layers".to_owned(),
    };
    let suffix = channel.map_or_else(
        || "mask.png".to_owned(),
        |channel| format!("{}.png", channel.filename()),
    );
    format!("{prefix}/{id}/{suffix}")
}

fn convert_procedural_graph(
    source: &SourceProceduralGraph,
    project_id: Uuid,
    set_id: u32,
) -> Result<ProceduralGraph, ProjectError> {
    if source.nodes.is_empty() || source.nodes.len() > 64 || source.edges.len() > 64 {
        return invalid("Kalium procedural graph exceeds SubCraft's node or edge limit");
    }
    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(source.nodes.len())
        .map_err(|_| ProjectError::Invalid("procedural graph allocation failed".into()))?;
    for node in &source.nodes {
        let kind = match node.kind {
            SourceProceduralNodeKind::Uv => ProceduralNodeKind::Uv,
            SourceProceduralNodeKind::ValueNoise { seed, scale } => {
                ProceduralNodeKind::ValueNoise { seed, scale }
            }
            SourceProceduralNodeKind::OcclusionInput { source_layer_id } => {
                if source_layer_id == 0 {
                    return invalid("Kalium procedural graph references layer ID zero");
                }
                ProceduralNodeKind::OcclusionInput {
                    source_layer_id: LayerId(stable_layer_id(
                        project_id,
                        set_id,
                        source_layer_id,
                    )),
                }
            }
            SourceProceduralNodeKind::Multiply => ProceduralNodeKind::Multiply,
            SourceProceduralNodeKind::RoughnessOutput => ProceduralNodeKind::RoughnessOutput,
        };
        nodes.push(ProceduralNode { id: node.id, kind });
    }
    let graph = ProceduralGraph {
        nodes,
        edges: source.edges.clone(),
    };
    graph
        .validate()
        .map_err(|error| ProjectError::Invalid(format!("Kalium procedural graph: {error}")))?;
    Ok(graph)
}

fn convert_nodes<R: Read + Seek>(
    nodes: &[SourceNode],
    set: &SourceSetKey,
    project_id: Uuid,
    zip: &mut ZipArchive<R>,
    directory: &limits::ZipDirectory,
    budget: &mut limits::ReadBudget,
    decoded_budget: &mut u64,
) -> Result<Vec<std::sync::Arc<subcraft_doc::Layer>>, ProjectError> {
    let mut out = Vec::new();
    out.try_reserve_exact(nodes.len())
        .map_err(|_| ProjectError::Invalid("layer allocation failed".into()))?;
    for node in nodes {
        let props = properties(node);
        let mask_bytes = (set.size as u64)
            .checked_mul(set.size as u64)
            .and_then(|v| v.checked_mul(2))
            .ok_or_else(|| ProjectError::Invalid("mask allocation size overflow".into()))?;
        add_decoded_budget(decoded_budget, mask_bytes)?;
        let mask_path = entry_name(set, props.id, None);
        let mask_image = read_bitmap(zip, directory, budget, &mask_path, set.size, 1)?;
        let mask = bitmap_to_mask(mask_image, set.size)?;
        let mut layer = match node {
            SourceNode::Paint {
                channels,
                procedural_graph,
                ..
            } => {
                let mut paint = PaintContent::default();
                for &channel in channels {
                    let kind = channel.target();
                    let components = kind.components();
                    let allocation = (set.size as u64)
                        .checked_mul(set.size as u64)
                        .and_then(|v| v.checked_mul((components as u64 + 1) * 2))
                        .ok_or_else(|| {
                            ProjectError::Invalid("paint allocation size overflow".into())
                        })?;
                    add_decoded_budget(decoded_budget, allocation)?;
                    let path = entry_name(set, props.id, Some(channel));
                    let bitmap = read_bitmap(zip, directory, budget, &path, set.size, 4)?;
                    paint
                        .channels
                        .insert(kind, bitmap_to_paint(bitmap, kind, set.size)?);
                }
                paint.procedural_graph = procedural_graph
                    .as_ref()
                    .map(|graph| convert_procedural_graph(graph, project_id, set.id))
                    .transpose()?;
                Layer {
                    id: LayerId(stable_layer_id(project_id, set.id, props.id)),
                    name: props.name.clone(),
                    visible: props.visible,
                    blend: BTreeMap::new(),
                    mask: None,
                    parts: None,
                    content: LayerContent::Paint(paint),
                    content_effects: Vec::new(),
                }
            }
            SourceNode::Fill { values, .. } => {
                let mut channels = BTreeMap::new();
                for value in values {
                    let kind = value.channel.target();
                    channels.insert(
                        kind,
                        FillChannel {
                            enabled: true,
                            source: FillSource::uniform(fill_channel_value(kind, value.value)),
                            strength: 1.0,
                        },
                    );
                }
                let mut layer =
                    Layer::new_fill(props.name.clone(), &TextureSet::new("", 0, set.size));
                layer.id = LayerId(stable_layer_id(project_id, set.id, props.id));
                layer.name = props.name.clone();
                layer.visible = props.visible;
                layer.blend = layer_blends(props.blend, &props.disabled_channels, props.opacity);
                layer.content = LayerContent::Fill(FillContent {
                    projection: Default::default(),
                    channels,
                });
                layer
            }
            SourceNode::Group { children, .. } => {
                let children = convert_nodes(
                    children,
                    set,
                    project_id,
                    zip,
                    directory,
                    budget,
                    decoded_budget,
                )?;
                let mut layer =
                    Layer::new_folder(props.name.clone(), &TextureSet::new("", 0, set.size));
                layer.id = LayerId(stable_layer_id(project_id, set.id, props.id));
                layer.name = props.name.clone();
                layer.visible = props.visible;
                layer.blend = layer_blends(props.blend, &props.disabled_channels, props.opacity);
                layer.content = LayerContent::Group(GroupContent {
                    children,
                    expanded: true,
                });
                layer
            }
        };
        if matches!(node, SourceNode::Paint { .. }) {
            layer.blend = layer_blends(props.blend, &props.disabled_channels, props.opacity);
        }
        layer.mask = Some(Mask {
            enabled: true,
            base: mask,
            effects: Vec::new(),
        });
        out.push(std::sync::Arc::new(layer));
    }
    Ok(out)
}

fn layer_blends(
    mode: source_blend::Mode,
    disabled: &[SourceChannel],
    opacity: f32,
) -> BTreeMap<ChannelKind, ChannelBlend> {
    ChannelKind::ALL
        .into_iter()
        .map(|channel| {
            let source = match channel {
                ChannelKind::BaseColor => SourceChannel::BaseColor,
                ChannelKind::Roughness => SourceChannel::Roughness,
                ChannelKind::Metallic => SourceChannel::Metallic,
                ChannelKind::Normal => SourceChannel::Normal,
                ChannelKind::Height => SourceChannel::Height,
                ChannelKind::Emissive => SourceChannel::Emissive,
                ChannelKind::Opacity => SourceChannel::Opacity,
                ChannelKind::AmbientOcclusion => SourceChannel::Occlusion,
                _ => return (channel, ChannelBlend::new(BlendMode::Disable)),
            };
            let mode = if disabled.contains(&source) {
                BlendMode::Disable
            } else {
                mode.target()
            };
            (channel, ChannelBlend { mode, opacity })
        })
        .collect()
}

fn collect_channels(roots: &[std::sync::Arc<subcraft_doc::Layer>]) -> BTreeSet<ChannelKind> {
    // Kalium returns a channel-specific default image even when the layer tree has no authored
    // pixels for that channel. An explicit editable Fill preserves those eight defaults.
    let mut out = BTreeSet::from([
        ChannelKind::BaseColor,
        ChannelKind::Roughness,
        ChannelKind::Metallic,
        ChannelKind::Normal,
        ChannelKind::Height,
        ChannelKind::Emissive,
        ChannelKind::Opacity,
        ChannelKind::AmbientOcclusion,
    ]);
    let mut stack: Vec<_> = roots.iter().collect();
    while let Some(layer) = stack.pop() {
        match &layer.content {
            LayerContent::Paint(paint) => out.extend(paint.channels.keys().copied()),
            LayerContent::Fill(fill) => out.extend(fill.channels.keys().copied()),
            LayerContent::Group(group) => stack.extend(group.children.iter()),
        }
    }
    out
}

fn fill_channel_value(kind: ChannelKind, value: [u8; 4]) -> ChannelValue {
    let mut rgb = [
        value[0] as f32 / 255.0,
        value[1] as f32 / 255.0,
        value[2] as f32 / 255.0,
    ];
    if kind.is_color() {
        rgb = rgb.map(subcraft_core::color::srgb_to_linear);
    } else if kind == ChannelKind::Height {
        rgb = rgb.map(|v| (v * 255.0 - 128.0) / 127.0);
    }
    if kind.components() == 1 {
        ChannelValue::scalar(rgb[0])
    } else {
        ChannelValue::rgb(rgb[0], rgb[1], rgb[2])
    }
}

fn add_decoded_budget(budget: &mut u64, bytes: u64) -> Result<(), ProjectError> {
    *budget = budget
        .checked_add(bytes)
        .ok_or_else(|| ProjectError::Invalid("decoded raster byte count overflow".into()))?;
    if *budget > IMPORT_RASTER_BUDGET {
        return invalid("Kalium decoded editable rasters exceed SubCraft's 512-MiB import budget");
    }
    Ok(())
}

fn read_bitmap<R: Read + Seek>(
    zip: &mut ZipArchive<R>,
    directory: &limits::ZipDirectory,
    budget: &mut limits::ReadBudget,
    path: &str,
    size: u32,
    channels: u8,
) -> Result<subcraft_image::Bitmap, ProjectError> {
    let decoded = (size as u64)
        .checked_mul(size as u64)
        .and_then(|v| v.checked_mul(channels as u64))
        .ok_or_else(|| ProjectError::Invalid("PNG decoded size overflow".into()))?;
    let entry_cap = decoded
        .checked_add(PNG_OVERHEAD)
        .ok_or_else(|| ProjectError::Invalid("PNG entry size overflow".into()))?;
    let bytes = limits::read_entry_bounded(zip, directory, path, entry_cap, budget)?;
    validate_png_header(&bytes, path, size, channels)?;
    let bitmap = subcraft_image::io::decode_bitmap(&bytes, Some(path))
        .map_err(|e| ProjectError::Invalid(format!("{path}: {e}")))?;
    if bitmap.width != size
        || bitmap.height != size
        || bitmap.channels != channels
        || !matches!(&bitmap.data, BitmapData::U8(_))
    {
        return invalid(format!(
            "{path}: expected {size}x{size} 8-bit image with {channels} channels"
        ));
    }
    Ok(bitmap)
}

/// Check PNG dimensions and representation before invoking the decoder, which otherwise allocates
/// based on untrusted IHDR dimensions. The writer emits only 8-bit Luma masks and RGBA paint maps.
fn validate_png_header(
    bytes: &[u8],
    path: &str,
    size: u32,
    channels: u8,
) -> Result<(), ProjectError> {
    let expected_color_type = if channels == 1 { 0 } else { 6 };
    if bytes.len() < 33
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || u32::from_be_bytes(bytes[8..12].try_into().expect("checked PNG header")) != 13
        || &bytes[12..16] != b"IHDR"
        || u32::from_be_bytes(bytes[16..20].try_into().expect("checked PNG header")) != size
        || u32::from_be_bytes(bytes[20..24].try_into().expect("checked PNG header")) != size
        || bytes[24] != 8
        || bytes[25] != expected_color_type
        || bytes[26..29] != [0, 0, 0]
    {
        return invalid(format!("{path}: unsupported PNG header or dimensions"));
    }
    Ok(())
}

fn bitmap_to_mask(bitmap: subcraft_image::Bitmap, size: u32) -> Result<TiledImage, ProjectError> {
    let BitmapData::U8(data) = bitmap.data else {
        return invalid("Kalium mask is not 8-bit grayscale");
    };
    let mut image = TiledImage::new(size, size, 1, [1.0, 0.0, 0.0, 0.0]);
    for (index, value) in data.into_iter().enumerate() {
        image.set(
            (index as u32) % size,
            (index as u32) / size,
            [value as f32 / 255.0, 0.0, 0.0, 0.0],
        );
    }
    Ok(image)
}

fn bitmap_to_paint(
    bitmap: subcraft_image::Bitmap,
    channel: ChannelKind,
    size: u32,
) -> Result<TiledImage, ProjectError> {
    let BitmapData::U8(data) = bitmap.data else {
        return invalid("Kalium paint image is not 8-bit RGBA");
    };
    let components = channel.components();
    let mut fill = [0.0f32; 4];
    let default = channel.default_value().0;
    fill[..components].copy_from_slice(&default[..components]);
    fill[components] = 0.0;
    let mut image = TiledImage::new(size, size, components + 1, fill);
    for (index, pixel) in data.chunks_exact(4).enumerate() {
        if components == 1 && !(pixel[0] == pixel[1] && pixel[1] == pixel[2]) {
            return invalid(format!(
                "Kalium {channel:?} paint image contains non-grayscale RGB data; SubCraft stores this channel as one component"
            ));
        }
        let mut values = [0.0f32; 4];
        for c in 0..components {
            let value = pixel[c] as f32 / 255.0;
            values[c] = if channel.is_color() {
                subcraft_core::color::srgb_to_linear(value)
            } else if channel == ChannelKind::Height {
                (pixel[c] as f32 - 128.0) / 127.0
            } else {
                value
            };
        }
        values[components] = pixel[3] as f32 / 255.0;
        image.set((index as u32) % size, (index as u32) / size, values);
    }
    Ok(image)
}

fn stable_set_id(project_id: Uuid, source_id: u32, material_slot: u32) -> TextureSetId {
    TextureSetId(Uuid::new_v5(
        &project_id,
        format!("subcraft/kalium/set/{source_id}/{material_slot}").as_bytes(),
    ))
}

fn stable_layer_id(project_id: Uuid, set_id: u32, source_id: u32) -> Uuid {
    Uuid::new_v5(
        &project_id,
        format!("subcraft/kalium/layer/{set_id}/{source_id}").as_bytes(),
    )
}

fn background_layer(
    project_id: Uuid,
    set_id: u32,
    size: u32,
    channels: &BTreeSet<ChannelKind>,
) -> Layer {
    let texture_set = TextureSet::new("Kalium background", 0, size);
    let mut layer = Layer::new_fill("Kalium Background", &texture_set);
    layer.id = LayerId(stable_layer_id(project_id, set_id, u32::MAX));
    layer.blend = layer_blends(source_blend::Mode::Normal, &[], 1.0);
    let mut fill_channels = BTreeMap::new();
    for &channel in channels {
        let bytes = match channel {
            ChannelKind::BaseColor => [190, 185, 202, 255],
            ChannelKind::Roughness => [170, 170, 170, 255],
            ChannelKind::Metallic | ChannelKind::Emissive => [0, 0, 0, 255],
            ChannelKind::Normal => [128, 128, 255, 255],
            ChannelKind::Height => [128, 128, 128, 255],
            ChannelKind::Opacity | ChannelKind::AmbientOcclusion => [255, 255, 255, 255],
            ChannelKind::Specular => [128, 128, 128, 255],
            ChannelKind::User0 | ChannelKind::User1 | ChannelKind::User2 | ChannelKind::User3 => {
                [0, 0, 0, 255]
            }
        };
        fill_channels.insert(
            channel,
            FillChannel {
                enabled: true,
                source: FillSource::uniform(fill_channel_value(channel, bytes)),
                strength: 1.0,
            },
        );
    }
    layer.content = LayerContent::Fill(FillContent {
        projection: Default::default(),
        channels: fill_channels,
    });
    layer
}

fn convert_mesh(source: SourceMesh) -> Result<Mesh, ProjectError> {
    if source.vertices.is_empty() || source.indices.is_empty() || source.indices.len() % 3 != 0 {
        return invalid("Kalium mesh has no triangles or an invalid index count");
    }
    if source.vertices.len() > 1_050_000 || source.indices.len() / 3 > 350_000 {
        return invalid("Kalium mesh exceeds SubCraft's import geometry limits");
    }
    if source.materials.is_empty() {
        return invalid("Kalium mesh has no named material slots");
    }
    let triangle_count = source.indices.len() / 3;
    let tri_material = if source.triangle_materials.is_empty() {
        vec![0; triangle_count]
    } else {
        if source.triangle_materials.len() != triangle_count {
            return invalid("Kalium mesh triangle material count is invalid");
        }
        source.triangle_materials
    };
    if tri_material
        .iter()
        .any(|&slot| slot as usize >= source.materials.len())
    {
        return invalid("Kalium mesh references an unknown material slot");
    }
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut uvs = Vec::new();
    let mut tangents = Vec::new();
    positions
        .try_reserve_exact(source.vertices.len())
        .map_err(|_| ProjectError::Invalid("mesh position allocation failed".into()))?;
    normals
        .try_reserve_exact(source.vertices.len())
        .map_err(|_| ProjectError::Invalid("mesh normal allocation failed".into()))?;
    uvs.try_reserve_exact(source.vertices.len())
        .map_err(|_| ProjectError::Invalid("mesh UV allocation failed".into()))?;
    tangents
        .try_reserve_exact(source.vertices.len())
        .map_err(|_| ProjectError::Invalid("mesh tangent allocation failed".into()))?;
    for vertex in &source.vertices {
        let position = Vec3::from_array(vertex.position);
        let normal = Vec3::from_array(vertex.normal);
        let uv = Vec2::from_array(vertex.uv);
        if !position.is_finite()
            || !normal.is_finite()
            || !uv.is_finite()
            || normal.length_squared() < 1e-12
        {
            return invalid("Kalium mesh contains invalid vertex attributes");
        }
        positions.push(position);
        normals.push(normal.normalize());
        uvs.push(uv);
        tangents.push(vertex.tangent.map(Vec4::from_array).unwrap_or(Vec4::ZERO));
    }
    if source
        .indices
        .iter()
        .any(|&index| index as usize >= source.vertices.len())
    {
        return invalid("Kalium mesh index is out of range");
    }
    if tangents.iter().any(|t| *t == Vec4::ZERO) {
        let mut corner_positions = Vec::new();
        let mut corner_normals = Vec::new();
        let mut corner_uvs = Vec::new();
        corner_positions
            .try_reserve_exact(source.indices.len())
            .map_err(|_| ProjectError::Invalid("mesh tangent-input allocation failed".into()))?;
        corner_normals
            .try_reserve_exact(source.indices.len())
            .map_err(|_| ProjectError::Invalid("mesh tangent-input allocation failed".into()))?;
        corner_uvs
            .try_reserve_exact(source.indices.len())
            .map_err(|_| ProjectError::Invalid("mesh tangent-input allocation failed".into()))?;
        for &index in &source.indices {
            let i = index as usize;
            corner_positions.push(positions[i]);
            corner_normals.push(normals[i]);
            corner_uvs.push(uvs[i]);
        }
        let corner_tangents = subcraft_mesh::tangent::compute_corner_tangents(
            &corner_positions,
            &corner_normals,
            &corner_uvs,
        );
        let mut assigned = vec![false; tangents.len()];
        for (&index, tangent) in source.indices.iter().zip(corner_tangents) {
            let i = index as usize;
            if !assigned[i] && tangents[i] == Vec4::ZERO {
                tangents[i] = tangent;
                assigned[i] = true;
            }
        }
    }
    if tangents
        .iter()
        .any(|t| !t.is_finite() || t.truncate().length_squared() < 1e-12)
    {
        return invalid("Kalium mesh has invalid tangent data");
    }
    let mut triangle_order: Vec<usize> = (0..triangle_count).collect();
    triangle_order.sort_by_key(|&triangle| tri_material[triangle]);
    let mut indices = Vec::new();
    let mut sorted_materials = Vec::new();
    indices
        .try_reserve_exact(source.indices.len())
        .map_err(|_| ProjectError::Invalid("mesh index allocation failed".into()))?;
    sorted_materials
        .try_reserve_exact(triangle_count)
        .map_err(|_| ProjectError::Invalid("mesh material allocation failed".into()))?;
    for triangle in triangle_order {
        indices.extend_from_slice(&source.indices[triangle * 3..triangle * 3 + 3]);
        sorted_materials.push(tri_material[triangle]);
    }
    let mut submeshes = Vec::new();
    let mut start = 0usize;
    while start < sorted_materials.len() {
        let material = sorted_materials[start];
        let mut end = start + 1;
        while end < sorted_materials.len() && sorted_materials[end] == material {
            end += 1;
        }
        submeshes.push(Submesh {
            material,
            first_triangle: start as u32,
            triangle_count: (end - start) as u32,
        });
        start = end;
    }
    let materials = source
        .materials
        .into_iter()
        .map(|name| MaterialSlot { name })
        .collect();
    let mesh = Mesh {
        name: source.name,
        bounds: Aabb::from_points(positions.iter().copied()),
        positions,
        normals,
        uvs,
        tangents,
        indices,
        tri_material: sorted_materials,
        tri_object: vec![None; triangle_count],
        objects: Vec::<MeshObject>::new(),
        materials,
        submeshes,
    };
    let problems = mesh.validate();
    if !problems.is_empty() {
        return invalid(format!(
            "Kalium mesh cannot be represented by SubCraft: {}",
            problems.join("; ")
        ));
    }
    Ok(mesh)
}

#[cfg(test)]
#[path = "kalium_tests.rs"]
mod tests;
