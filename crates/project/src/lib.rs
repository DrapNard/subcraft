//! Project files (`.subcraft`): a zip container with a versioned logical schema.
//!
//! ```text
//! manifest.json        format name + schema version (read first, gates everything else)
//! mesh.bin             mesh::serialize blob
//! document.json        schema v1: texture sets, layer tree, masks, raster references
//! rasters/NNNNN.tiles  one sparse TiledImage per paint channel / mask base / mask paint effect
//! resources/<id>.<ext> original bytes of imported bitmaps referenced by fills
//! ```
//!
//! Saving is atomic: the container is written to a temporary file next to the target, synced,
//! then renamed over it, so a crash never leaves a half-written project.

#![cfg_attr(test, recursion_limit = "256")]

pub mod fragment;
pub mod limits;
pub mod raster;
pub mod schema;
pub mod spp_checksum;
#[cfg(all(feature = "spp-hdf5", not(target_arch = "wasm32")))]
pub mod spp_container;
#[cfg(all(feature = "spp-worker", not(target_arch = "wasm32")))]
pub mod spp_worker;
pub mod spp_document;
pub mod spp_semantics;
pub mod spp_fill;
pub mod spp_scene;
pub mod spp_scene_node;
pub mod spp_scene_material;
pub mod spp_scene_mesh;

use std::collections::BTreeMap;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use subcraft_core::{EffectId, LayerId, ResourceId, TextureSetId};
use subcraft_doc::{
    ChannelBlend, ChannelDef, ContentEffect, ContentEffectKind, Document, FillChannel, FillContent,
    GroupContent, Layer, LayerContent, Mask, MaskEffect, MaskEffectKind, PaintContent, PartMask,
    Projection, Resource, ResourceStore, TextureSet,
};
use subcraft_image::TiledImage;
use subcraft_mesh::Mesh;
use zip::write::SimpleFileOptions;

use schema::*;

pub const EXTENSION: &str = "subcraft";

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a valid project container: {0}")]
    Container(String),
    #[error("not a SubCraft project")]
    NotAProject,
    #[error(
        "project was saved by a newer SubCraft (schema {0}, this version reads up to {SCHEMA_VERSION})"
    )]
    UnsupportedVersion(u32),
    #[error("project data is invalid: {0}")]
    Invalid(String),
}

impl From<zip::result::ZipError> for ProjectError {
    fn from(e: zip::result::ZipError) -> Self {
        ProjectError::Container(e.to_string())
    }
}

/// Baked mesh maps per texture set.
pub type MeshMapSet = BTreeMap<TextureSetId, BTreeMap<subcraft_core::MeshMapKind, TiledImage>>;

/// Everything a project file holds.
#[derive(Debug)]
pub struct ProjectData {
    pub mesh: Mesh,
    pub document: Document,
    pub resources: ResourceStore,
    pub mesh_maps: MeshMapSet,
}

// ----- save ----------------------------------------------------------------------------------

struct Writer {
    rasters: Vec<(String, Vec<u8>)>,
}

impl Writer {
    fn raster(&mut self, img: &TiledImage) -> String {
        let name = format!("rasters/{:05}.tiles", self.rasters.len());
        self.rasters.push((name.clone(), raster::encode(img)));
        name
    }

    fn mask_effect(&mut self, e: &MaskEffect) -> MaskEffectV1 {
        MaskEffectV1 {
            id: e.id.0,
            name: e.name.clone(),
            visible: e.visible,
            blend: e.blend,
            opacity: e.opacity,
            kind: match &e.kind {
                MaskEffectKind::Paint { image } => MaskEffectKindV1::Paint {
                    raster: self.raster(image),
                },
                MaskEffectKind::Fill {
                    value,
                    procedural,
                    projection,
                } => MaskEffectKindV1::Fill {
                    value: *value,
                    procedural: *procedural,
                    projection: *projection,
                },
                MaskEffectKind::Generator(g) => MaskEffectKindV1::Generator { generator: *g },
                MaskEffectKind::Levels(l) => MaskEffectKindV1::Levels { levels: *l },
                MaskEffectKind::Invert => MaskEffectKindV1::Invert,
                MaskEffectKind::Image {
                    resource,
                    projection,
                    invert,
                } => MaskEffectKindV1::Image {
                    resource: resource.0,
                    projection: *projection,
                    invert: *invert,
                },
            },
        }
    }

    fn paint_channels(
        &mut self,
        channels: &BTreeMap<subcraft_core::ChannelKind, TiledImage>,
    ) -> Vec<PaintChannelV1> {
        channels
            .iter()
            .map(|(&channel, img)| PaintChannelV1 {
                channel,
                raster: self.raster(img),
            })
            .collect()
    }

    fn content_effect(&mut self, e: &ContentEffect) -> ContentEffectV5 {
        ContentEffectV5 {
            id: e.id.0,
            name: e.name.clone(),
            visible: e.visible,
            blend: blends(&e.blend),
            kind: match &e.kind {
                ContentEffectKind::Paint { channels } => ContentEffectKindV5::Paint {
                    channels: self.paint_channels(channels),
                },
                ContentEffectKind::Fill(f) => ContentEffectKindV5::Fill {
                    projection: f.projection,
                    channels: fill_channels(f),
                },
                ContentEffectKind::Levels { channel, levels } => ContentEffectKindV5::Levels {
                    channel: *channel,
                    levels: *levels,
                },
            },
        }
    }

    fn layer(&mut self, l: &Layer) -> LayerV1 {
        let blend = blends(&l.blend);
        let mask = l.mask.as_ref().map(|m| MaskV1 {
            enabled: m.enabled,
            base: self.raster(&m.base),
            effects: m.effects.iter().map(|e| self.mask_effect(e)).collect(),
        });
        let content = match &l.content {
            LayerContent::Paint(p) => ContentV1::Paint {
                channels: self.paint_channels(&p.channels),
                procedural_graph: p.procedural_graph.clone(),
                path: p.path.as_ref().map(|path| PenPathV4 {
                    anchors: path
                        .anchors
                        .iter()
                        .map(|a| PenAnchorV4 {
                            id: a.id.0,
                            position: a.position.to_array(),
                            incoming: a.incoming.to_array(),
                            outgoing: a.outgoing.to_array(),
                        })
                        .collect(),
                    closed: path.closed,
                    radius: path.radius,
                    hardness: path.hardness,
                    opacity: path.opacity,
                    values: path
                        .values
                        .iter()
                        .map(|(&channel, value)| PenChannelV4 {
                            channel,
                            value: value.0,
                        })
                        .collect(),
                }),
            },
            LayerContent::Fill(f) => ContentV1::Fill {
                projection: f.projection,
                channels: fill_channels(f),
            },
            LayerContent::Group(g) => ContentV1::Folder {
                expanded: g.expanded,
                children: g.children.iter().map(|c| self.layer(c)).collect(),
            },
        };
        let parts = l.parts.as_ref().map(|p| PartsV1 {
            enabled: p.enabled,
            kind: p.kind,
            parts: p.parts.iter().copied().collect(),
        });
        let content_effects = l
            .content_effects
            .iter()
            .map(|e| self.content_effect(e))
            .collect();
        LayerV1 {
            id: l.id.0,
            name: l.name.clone(),
            visible: l.visible,
            blend,
            mask,
            parts,
            content,
            content_effects,
        }
    }
}

fn blends(blend: &BTreeMap<subcraft_core::ChannelKind, ChannelBlend>) -> Vec<BlendV1> {
    blend
        .iter()
        .map(|(&channel, b)| BlendV1 {
            channel,
            mode: b.mode,
            opacity: b.opacity,
        })
        .collect()
}

fn fill_channels(f: &FillContent) -> Vec<FillChannelV1> {
    f.channels
        .iter()
        .map(|(&channel, c)| FillChannelV1 {
            channel,
            enabled: c.enabled,
            source: c.source.clone(),
            strength: c.strength,
        })
        .collect()
}

fn read_fill(projection: Projection, channels: &[FillChannelV1]) -> FillContent {
    FillContent {
        projection,
        channels: channels
            .iter()
            .map(|c| {
                (
                    c.channel,
                    FillChannel {
                        enabled: c.enabled,
                        source: c.source.clone(),
                        strength: c.strength,
                    },
                )
            })
            .collect(),
    }
}

fn referenced_resources(doc: &Document) -> Vec<ResourceId> {
    let mut out = Vec::new();
    for ts in &doc.texture_sets {
        for l in &ts.layers {
            l.collect_resources(&mut out);
        }
    }
    out
}

/// Write a project container to any seekable writer (files, memory buffers in tests).
pub fn write_project<W: Write + Seek>(
    w: W,
    mesh: &Mesh,
    doc: &Document,
    resources: &ResourceStore,
    mesh_maps: &MeshMapSet,
) -> Result<W, ProjectError> {
    let mut writer = Writer {
        rasters: Vec::new(),
    };
    let texture_sets = doc
        .texture_sets
        .iter()
        .map(|ts| TextureSetV1 {
            id: ts.id.0,
            name: ts.name.clone(),
            material_slot: ts.material_slot,
            resolution: ts.resolution,
            channels: ts
                .channels
                .iter()
                .map(|c| ChannelDefV1 {
                    kind: c.kind,
                    format: c.format,
                })
                .collect(),
            layers: ts.layers.iter().map(|l| writer.layer(l)).collect(),
            mesh_maps: mesh_maps
                .get(&ts.id)
                .map(|m| {
                    m.iter()
                        .map(|(&kind, img)| MeshMapV1 {
                            kind,
                            raster: writer.raster(img),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    let (res_entries, res_files) = pack_resources(&referenced_resources(doc), resources);
    let document = DocumentV1 {
        texture_sets,
        resources: res_entries,
    };
    let manifest = Manifest {
        format: FORMAT_NAME.into(),
        schema_version: SCHEMA_VERSION,
        writer: format!("SubCraft {}", env!("CARGO_PKG_VERSION")),
        mesh: "mesh.bin".into(),
        document: "document.json".into(),
    };

    let mut zip = zip::ZipWriter::new(w);
    let deflate = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("manifest.json", deflate)?;
    zip.write_all(to_json(&manifest).as_bytes())?;
    zip.start_file("document.json", deflate)?;
    zip.write_all(to_json(&document).as_bytes())?;
    zip.start_file("mesh.bin", deflate)?;
    zip.write_all(&subcraft_mesh::serialize::encode(mesh))?;
    write_payload(&mut zip, &writer.rasters, &res_files)?;
    Ok(zip.finish()?)
}

/// Resource table entries + file payloads for `ids` (resources without original bytes are skipped).
fn pack_resources(
    ids: &[ResourceId],
    resources: &ResourceStore,
) -> (Vec<ResourceV1>, Vec<(String, Arc<Vec<u8>>)>) {
    let mut files = Vec::new();
    let mut entries = Vec::new();
    for &id in ids {
        let Some(r) = resources.get(id) else { continue };
        let Some(bytes) = &r.encoded else {
            log::warn!("resource {} has no original file bytes; not saved", r.name);
            continue;
        };
        // By content, so re-saving a loaded project keeps the extension (TGA falls back to the name).
        let hint = r
            .source_path
            .as_deref()
            .and_then(|p| p.to_str())
            .unwrap_or(&r.name);
        let ext = subcraft_image::guess_extension(bytes, Some(hint));
        let file = format!("resources/{}.{ext}", id.0);
        entries.push(ResourceV1 {
            id: id.0,
            name: r.name.clone(),
            kind: r.kind,
            file: file.clone(),
        });
        files.push((file, bytes.clone()));
    }
    (entries, files)
}

/// Raster blobs (deflated) and resource files (stored: already compressed image formats).
fn write_payload<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    rasters: &[(String, Vec<u8>)],
    resources: &[(String, Arc<Vec<u8>>)],
) -> Result<(), ProjectError> {
    let deflate = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in rasters {
        zip.start_file(name.as_str(), deflate)?;
        zip.write_all(bytes)?;
    }
    for (name, bytes) in resources {
        zip.start_file(
            name.as_str(),
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )?;
        zip.write_all(bytes)?;
    }
    Ok(())
}

fn unpack_resources<R: Read + Seek>(
    loader: &mut Loader<'_, R>,
    entries: &[ResourceV1],
) -> Result<ResourceStore, ProjectError> {
    let mut resources = ResourceStore::default();
    let mut resource_bytes = 0u64;
    let mut decoded_resource_bytes = 0u64;
    for r in entries {
        limits::validate_entry_name(&r.file)?;
        resource_bytes = resource_bytes
            .checked_add(loader.entry_size(&r.file)?)
            .ok_or_else(|| ProjectError::Invalid("resource byte count overflow".into()))?;
        if resource_bytes > limits::MAX_TOTAL_RESOURCE_BYTES {
            return Err(ProjectError::Invalid(
                "project resources exceed the aggregate byte limit".into(),
            ));
        }
        let bytes = loader.bytes(&r.file, limits::MAX_RESOURCE_BYTES)?;
        let decoded_cost = subcraft_image::io::bitmap_decoded_cost(
            &bytes,
            Some(&r.file),
            limits::MAX_RASTER_DIMENSION,
        )
        .map_err(|e| ProjectError::Invalid(format!("resource {}: {e}", r.name)))?;
        let remaining = limits::MAX_TOTAL_RESOURCE_DECODED_BYTES
            .saturating_sub(decoded_resource_bytes)
            .min(limits::MAX_RESOURCE_DECODED_BYTES);
        if decoded_cost > remaining {
            return Err(ProjectError::Invalid(
                "project decoded resources exceed the aggregate memory limit".into(),
            ));
        }
        let bitmap = subcraft_image::io::decode_bitmap_bounded(
            &bytes,
            Some(&r.file),
            limits::MAX_RASTER_DIMENSION,
            remaining,
        )
        .map_err(|e| ProjectError::Invalid(format!("resource {}: {e}", r.name)))?;
        decoded_resource_bytes += decoded_cost;
        resources.insert(Resource {
            id: ResourceId(r.id),
            name: r.name.clone(),
            kind: r.kind,
            bitmap: Arc::new(bitmap),
            source_path: None,
            encoded: Some(Arc::new(bytes)),
        });
    }
    Ok(resources)
}

fn to_json<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).expect("schema types serialize")
}

/// Save atomically to `path` (temp file + rename).
#[cfg(not(target_arch = "wasm32"))]
pub fn save_project(
    path: &Path,
    mesh: &Mesh,
    doc: &Document,
    resources: &ResourceStore,
    mesh_maps: &MeshMapSet,
) -> Result<(), ProjectError> {
    reject_spp_save_path(path)?;
    let (tmp, file) = create_staging_file(path)?;
    let result =
        write_stage_and_replace(tmp.as_path(), path, file, mesh, doc, resources, mesh_maps);
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Web build: encode in memory and hand the file to `vfs` (which offers it as a download).
#[cfg(target_arch = "wasm32")]
pub fn save_project(
    path: &Path,
    mesh: &Mesh,
    doc: &Document,
    resources: &ResourceStore,
    mesh_maps: &MeshMapSet,
) -> Result<(), ProjectError> {
    reject_spp_save_path(path)?;
    let buf = write_project(
        std::io::Cursor::new(Vec::new()),
        mesh,
        doc,
        resources,
        mesh_maps,
    )?;
    subcraft_core::vfs::write(path, &buf.into_inner())?;
    Ok(())
}

fn reject_spp_save_path(path: &Path) -> Result<(), ProjectError> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("uspp"))
    {
        return Err(ProjectError::Invalid(
            "Universal SPP .uspp support follows native SPP integration; not implemented".into(),
        ));
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "kalium" | "kbrush"))
    {
        return Err(ProjectError::Invalid(
            "Kalium formats are not supported".into(),
        ));
    }
    if path
        .extension()
        .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("spp"))
    {
        return Err(ProjectError::Invalid(
            "Substance Painter .spp export is not implemented".into(),
        ));
    }
    Ok(())
}

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
fn create_staging_file(path: &Path) -> Result<(PathBuf, std::fs::File), ProjectError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_STAGE: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = path.file_name().unwrap_or_default().to_string_lossy();
    for _ in 0..128 {
        let nonce = NEXT_STAGE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{stem}.{}.{}.saving", std::process::id(), nonce));
        match std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(ProjectError::Io(error)),
        }
    }
    Err(ProjectError::Invalid(
        "could not create a unique staging file".into(),
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn write_stage_and_replace(
    tmp: &Path,
    target: &Path,
    file: std::fs::File,
    mesh: &Mesh,
    doc: &Document,
    resources: &ResourceStore,
    mesh_maps: &MeshMapSet,
) -> Result<(), ProjectError> {
    let buf = write_project(
        std::io::BufWriter::new(file),
        mesh,
        doc,
        resources,
        mesh_maps,
    )?;
    let file = buf
        .into_inner()
        .map_err(|e| ProjectError::Io(e.into_error()))?;
    file.sync_all()?;
    drop(file);
    replace_file(tmp, target)?;
    sync_parent_directory(target)?;
    Ok(())
}

#[cfg(unix)]
fn replace_file(staged: &Path, target: &Path) -> Result<(), ProjectError> {
    // `rename` installs the staged inode, so it would otherwise replace the target's
    // permissions with the staging file's umask-derived permissions. Preserve the mode
    // of an existing regular file before the atomic commit.
    match std::fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_file() => {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                staged,
                std::fs::Permissions::from_mode(metadata.permissions().mode()),
            )?;
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ProjectError::Io(error)),
    }
    std::fs::rename(staged, target)?;
    Ok(())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn replace_file(staged: &Path, target: &Path) -> Result<(), ProjectError> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let target_w: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
    let staged_w: Vec<u16> = staged.as_os_str().encode_wide().chain(Some(0)).collect();
    if target_w[..target_w.len() - 1].contains(&0) || staged_w[..staged_w.len() - 1].contains(&0) {
        return Err(ProjectError::Invalid(
            "project path contains a null character".into(),
        ));
    }
    let replaced = unsafe {
        ReplaceFileW(
            target_w.as_ptr(),
            staged_w.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if replaced != 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        let moved = unsafe { MoveFileExW(staged_w.as_ptr(), target_w.as_ptr(), 0x1 | 0x8) };
        if moved != 0 {
            return Ok(());
        }
        return Err(ProjectError::Io(std::io::Error::last_os_error()));
    }
    Err(ProjectError::Io(error))
}

#[cfg(not(any(unix, windows)))]
fn replace_file(staged: &Path, target: &Path) -> Result<(), ProjectError> {
    if target.exists() {
        return Err(ProjectError::Invalid(
            "atomic project replacement is unsupported on this platform".into(),
        ));
    }
    std::fs::rename(staged, target)?;
    Ok(())
}

#[cfg(unix)]
fn sync_parent_directory(target: &Path) -> Result<(), ProjectError> {
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_parent_directory(_target: &Path) -> Result<(), ProjectError> {
    Ok(())
}

// ----- load ----------------------------------------------------------------------------------

struct Loader<'a, R: Read + Seek> {
    zip: &'a mut zip::ZipArchive<R>,
    directory: &'a limits::ZipDirectory,
    read_budget: limits::ReadBudget,
    cache: BTreeMap<String, TiledImage>,
    raster_cost: u64,
}

impl<R: Read + Seek> Loader<'_, R> {
    fn entry_size(&self, name: &str) -> Result<u64, ProjectError> {
        limits::validate_entry_name(name)?;
        self.directory
            .entries
            .get(name)
            .map(|e| e.uncompressed_size)
            .ok_or_else(|| ProjectError::Invalid(format!("missing project entry {name}")))
    }

    fn bytes(&mut self, name: &str, cap: u64) -> Result<Vec<u8>, ProjectError> {
        limits::read_entry_bounded(self.zip, self.directory, name, cap, &mut self.read_budget)
    }

    fn raster(&mut self, name: &str, resolution: u32) -> Result<TiledImage, ProjectError> {
        if let Some(img) = self.cache.get(name) {
            return Ok(img.clone());
        }
        let img = self.decode_raster(name, Some(resolution))?;
        self.cache.insert(name.to_string(), img.clone());
        Ok(img)
    }

    fn decode_raster(
        &mut self,
        name: &str,
        expected_resolution: Option<u32>,
    ) -> Result<TiledImage, ProjectError> {
        let bytes = self.bytes(name, limits::MAX_RASTER_BYTES)?;
        let cost = raster::decoded_cost(&bytes)
            .map_err(|e| ProjectError::Invalid(format!("{name}: {e}")))?;
        let next_cost = self
            .raster_cost
            .checked_add(cost)
            .ok_or_else(|| ProjectError::Invalid("decoded raster cost overflow".into()))?;
        if next_cost > limits::MAX_TOTAL_RASTER_BYTES {
            return Err(ProjectError::Invalid(
                "project decoded rasters exceed the aggregate memory limit".into(),
            ));
        }
        if bytes.len() < 16 {
            return Err(ProjectError::Invalid(format!(
                "{name}: truncated raster header"
            )));
        }
        let width = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let height = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if width > limits::MAX_RASTER_DIMENSION || height > limits::MAX_RASTER_DIMENSION {
            return Err(ProjectError::Invalid(format!(
                "{name}: raster dimensions exceed the project limit"
            )));
        }
        if expected_resolution.is_some_and(|resolution| width != resolution || height != resolution)
        {
            return Err(ProjectError::Invalid(format!(
                "{name}: {width}x{height} does not match resolution {}",
                expected_resolution.unwrap()
            )));
        }
        let img =
            raster::decode(&bytes).map_err(|e| ProjectError::Invalid(format!("{name}: {e}")))?;
        self.raster_cost = next_cost;
        Ok(img)
    }

    fn mask_effect(&mut self, e: &MaskEffectV1, res: u32) -> Result<MaskEffect, ProjectError> {
        let kind = match &e.kind {
            MaskEffectKindV1::Paint { raster } => MaskEffectKind::Paint {
                image: self.raster(raster, res)?,
            },
            MaskEffectKindV1::Fill {
                value,
                procedural,
                projection,
            } => MaskEffectKind::Fill {
                value: *value,
                procedural: *procedural,
                projection: *projection,
            },
            MaskEffectKindV1::Generator { generator } => MaskEffectKind::Generator(*generator),
            MaskEffectKindV1::Levels { levels } => MaskEffectKind::Levels(*levels),
            MaskEffectKindV1::Invert => MaskEffectKind::Invert,
            MaskEffectKindV1::Image {
                resource,
                projection,
                invert,
            } => MaskEffectKind::Image {
                resource: ResourceId(*resource),
                projection: *projection,
                invert: *invert,
            },
        };
        Ok(MaskEffect {
            id: EffectId(e.id),
            name: e.name.clone(),
            visible: e.visible,
            blend: e.blend,
            opacity: e.opacity,
            kind,
        })
    }

    fn paint_channels(
        &mut self,
        channels: &[PaintChannelV1],
        res: u32,
    ) -> Result<BTreeMap<subcraft_core::ChannelKind, TiledImage>, ProjectError> {
        let mut out = BTreeMap::new();
        for c in channels {
            let img = self.raster(&c.raster, res)?;
            if img.comps() != c.channel.components() + 1 {
                return Err(ProjectError::Invalid(format!(
                    "{}: wrong component count for {:?}",
                    c.raster, c.channel
                )));
            }
            out.insert(c.channel, img);
        }
        Ok(out)
    }

    fn content_effect(
        &mut self,
        e: &ContentEffectV5,
        res: u32,
    ) -> Result<ContentEffect, ProjectError> {
        let kind = match &e.kind {
            ContentEffectKindV5::Paint { channels } => ContentEffectKind::Paint {
                channels: self.paint_channels(channels, res)?,
            },
            ContentEffectKindV5::Fill {
                projection,
                channels,
            } => ContentEffectKind::Fill(read_fill(*projection, channels)),
            ContentEffectKindV5::Levels { channel, levels } => {
                if *channel == subcraft_core::ChannelKind::Normal {
                    return Err(ProjectError::Invalid(
                        "levels effect on the normal channel".into(),
                    ));
                }
                ContentEffectKind::Levels {
                    channel: *channel,
                    levels: *levels,
                }
            }
        };
        let blend = e
            .blend
            .iter()
            .map(|b| {
                (
                    b.channel,
                    ChannelBlend {
                        mode: b.mode,
                        opacity: b.opacity,
                    },
                )
            })
            .collect();
        Ok(ContentEffect {
            id: EffectId(e.id),
            name: e.name.clone(),
            visible: e.visible,
            blend,
            kind,
        })
    }

    fn layer(&mut self, l: &LayerV1, res: u32) -> Result<Layer, ProjectError> {
        let mask = match &l.mask {
            None => None,
            Some(m) => {
                let base = self.raster(&m.base, res)?;
                if base.comps() != 1 {
                    return Err(ProjectError::Invalid(
                        "mask base must have 1 component".into(),
                    ));
                }
                let mut effects = Vec::new();
                for e in &m.effects {
                    effects.push(Arc::new(self.mask_effect(e, res)?));
                }
                Some(Mask {
                    enabled: m.enabled,
                    base,
                    effects,
                })
            }
        };
        let content = match &l.content {
            ContentV1::Paint {
                channels,
                path,
                procedural_graph,
            } => {
                let mut p = PaintContent::default();
                if let Some(path) = path {
                    let source = subcraft_doc::PenPath {
                        anchors: path
                            .anchors
                            .iter()
                            .map(|a| subcraft_doc::PenAnchor {
                                id: subcraft_core::AnchorId(a.id),
                                position: a.position.into(),
                                incoming: a.incoming.into(),
                                outgoing: a.outgoing.into(),
                            })
                            .collect(),
                        closed: path.closed,
                        radius: path.radius,
                        hardness: path.hardness,
                        opacity: path.opacity,
                        values: path
                            .values
                            .iter()
                            .map(|v| (v.channel, subcraft_core::ChannelValue(v.value)))
                            .collect(),
                    };
                    source
                        .validate()
                        .map_err(|e| ProjectError::Invalid(e.into()))?;
                    if source.values.len() != path.values.len() {
                        return Err(ProjectError::Invalid("duplicate pen channels".into()));
                    }
                    p.path = Some(source);
                }
                p.channels = self.paint_channels(channels, res)?;
                if let Some(graph) = procedural_graph {
                    graph
                        .validate()
                        .map_err(|error| ProjectError::Invalid(error.to_string()))?;
                }
                p.procedural_graph = procedural_graph.clone();
                LayerContent::Paint(p)
            }
            ContentV1::Fill {
                projection,
                channels,
            } => LayerContent::Fill(read_fill(*projection, channels)),
            ContentV1::Folder { expanded, children } => LayerContent::Group(GroupContent {
                expanded: *expanded,
                children: children
                    .iter()
                    .map(|c| self.layer(c, res).map(Arc::new))
                    .collect::<Result<_, _>>()?,
            }),
        };
        if matches!(content, LayerContent::Group(_)) && !l.content_effects.is_empty() {
            return Err(ProjectError::Invalid("content effects on a folder".into()));
        }
        let content_effects = l
            .content_effects
            .iter()
            .map(|e| self.content_effect(e, res).map(Arc::new))
            .collect::<Result<_, _>>()?;
        Ok(Layer {
            id: LayerId(l.id),
            name: l.name.clone(),
            visible: l.visible,
            blend: l
                .blend
                .iter()
                .map(|b| {
                    (
                        b.channel,
                        ChannelBlend {
                            mode: b.mode,
                            opacity: b.opacity,
                        },
                    )
                })
                .collect(),
            mask,
            parts: l.parts.as_ref().map(|p| PartMask {
                enabled: p.enabled,
                kind: p.kind,
                parts: p.parts.iter().copied().collect(),
            }),
            content,
            content_effects,
        })
    }
}

fn validate_document_references(
    doc: &DocumentV1,
    directory: &limits::ZipDirectory,
) -> Result<(), ProjectError> {
    use std::collections::HashSet;
    if doc.texture_sets.len() > limits::MAX_TEXTURE_SETS {
        return Err(ProjectError::Invalid(
            "project contains too many texture sets".into(),
        ));
    }
    let mut resource_ids = HashSet::new();
    let mut resource_paths = HashSet::new();
    for resource in &doc.resources {
        limits::validate_entry_name(&resource.file)?;
        if !resource.file.starts_with("resources/")
            || !directory.entries.contains_key(&resource.file)
        {
            return Err(ProjectError::Invalid(format!(
                "resource {} refers to a missing or invalid file",
                resource.name
            )));
        }
        if !resource_ids.insert(resource.id) || !resource_paths.insert(resource.file.as_str()) {
            return Err(ProjectError::Invalid(
                "project contains duplicate resource identifiers or paths".into(),
            ));
        }
    }
    let mut referenced_resource_ids = HashSet::new();
    let mut nodes = 0usize;
    let mut stack = Vec::new();
    for texture_set in &doc.texture_sets {
        if !subcraft_doc::RESOLUTIONS.contains(&texture_set.resolution) {
            return Err(ProjectError::Invalid(format!(
                "texture set {}: unsupported resolution {}",
                texture_set.name, texture_set.resolution
            )));
        }
        for layer in &texture_set.layers {
            stack.push((layer, 1usize));
        }
        for map in &texture_set.mesh_maps {
            validate_raster_reference(&map.raster, directory)?;
        }
    }
    while let Some((layer, depth)) = stack.pop() {
        nodes = nodes
            .checked_add(1)
            .ok_or_else(|| ProjectError::Invalid("layer count overflow".into()))?;
        if nodes > limits::MAX_LAYER_NODES || depth > limits::MAX_LAYER_DEPTH {
            return Err(ProjectError::Invalid(
                "project layer tree exceeds its node or depth limit".into(),
            ));
        }
        if let Some(mask) = &layer.mask {
            validate_raster_reference(&mask.base, directory)?;
            for effect in &mask.effects {
                if let MaskEffectKindV1::Paint { raster } = &effect.kind {
                    validate_raster_reference(raster, directory)?;
                }
                if let MaskEffectKindV1::Image { resource, .. } = &effect.kind {
                    referenced_resource_ids.insert(*resource);
                }
            }
        }
        match &layer.content {
            ContentV1::Paint {
                channels,
                procedural_graph,
                ..
            } => {
                if let Some(graph) = procedural_graph {
                    graph
                        .validate()
                        .map_err(|error| ProjectError::Invalid(error.to_string()))?;
                }
                for channel in channels {
                    validate_raster_reference(&channel.raster, directory)?;
                }
            }
            ContentV1::Fill { channels, .. } => {
                for channel in channels {
                    if let subcraft_doc::FillSource::Bitmap { resource } = &channel.source {
                        referenced_resource_ids.insert(resource.0);
                    }
                }
            }
            ContentV1::Folder { children, .. } => {
                for child in children {
                    stack.push((child, depth + 1));
                }
            }
        }
        for effect in &layer.content_effects {
            match &effect.kind {
                ContentEffectKindV5::Paint { channels } => {
                    for channel in channels {
                        validate_raster_reference(&channel.raster, directory)?;
                    }
                }
                ContentEffectKindV5::Fill { channels, .. } => {
                    for channel in channels {
                        if let subcraft_doc::FillSource::Bitmap { resource } = &channel.source {
                            referenced_resource_ids.insert(resource.0);
                        }
                    }
                }
                ContentEffectKindV5::Levels { .. } => {}
            }
        }
    }
    if referenced_resource_ids
        .iter()
        .any(|id| !resource_ids.contains(id))
    {
        return Err(ProjectError::Invalid(
            "project references a missing image resource".into(),
        ));
    }
    Ok(())
}

fn validate_raster_reference(
    name: &str,
    directory: &limits::ZipDirectory,
) -> Result<(), ProjectError> {
    limits::validate_entry_name(name)?;
    if !name.starts_with("rasters/") || !directory.entries.contains_key(name) {
        return Err(ProjectError::Invalid(format!(
            "raster reference {name} is missing or invalid"
        )));
    }
    Ok(())
}

/// Read a project container. ZIP metadata and JSON structure are checked before allocating the
/// corresponding archive tables, document tree, decoded raster tiles, or resource bitmaps.
pub fn read_project<R: Read + Seek>(mut r: R) -> Result<ProjectData, ProjectError> {
    let directory = limits::preflight_zip_directory(&mut r)?;
    r.seek(std::io::SeekFrom::Start(0))?;
    let mut zip = zip::ZipArchive::new(r)?;
    if zip.len() != directory.entry_count {
        return Err(ProjectError::Invalid(
            "project ZIP entry count changed after preflight".into(),
        ));
    }
    let manifest_bytes = limits::read_entry_bounded(
        &mut zip,
        &directory,
        "manifest.json",
        limits::MAX_MANIFEST_BYTES,
        &mut limits::ReadBudget::default(),
    )
    .map_err(|_| ProjectError::NotAProject)?;
    limits::preflight_json(&manifest_bytes).map_err(|_| ProjectError::NotAProject)?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|_| ProjectError::NotAProject)?;
    if manifest.format != FORMAT_NAME {
        return Err(ProjectError::NotAProject);
    }
    if manifest.schema_version > SCHEMA_VERSION || manifest.schema_version == 0 {
        return Err(ProjectError::UnsupportedVersion(manifest.schema_version));
    }
    limits::validate_entry_name(&manifest.mesh)?;
    limits::validate_entry_name(&manifest.document)?;
    if manifest.mesh == manifest.document
        || manifest.mesh == "manifest.json"
        || manifest.document == "manifest.json"
    {
        return Err(ProjectError::Invalid(
            "manifest entries must refer to distinct project files".into(),
        ));
    }
    let mut loader = Loader {
        zip: &mut zip,
        directory: &directory,
        read_budget: limits::ReadBudget::default(),
        cache: BTreeMap::new(),
        raster_cost: 0,
    };
    let mesh =
        subcraft_mesh::serialize::decode(&loader.bytes(&manifest.mesh, limits::MAX_MESH_BYTES)?)
            .map_err(|e| ProjectError::Invalid(format!("mesh: {e}")))?;
    let document_bytes = loader.bytes(&manifest.document, limits::MAX_DOCUMENT_BYTES)?;
    limits::preflight_json(&document_bytes)?;
    let doc: DocumentV1 = serde_json::from_slice(&document_bytes)
        .map_err(|e| ProjectError::Invalid(format!("document: {e}")))?;
    validate_document_references(&doc, &directory)?;

    let mut texture_sets = Vec::new();
    let mut mesh_maps = MeshMapSet::new();
    for t in &doc.texture_sets {
        let mut ts = TextureSet {
            id: TextureSetId(t.id),
            name: t.name.clone(),
            material_slot: t.material_slot,
            resolution: t.resolution,
            channels: t
                .channels
                .iter()
                .map(|c| ChannelDef {
                    kind: c.kind,
                    format: c.format,
                })
                .collect(),
            layers: Vec::new(),
        };
        let layers = t
            .layers
            .iter()
            .map(|l| loader.layer(l, ts.resolution).map(Arc::new))
            .collect::<Result<Vec<_>, _>>()?;
        ts.layers = layers;
        let mut maps = BTreeMap::new();
        for m in &t.mesh_maps {
            let img = loader.decode_raster(&m.raster, None)?;
            if img.width() != img.height() || img.comps() != m.kind.components() {
                return Err(ProjectError::Invalid(format!(
                    "{}: unexpected layout for {:?}",
                    m.raster, m.kind
                )));
            }
            maps.insert(m.kind, img);
        }
        if !maps.is_empty() {
            mesh_maps.insert(ts.id, maps);
        }
        texture_sets.push(ts);
    }

    let resources = unpack_resources(&mut loader, &doc.resources)?;
    Ok(ProjectData {
        mesh,
        document: Document { texture_sets },
        resources,
        mesh_maps,
    })
}

pub fn load_project(path: &Path) -> Result<ProjectData, ProjectError> {
    reject_spp_load_path(path)?;
    #[cfg(not(target_arch = "wasm32"))]
    return read_project(std::io::BufReader::new(std::fs::File::open(path)?));
    #[cfg(target_arch = "wasm32")]
    return read_project(std::io::Cursor::new(subcraft_core::vfs::read(path)?));
}

fn reject_spp_load_path(path: &Path) -> Result<(), ProjectError> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("uspp"))
    {
        return Err(ProjectError::Invalid(
            "Universal SPP .uspp support follows native SPP integration; not implemented".into(),
        ));
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "kalium" | "kbrush"))
    {
        return Err(ProjectError::Invalid(
            "Kalium formats are not supported".into(),
        ));
    }
    if path
        .extension()
        .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("spp"))
    {
        return Err(ProjectError::Invalid(
            "Substance Painter .spp import is not implemented; source file was not modified".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
