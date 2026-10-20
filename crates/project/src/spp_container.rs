//! Experimental native SPP document I/O, not a complete project importer.
//!
//! Only the observed unfiltered, contiguous U8 document and scene datasets are read.
//! Other payloads stay opaque in a staged source copy. Size-changing writes reject;
//! mesh, stroke, fill and resource mapping into the editor remain unfinished.
//! Native HDF5 metadata parsing is not a sandbox: callers should run this backend
//! in an isolated worker with memory/time limits before exposing arbitrary files.
use crate::{
    spp_checksum::spp_dataset_checksum,
    spp_document::{MAX_DATASET_BYTES, SppDocumentRecords},
    spp_scene::SppScene,
};
use hdf5::types::{IntSize, TypeDescriptor};
use hdf5::{Dataset, Group, LinkType, plist::dataset_create::Layout};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub const MAX_CONTAINER_BYTES: u64 = 128 * 1024 * 1024;
const MAX_GROUP_LINKS: u64 = 4096;
#[cfg(test)]
const DOCUMENT: &str = "/paint/document.bin";
const SCENE_CHILD: &str = "scene.assbin";
#[cfg(test)]
const SCENE: &str = "/paint/scene.assbin";
const MAX_SCENE_BYTES: usize = 64 * 1024 * 1024;
const MIN_SCENE_BYTES: usize = 512;
const CHECKSUM: &str = "m3_x64_128";

#[derive(Debug, thiserror::Error)]
pub enum SppContainerError {
    #[error("SPP container I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("SPP native container: {0}")]
    Native(#[from] hdf5::Error),
    #[error("SPP document: {0}")]
    Records(#[from] crate::spp_document::SppDocumentError),
    #[error("SPP scene: {0}")]
    Scene(#[from] crate::spp_scene::SppSceneError),
    #[error("unsupported SPP container: {0}")]
    Unsupported(&'static str),
    #[error("SPP document checksum does not match")]
    Checksum,
    #[error("SPP staged copy no longer matches the source document")]
    SourceChanged,
    #[error("SPP output was replaced, but its directory could not be synced: {0}")]
    CommittedButNotSynced(String),
    #[error("SPP atomic staging: {0}")]
    Staging(String),
}
type Result<T> = std::result::Result<T, SppContainerError>;

#[derive(Clone, Debug)]
pub struct SppContainerDocument {
    pub records: SppDocumentRecords,
    /// Original bytes are retained for source identity and unchanged encoding checks.
    original_bytes: Vec<u8>,
    container_digest: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct SppContainerScene {
    pub scene: SppScene,
    container_digest: [u8; 32],
}

fn file_preflight(path: &Path) -> Result<()> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > MAX_CONTAINER_BYTES {
        return Err(SppContainerError::Unsupported(
            "source size/type exceeds limits",
        ));
    }
    let mut magic = [0; 8];
    file.read_exact(&mut magic)?;
    if magic != *b"\x89HDF\r\n\x1a\n" {
        return Err(SppContainerError::Unsupported(
            "HDF5 signature is not at byte zero",
        ));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn with_plugins_disabled<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    // Match official H5PLpublic.h; the pinned sys binding incorrectly uses a pointer
    // for the setter argument. Hold the reentrant HDF5 lock for the whole operation.
    unsafe extern "C" {
        #[link_name = "H5PLset_loading_state"]
        fn set_plugin_loading(mask: std::ffi::c_uint) -> std::ffi::c_int;
        #[link_name = "H5PLget_loading_state"]
        fn get_plugin_loading(mask: *mut std::ffi::c_uint) -> std::ffi::c_int;
    }
    hdf5::sync::sync(|| {
        let mut previous = 0;
        // SAFETY: official C signatures, valid output pointer, initialized/locked library.
        if unsafe { get_plugin_loading(&mut previous) } < 0 || unsafe { set_plugin_loading(0) } < 0
        {
            return Err(SppContainerError::Unsupported(
                "cannot disable HDF5 plugins",
            ));
        }
        struct Restore(std::ffi::c_uint);
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: same locked library and official signature; also runs on unwind.
                if unsafe { set_plugin_loading(self.0) } < 0 {
                    log::error!("HDF5 plugin state could not be restored");
                }
            }
        }
        let _restore = Restore(previous);
        work()
    })
}

/// Own a bounded private snapshot before the native parser sees any source bytes.
struct SourceSnapshot {
    path: std::path::PathBuf,
    digest: [u8; 32],
    permissions: std::fs::Permissions,
    // Keep the shared source lock throughout staging/verification.
    _source_guard: File,
}
impl Drop for SourceSnapshot {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
fn hash_file(file: &mut File) -> Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut count = 0_u64;
    let mut buf = [0_u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > MAX_CONTAINER_BYTES {
            return Err(SppContainerError::SourceChanged);
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().into())
}
impl SourceSnapshot {
    fn new(source: &Path) -> Result<Self> {
        let mut input = File::open(source)?;
        let meta = input.metadata()?;
        if !meta.is_file() || meta.len() > MAX_CONTAINER_BYTES {
            return Err(SppContainerError::Unsupported(
                "source size/type exceeds limits",
            ));
        }
        input
            .try_lock_shared()
            .map_err(|_| SppContainerError::Unsupported("source is in use or cannot be locked"))?;
        let temp_target = std::env::temp_dir().join("subcraft-spp-snapshot");
        let (path, mut output) = crate::create_staging_file(&temp_target)
            .map_err(|e| SppContainerError::Staging(e.to_string()))?;
        let result = (|| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            let mut hash = Sha256::new();
            let mut count = 0_u64;
            let mut buf = [0_u8; 65536];
            loop {
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                count += n as u64;
                if count > MAX_CONTAINER_BYTES {
                    return Err(SppContainerError::SourceChanged);
                }
                std::io::Write::write_all(&mut output, &buf[..n])?;
                hash.update(&buf[..n]);
            }
            if count != meta.len() || count != input.metadata()?.len() {
                return Err(SppContainerError::SourceChanged);
            }
            let digest: [u8; 32] = hash.finalize().into();
            if hash_file(&mut input)? != digest {
                return Err(SppContainerError::SourceChanged);
            }
            output.sync_all()?;
            Ok(digest)
        })();
        drop(output);
        match result {
            Ok(digest) => Ok(Self {
                path,
                digest,
                permissions: meta.permissions(),
                _source_guard: input,
            }),
            Err(error) => {
                let _ = std::fs::remove_file(path);
                Err(error)
            }
        }
    }
    fn verify_source(&self, source: &Path) -> Result<()> {
        let mut current = File::open(source)?;
        if !current.metadata()?.is_file()
            || current.metadata()?.len() > MAX_CONTAINER_BYTES
            || hash_file(&mut current)? != self.digest
        {
            return Err(SppContainerError::SourceChanged);
        }
        Ok(())
    }
}

fn hard_child(group: &Group, child: &str) -> Result<()> {
    if group.len() > MAX_GROUP_LINKS {
        return Err(SppContainerError::Unsupported(
            "group link count exceeds limits",
        ));
    }
    let found = group.iter_visit_default((false, false), |_, name, info, state| {
        if name.len() > 1024 || info.link_type != LinkType::Hard {
            state.1 = true;
            return false;
        }
        if name == child {
            state.0 = true;
        }
        true
    })?;
    if !found.0 || found.1 {
        return Err(SppContainerError::Unsupported(
            "missing child or non-hard/oversized link on accepted dataset path",
        ));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn checked_byte_dataset(
    file: &hdf5::File,
    child: &str,
    max_bytes: usize,
    min_bytes: usize,
) -> Result<Dataset> {
    if child.is_empty() || child.contains('/') || child.len() > 1024 {
        return Err(SppContainerError::Unsupported(
            "invalid byte dataset child name",
        ));
    }
    hard_child(&file.group("/")?, "paint")?;
    hard_child(&file.group("/paint")?, child)?;
    let path = format!("/paint/{child}");
    let ds = file.dataset(&path)?;
    let dims = ds.shape();
    if dims.len() != 1 || dims[0] > max_bytes || dims[0] < min_bytes {
        return Err(SppContainerError::Unsupported(
            "byte dataset dimensions exceed accepted subset",
        ));
    }
    if ds.dtype()?.to_descriptor()? != TypeDescriptor::Unsigned(IntSize::U1) {
        return Err(SppContainerError::Unsupported(
            "accepted payload is not a U8 byte dataset",
        ));
    }
    let props = ds.create_plist()?;
    let external_count = hdf5::sync::sync(|| {
        // SAFETY: live dataset creation property-list ID, locked HDF5 call.
        unsafe { hdf5_sys::h5p::H5Pget_external_count(props.id()) }
    });
    if external_count != 0 || props.layout() != Layout::Contiguous || !ds.filters().is_empty() {
        return Err(SppContainerError::Unsupported(
            "external, virtual, chunked or filtered byte dataset storage",
        ));
    }
    let attr = ds.attr(CHECKSUM)?;
    if attr.shape() != [2]
        || attr.dtype()?.to_descriptor()? != TypeDescriptor::Unsigned(IntSize::U8)
    {
        return Err(SppContainerError::Unsupported(
            "checksum must be exactly two unsigned 64-bit words",
        ));
    }
    Ok(ds)
}

fn checked_dataset(file: &hdf5::File) -> Result<Dataset> {
    checked_byte_dataset(file, "document.bin", MAX_DATASET_BYTES, 24)
}

fn read_checked_byte_dataset(
    file: &hdf5::File,
    child: &str,
    max_bytes: usize,
    min_bytes: usize,
) -> Result<Vec<u8>> {
    let ds = checked_byte_dataset(file, child, max_bytes, min_bytes)?;
    let bytes = ds.read_raw::<u8>()?;
    let hash = ds.attr(CHECKSUM)?.read_raw::<u64>()?;
    if hash.as_slice() != spp_dataset_checksum(&bytes) {
        return Err(SppContainerError::Checksum);
    }
    Ok(bytes)
}

/// Read the understood document dataset; does not create an editor Session.
pub fn read_document(path: &Path) -> Result<SppContainerDocument> {
    let snapshot = SourceSnapshot::new(path)?;
    with_plugins_disabled(|| read_snapshot(&snapshot.path))
}
fn read_snapshot(path: &Path) -> Result<SppContainerDocument> {
    file_preflight(path)?;
    let file = hdf5::File::open(path)?;
    let ds = checked_dataset(&file)?;
    let bytes = ds.read_raw::<u8>()?;
    let hash = ds.attr(CHECKSUM)?.read_raw::<u64>()?;
    if hash.as_slice() != spp_dataset_checksum(&bytes) {
        return Err(SppContainerError::Checksum);
    }
    let records = SppDocumentRecords::decode(&bytes)?;
    Ok(SppContainerDocument {
        records,
        original_bytes: bytes,
        container_digest: hash_file(&mut File::open(path)?)?,
    })
}

/// Read the observed opaque scene payload after bounded snapshotting and checksum validation.
/// This returns bytes only; it does not decode a mesh or construct an editor Session.
pub fn read_scene_bytes(path: &Path) -> Result<Vec<u8>> {
    let snapshot = SourceSnapshot::new(path)?;
    with_plugins_disabled(|| read_scene_snapshot(&snapshot.path))
}

/// Read the understood scene 1.7 triangle subset from one bounded source snapshot.
/// The returned scene preserves opaque hierarchy/material sections but is not a SubCraft mesh.
pub fn read_scene(path: &Path) -> Result<SppContainerScene> {
    let snapshot = SourceSnapshot::new(path)?;
    with_plugins_disabled(|| {
        let bytes = read_scene_snapshot(&snapshot.path)?;
        let scene = SppScene::decode(&bytes)?;
        Ok(SppContainerScene {
            scene,
            container_digest: snapshot.digest,
        })
    })
}

fn read_scene_snapshot(path: &Path) -> Result<Vec<u8>> {
    file_preflight(path)?;
    let file = hdf5::File::open(path)?;
    read_checked_byte_dataset(&file, SCENE_CHILD, MAX_SCENE_BYTES, MIN_SCENE_BYTES)
}

fn publish_stage(
    stage: &Path,
    target: &Path,
    sync: impl FnOnce(&Path) -> std::result::Result<(), crate::ProjectError>,
) -> Result<()> {
    crate::replace_file(stage, target).map_err(|e| SppContainerError::Staging(e.to_string()))?;
    sync(target).map_err(|e| SppContainerError::CommittedButNotSynced(e.to_string()))
}

/// Copy the locked bounded source snapshot beside the target, validate the full staged container
/// before invoking the caller's dataset edit, then verify the live source and atomically publish.
fn stage_copy_and_replace(
    source: &Path,
    target: &Path,
    snapshot: &SourceSnapshot,
    edit: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    let (stage, mut output) = crate::create_staging_file(target)
        .map_err(|e| SppContainerError::Staging(e.to_string()))?;
    let result = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            output.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let mut input = File::open(&snapshot.path)?;
        let count = std::io::copy(&mut (&mut input).take(MAX_CONTAINER_BYTES + 1), &mut output)?;
        if count > MAX_CONTAINER_BYTES || count != input.metadata()?.len() {
            return Err(SppContainerError::SourceChanged);
        }
        output.sync_all()?;
        // Release the staging handle before HDF5 opens it on Windows.
        drop(output);

        let mut staged_file = File::open(&stage)?;
        if hash_file(&mut staged_file)? != snapshot.digest {
            return Err(SppContainerError::SourceChanged);
        }
        drop(staged_file);
        edit(&stage)?;

        OpenOptions::new().write(true).open(&stage)?.sync_all()?;
        snapshot.verify_source(source)?;
        std::fs::set_permissions(&stage, snapshot.permissions.clone())?;
        publish_stage(&stage, target, crate::sync_parent_directory)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&stage);
    }
    result
}

/// Invalidate only derived texture-cache payloads. Keep its groups/attributes and every
/// artist resource elsewhere. Never follow links or infer cache data from resource names.
fn invalidate_texture_cache(file: &hdf5::File, records: &SppDocumentRecords) -> Result<()> {
    if !file.link_exists("texture-cache") {
        return Ok(());
    }
    hard_child(&file.group("/")?, "texture-cache")?;
    fn check_pins(
        object: &crate::spp_document::SppObject,
        records: &SppDocumentRecords,
        depth: usize,
    ) -> Result<()> {
        use crate::spp_document::SppRecordValue;
        if depth > crate::spp_document::MAX_OBJECT_DEPTH {
            return Err(SppContainerError::Unsupported("cache pin traversal depth"));
        }
        let class = &records.classes[object.class_index];
        if class.name == "DataLayerColor" {
            for (property, value) in class.properties.iter().zip(&object.fields) {
                if property.name == "explicitPersistentCachePin"
                    && !matches!(value, SppRecordValue::Bool(false))
                {
                    return Err(SppContainerError::Unsupported(
                        "persistent layer cache pins require separate save support",
                    ));
                }
            }
        }
        for value in &object.fields {
            match value {
                SppRecordValue::Object(Some(child)) => check_pins(child, records, depth + 1)?,
                SppRecordValue::InlineObjects(children) => {
                    for child in children {
                        check_pins(child, records, depth + 1)?;
                    }
                }
                SppRecordValue::PointerObjects(children) => {
                    for child in children.iter().flatten() {
                        check_pins(child, records, depth + 1)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    if let Some(root) = &records.root {
        check_pins(root, records, 0)?;
    }
    fn collect(
        group: &Group,
        depth: usize,
        total: &mut usize,
        datasets: &mut Vec<String>,
    ) -> Result<()> {
        if depth > 16 || group.len() > MAX_GROUP_LINKS || group.loc_info()?.num_links != 1 {
            return Err(SppContainerError::Unsupported(
                "cache group depth, aliases or link count",
            ));
        }
        let (names, invalid) =
            group.iter_visit_default((Vec::<String>::new(), false), |_, name, info, state| {
                if name.len() > 1024 || info.link_type != LinkType::Hard {
                    state.1 = true;
                    return false;
                }
                state.0.push(name.to_owned());
                true
            })?;
        if invalid {
            return Err(SppContainerError::Unsupported(
                "cache contains non-hard or oversized links",
            ));
        }
        *total = total
            .checked_add(names.len())
            .filter(|v| *v <= 4096)
            .ok_or(SppContainerError::Unsupported("cache object count"))?;
        for name in names {
            match group.loc_type_by_name(&name)? {
                hdf5::LocationType::Group => {
                    collect(&group.group(&name)?, depth + 1, total, datasets)?
                }
                hdf5::LocationType::Dataset => datasets.push(format!("{}/{}", group.name(), name)),
                _ => return Err(SppContainerError::Unsupported("cache object type")),
            }
        }
        Ok(())
    }
    let mut datasets = Vec::new();
    collect(&file.group("texture-cache")?, 0, &mut 0, &mut datasets)?;
    // Validate the entire cache link graph before making any change, even in staging.
    for path in datasets {
        file.unlink(&path)?;
    }
    Ok(())
}

/// Save same-sized record edits in an atomic staged source copy. All untouched
/// artist datasets and attributes are preserved. Changed records invalidate derived
/// texture-cache payloads; persistent cache pins reject. Unsupported encodings/size changes fail
/// before replacement, including when target equals source.
pub fn save_document_copy(
    source: &Path,
    target: &Path,
    document: &SppContainerDocument,
) -> Result<()> {
    if !target
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("spp"))
    {
        return Err(SppContainerError::Unsupported(
            "output must have an .spp extension",
        ));
    }
    let encoded = document.records.encode()?;
    let snapshot = SourceSnapshot::new(source)?;
    if snapshot.digest != document.container_digest {
        return Err(SppContainerError::SourceChanged);
    }
    with_plugins_disabled(|| {
        let original = read_snapshot(&snapshot.path)?;
        if encoded.len() != original.original_bytes.len() {
            return Err(SppContainerError::Unsupported(
                "size-changing document writes are not implemented",
            ));
        }
        stage_copy_and_replace(source, target, &snapshot, |stage| {
            let file = hdf5::File::open_rw(stage)?;
            {
                let ds = checked_dataset(&file)?;
                ds.write_raw(&encoded)?;
                ds.attr(CHECKSUM)?
                    .write_raw(&spp_dataset_checksum(&encoded))?;
            }
            if encoded != original.original_bytes {
                invalidate_texture_cache(&file, &original.records)?;
            }
            file.flush()?;
            file.close()?;
            let verified = read_snapshot(&stage)?;
            if verified.original_bytes != encoded || verified.records != document.records {
                return Err(SppContainerError::SourceChanged);
            }
            Ok(())
        })
    })
}

/// Save same-sized understood scene edits into an atomic copy of the source container.
/// Artist resources and metadata remain from the source snapshot; changed geometry
/// invalidates derived texture-cache payloads and refuses persistent cache pins.
pub fn save_scene_copy(source: &Path, target: &Path, scene: &SppContainerScene) -> Result<()> {
    if !target
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("spp"))
    {
        return Err(SppContainerError::Unsupported(
            "output must have an .spp extension",
        ));
    }
    let encoded = scene.scene.encode()?;
    let snapshot = SourceSnapshot::new(source)?;
    if snapshot.digest != scene.container_digest {
        return Err(SppContainerError::SourceChanged);
    }
    with_plugins_disabled(|| {
        let original = read_scene_snapshot(&snapshot.path)?;
        if encoded.len() != original.len() {
            return Err(SppContainerError::Unsupported(
                "size-changing scene writes are not implemented",
            ));
        }
        stage_copy_and_replace(source, target, &snapshot, |stage| {
            let file = hdf5::File::open_rw(stage)?;
            {
                let ds =
                    checked_byte_dataset(&file, SCENE_CHILD, MAX_SCENE_BYTES, MIN_SCENE_BYTES)?;
                ds.write_raw(&encoded)?;
                ds.attr(CHECKSUM)?
                    .write_raw(&spp_dataset_checksum(&encoded))?;
            }
            if encoded != original && file.link_exists("texture-cache") {
                let document = read_snapshot(&snapshot.path)?;
                invalidate_texture_cache(&file, &document.records)?;
            }
            file.flush()?;
            file.close()?;
            let verified = read_scene_snapshot(stage)?;
            if verified != encoded {
                return Err(SppContainerError::SourceChanged);
            }
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spp_document::{
        SppClassDefinition, SppDocumentHeader, SppObject, SppPropertyDefinition, SppRecordValue,
    };
    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("subcraft-spp-container-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&p).unwrap();
            Self(p)
        }
        fn path(&self, name: &str) -> std::path::PathBuf {
            self.0.join(name)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn records() -> SppDocumentRecords {
        SppDocumentRecords {
            header: SppDocumentHeader {
                magic: [0xdd, 0x2f, 0x7c, 0x1b],
                version: 1,
                preserved_word_8: 88,
                preserved_word_12: 18,
            },
            classes: vec![SppClassDefinition {
                name: "DataOriginalTest".into(),
                properties: vec![SppPropertyDefinition {
                    name: "label".into(),
                    type_code: 16,
                }],
            }],
            root: Some(SppObject {
                class_index: 0,
                fields: vec![SppRecordValue::Utf8("Original".into())],
            }),
        }
    }
    fn fixture(path: &Path, chunked: bool) {
        let file = hdf5::File::create(path).unwrap();
        let group = file.create_group("paint").unwrap();
        let bytes = records().encode().unwrap();
        let builder = group.new_dataset::<u8>().shape(bytes.len());
        let ds = if chunked {
            builder.chunk(16).create("document.bin")
        } else {
            builder.create("document.bin")
        }
        .unwrap();
        ds.write_raw(&bytes).unwrap();
        ds.new_attr::<u64>()
            .shape(2)
            .create(CHECKSUM)
            .unwrap()
            .write_raw(&spp_dataset_checksum(&bytes))
            .unwrap();
        ds.new_attr::<u32>()
            .create("future-document-attribute")
            .unwrap()
            .write_scalar(&37)
            .unwrap();
        write_scene_dataset(&group, &scene_bytes(), false);
        file.new_attr::<u32>()
            .create("future-root-attribute")
            .unwrap()
            .write_scalar(&41)
            .unwrap();
        let unknown = file.create_group("unknown-group").unwrap();
        unknown
            .new_dataset_builder()
            .with_data(&[7_u8, 9, 11, 13])
            .create("opaque")
            .unwrap();
        file.flush().unwrap();
    }

    fn scene_bytes() -> Vec<u8> {
        let mut header = vec![0; 512];
        header[..19].copy_from_slice(b"ASSIMP.binary-dump.");
        header[44..48].copy_from_slice(&1_u32.to_le_bytes());
        header[48..52].copy_from_slice(&7_u32.to_le_bytes());
        header[56..60].copy_from_slice(&0x11_u32.to_le_bytes());
        header[448] = 97;

        let root = crate::spp_scene_node::SppSceneNode {
            name: "Root".into(),
            matrix_bits: [0; 16],
            mesh_indices: vec![0],
            children: vec![],
            opaque_suffix: vec![43; 4],
        }
        .encode()
        .unwrap();

        let mut mesh = Vec::new();
        mesh.extend_from_slice(&8_u32.to_le_bytes());
        mesh.extend_from_slice(b"triangle");
        mesh.extend_from_slice(&4_u32.to_le_bytes());
        mesh.extend_from_slice(&2_u32.to_le_bytes());
        mesh.extend_from_slice(&0x107_u32.to_le_bytes());
        for vectors in [
            [[0.0_f32, 0.0, 0.0], [2.0, 0.0, 0.0], [0.0, 3.0, 0.0]],
            [[0.0_f32, 0.0, 1.0]; 3],
            [[1.0_f32, 0.0, 0.0]; 3],
            [[0.0_f32, 1.0, 0.0]; 3],
        ] {
            mesh.extend_from_slice(&3_u32.to_le_bytes());
            for vector in vectors {
                for value in vector {
                    mesh.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        mesh.extend_from_slice(&2_u32.to_le_bytes());
        mesh.extend_from_slice(&3_u32.to_le_bytes());
        for uv in [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            for value in uv {
                mesh.extend_from_slice(&value.to_le_bytes());
            }
        }
        mesh.extend_from_slice(&1_u32.to_le_bytes());
        mesh.extend_from_slice(&1_u32.to_le_bytes());
        mesh.extend_from_slice(&3_u32.to_le_bytes());
        for index in [0_u32, 1, 2] {
            mesh.extend_from_slice(&index.to_le_bytes());
        }
        mesh.extend_from_slice(&0_u32.to_le_bytes());

        fn append_block(out: &mut Vec<u8>, bytes: &[u8]) {
            out.extend_from_slice(&((bytes.len() as u64) + 8).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        let mut body = 8_u32.to_le_bytes().to_vec();
        body.extend_from_slice(&root);
        body.extend_from_slice(&1_u32.to_le_bytes());
        append_block(&mut body, &mesh);
        let materials = crate::spp_scene_material::SppSceneMaterials {
            materials: vec![
                crate::spp_scene_material::SppSceneMaterial {
                    properties: vec![],
                    trailing: vec![97]
                };
                3
            ],
            opaque_tail: vec![79, 0, 0, 0, 93],
        }
        .encode()
        .unwrap();
        body.extend_from_slice(&materials);
        append_block(&mut header, &body);
        header
    }

    fn write_scene_dataset(group: &Group, bytes: &[u8], chunked: bool) {
        let builder = group.new_dataset::<u8>().shape(bytes.len());
        let ds = if chunked {
            builder.chunk(128).create("scene.assbin")
        } else {
            builder.create("scene.assbin")
        }
        .unwrap();
        ds.write_raw(bytes).unwrap();
        ds.new_attr::<u64>()
            .shape(2)
            .create(CHECKSUM)
            .unwrap()
            .write_raw(&spp_dataset_checksum(bytes))
            .unwrap();
    }

    fn scene_fixture(path: &Path, chunked: bool) {
        let file = hdf5::File::create(path).unwrap();
        let group = file.create_group("paint").unwrap();
        write_scene_dataset(&group, &scene_bytes(), chunked);
        file.flush().unwrap();
    }
    #[test]
    fn same_size_edit_preserves_opaque_data_attributes_and_source() {
        let tmp = Temp::new();
        let source = tmp.path("source.spp");
        let target = tmp.path("edited.spp");
        fixture(&source, false);
        let original = std::fs::read(&source).unwrap();
        let mut doc = read_document(&source).unwrap();
        doc.records.root.as_mut().unwrap().fields[0] = SppRecordValue::Utf8("SubCraft".into());
        save_document_copy(&source, &target, &doc).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(read_document(&target).unwrap().records, doc.records);
        let f = hdf5::File::open(&target).unwrap();
        assert_eq!(
            f.dataset("unknown-group/opaque")
                .unwrap()
                .read_raw::<u8>()
                .unwrap(),
            [7, 9, 11, 13]
        );
        assert_eq!(
            f.attr("future-root-attribute")
                .unwrap()
                .read_scalar::<u32>()
                .unwrap(),
            41
        );
        assert_eq!(
            f.dataset(DOCUMENT)
                .unwrap()
                .attr("future-document-attribute")
                .unwrap()
                .read_scalar::<u32>()
                .unwrap(),
            37
        );
    }
    #[test]
    fn stale_document_rejects_resource_only_source_change() {
        let tmp = Temp::new();
        let source = tmp.path("source.spp");
        let target = tmp.path("existing.spp");
        fixture(&source, false);
        let doc = read_document(&source).unwrap();
        std::fs::write(&target, b"keep existing").unwrap();
        {
            let file = hdf5::File::open_rw(&source).unwrap();
            file.dataset("unknown-group/opaque")
                .unwrap()
                .write_raw(&[1_u8, 2, 3, 4])
                .unwrap();
            file.flush().unwrap();
        }
        assert!(matches!(
            save_document_copy(&source, &target, &doc),
            Err(SppContainerError::SourceChanged)
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"keep existing");
    }
    #[test]
    fn changed_records_clear_derived_cache_but_noop_and_unknown_resources_survive() {
        let tmp = Temp::new();
        let source = tmp.path("source.spp");
        let target = tmp.path("edited.spp");
        fixture(&source, false);
        {
            let f = hdf5::File::open_rw(&source).unwrap();
            let cache = f.create_group("texture-cache").unwrap();
            cache
                .new_attr::<u32>()
                .create("future-cache-attribute")
                .unwrap()
                .write_scalar(&73)
                .unwrap();
            cache
                .new_dataset_builder()
                .with_data(&[1_u8, 2, 3])
                .create("header")
                .unwrap();
            cache
                .create_group("material")
                .unwrap()
                .new_dataset_builder()
                .with_data(&[7_u8, 8])
                .create("derived")
                .unwrap();
            f.flush().unwrap();
        }
        let mut document = read_document(&source).unwrap();
        save_document_copy(&source, &target, &document).unwrap();
        assert!(
            hdf5::File::open(&target)
                .unwrap()
                .link_exists("texture-cache/material/derived")
        );
        document.records.root.as_mut().unwrap().fields[0] =
            crate::spp_document::SppRecordValue::Utf8("SubCraft".into());
        save_document_copy(&source, &target, &document).unwrap();
        let f = hdf5::File::open(&target).unwrap();
        assert!(!f.link_exists("texture-cache/header"));
        assert_eq!(f.group("texture-cache/material").unwrap().len(), 0);
        assert_eq!(
            f.group("texture-cache")
                .unwrap()
                .attr("future-cache-attribute")
                .unwrap()
                .read_scalar::<u32>()
                .unwrap(),
            73
        );
        assert_eq!(
            f.dataset("unknown-group/opaque")
                .unwrap()
                .read_raw::<u8>()
                .unwrap(),
            [7, 9, 11, 13]
        );
        assert!(
            hdf5::File::open(&source)
                .unwrap()
                .link_exists("texture-cache/material/derived")
        );
    }
    #[test]
    fn unsafe_cache_links_and_pins_fail_without_replacing_target() {
        let tmp = Temp::new();
        let source = tmp.path("source.spp");
        let target = tmp.path("existing.spp");
        fixture(&source, false);
        std::fs::write(&target, b"keep existing").unwrap();
        {
            let f = hdf5::File::open_rw(&source).unwrap();
            let cache = f.create_group("texture-cache").unwrap();
            cache.link_soft("/unknown-group", "unsafe").unwrap();
            f.flush().unwrap();
        }
        let before = std::fs::read(&source).unwrap();
        let mut document = read_document(&source).unwrap();
        document.records.root.as_mut().unwrap().fields[0] =
            crate::spp_document::SppRecordValue::Utf8("SubCraft".into());
        assert!(save_document_copy(&source, &target, &document).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep existing");
        assert_eq!(std::fs::read(&source).unwrap(), before);
        let safe_cache = tmp.path("safe.spp");
        fixture(&safe_cache, false);
        let f = hdf5::File::open_rw(&safe_cache).unwrap();
        f.create_group("texture-cache").unwrap();
        let mut pinned = records();
        pinned.classes[0].name = "DataLayerColor".into();
        pinned.classes[0]
            .properties
            .push(crate::spp_document::SppPropertyDefinition {
                name: "explicitPersistentCachePin".into(),
                type_code: 10,
            });
        pinned
            .root
            .as_mut()
            .unwrap()
            .fields
            .push(crate::spp_document::SppRecordValue::Bool(true));
        assert!(invalidate_texture_cache(&f, &pinned).is_err());
    }
    #[test]
    fn unsupported_length_and_bad_target_preserve_existing_files() {
        let tmp = Temp::new();
        let source = tmp.path("source.spp");
        let target = tmp.path("existing.spp");
        fixture(&source, false);
        std::fs::write(&target, b"keep existing").unwrap();
        let before = std::fs::read(&source).unwrap();
        let mut doc = read_document(&source).unwrap();
        doc.records.root.as_mut().unwrap().fields[0] =
            SppRecordValue::Utf8("longer layer name".into());
        assert!(save_document_copy(&source, &target, &doc).is_err());
        assert!(save_document_copy(&source, &source, &doc).is_err());
        assert!(save_document_copy(&source, &tmp.path("bad.subcraft"), &doc).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), before);
        assert_eq!(std::fs::read(&target).unwrap(), b"keep existing");
        assert_eq!(std::fs::read_dir(&tmp.0).unwrap().count(), 2);
    }
    #[test]
    fn checksum_corruption_and_chunked_document_are_rejected() {
        let tmp = Temp::new();
        let source = tmp.path("bad.spp");
        fixture(&source, false);
        {
            let f = hdf5::File::open_rw(&source).unwrap();
            f.dataset(DOCUMENT)
                .unwrap()
                .attr(CHECKSUM)
                .unwrap()
                .write_raw(&[0_u64, 0])
                .unwrap();
            f.flush().unwrap();
        }
        assert!(matches!(
            read_document(&source),
            Err(SppContainerError::Checksum)
        ));
        let chunked = tmp.path("chunked.spp");
        fixture(&chunked, true);
        assert!(matches!(
            read_document(&chunked),
            Err(SppContainerError::Unsupported(_))
        ));
    }

    #[test]
    fn scene_bytes_are_bounded_hard_linked_and_checksum_verified() {
        let tmp = Temp::new();
        let valid = tmp.path("scene.spp");
        scene_fixture(&valid, false);
        assert_eq!(read_scene_bytes(&valid).unwrap(), scene_bytes());
        let read = read_scene(&valid).unwrap();
        assert_eq!(read.scene.encode().unwrap(), scene_bytes());

        {
            let file = hdf5::File::open_rw(&valid).unwrap();
            file.dataset(SCENE)
                .unwrap()
                .attr(CHECKSUM)
                .unwrap()
                .write_raw(&[0_u64, 0])
                .unwrap();
            file.flush().unwrap();
        }
        assert!(matches!(
            read_scene_bytes(&valid),
            Err(SppContainerError::Checksum)
        ));

        let chunked = tmp.path("scene-chunked.spp");
        scene_fixture(&chunked, true);
        assert!(matches!(
            read_scene_bytes(&chunked),
            Err(SppContainerError::Unsupported(_))
        ));

        let soft = tmp.path("scene-soft-link.spp");
        {
            let file = hdf5::File::create(&soft).unwrap();
            let group = file.create_group("paint").unwrap();
            let bytes = scene_bytes();
            let target = group
                .new_dataset_builder()
                .with_data(&bytes)
                .create("scene-target")
                .unwrap();
            target
                .new_attr::<u64>()
                .shape(2)
                .create(CHECKSUM)
                .unwrap()
                .write_raw(&spp_dataset_checksum(&bytes))
                .unwrap();
            group
                .link_soft("/paint/scene-target", "scene.assbin")
                .unwrap();
            file.flush().unwrap();
        }
        assert!(matches!(
            read_scene_bytes(&soft),
            Err(SppContainerError::Unsupported(_))
        ));

        let external = tmp.path("scene-external-link.spp");
        let external_target = tmp.path("scene-target.h5");
        {
            let target_file = hdf5::File::create(&external_target).unwrap();
            let bytes = scene_bytes();
            let target = target_file
                .new_dataset_builder()
                .with_data(&bytes)
                .create("scene")
                .unwrap();
            target
                .new_attr::<u64>()
                .shape(2)
                .create(CHECKSUM)
                .unwrap()
                .write_raw(&spp_dataset_checksum(&bytes))
                .unwrap();
            target_file.flush().unwrap();
            let file = hdf5::File::create(&external).unwrap();
            let group = file.create_group("paint").unwrap();
            let filename = external_target.to_string_lossy();
            group
                .link_external(&filename, "/scene", "scene.assbin")
                .unwrap();
            file.flush().unwrap();
        }
        assert!(matches!(
            read_scene_bytes(&external),
            Err(SppContainerError::Unsupported(_))
        ));
    }

    #[test]
    fn scene_save_copies_preserves_source_and_rejects_stale_or_unsupported_edits() {
        let tmp = Temp::new();
        let source = tmp.path("scene-source.spp");
        let target = tmp.path("scene-copy.spp");
        fixture(&source, false);
        let original_source = std::fs::read(&source).unwrap();
        let mut scene = read_scene(&source).unwrap();
        scene.scene.meshes[0].positions[1][0] = 1.25;
        save_scene_copy(&source, &target, &scene).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), original_source);
        assert_eq!(
            read_scene(&target).unwrap().scene.meshes[0].positions[1][0],
            1.25
        );
        assert_eq!(
            read_document(&target).unwrap().records,
            read_document(&source).unwrap().records
        );

        let mut size_change = read_scene(&source).unwrap();
        size_change.scene.meshes[0].name = "a longer mesh label".into();
        std::fs::write(&target, b"keep existing").unwrap();
        assert!(matches!(
            save_scene_copy(&source, &target, &size_change),
            Err(SppContainerError::Unsupported(_))
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"keep existing");

        let mut stale = read_scene(&source).unwrap();
        {
            let file = hdf5::File::open_rw(&source).unwrap();
            file.dataset("unknown-group/opaque")
                .unwrap()
                .write_raw(&[1_u8, 2, 3, 4])
                .unwrap();
            file.flush().unwrap();
        }
        stale.scene.meshes[0].positions[1][0] = 1.5;
        assert!(matches!(
            save_scene_copy(&source, &target, &stale),
            Err(SppContainerError::SourceChanged)
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"keep existing");

        let unsupported = tmp.path("scene-unsupported.spp");
        {
            let file = hdf5::File::create(&unsupported).unwrap();
            let group = file.create_group("paint").unwrap();
            write_scene_dataset(&group, &vec![0_u8; MIN_SCENE_BYTES], false);
            file.flush().unwrap();
        }
        let unsupported_before = std::fs::read(&unsupported).unwrap();
        assert!(matches!(
            read_scene(&unsupported),
            Err(SppContainerError::Scene(_))
        ));
        assert_eq!(std::fs::read(&unsupported).unwrap(), unsupported_before);
    }
    #[test]
    fn soft_document_link_and_file_budget_reject_before_decoding() {
        let tmp = Temp::new();
        let source = tmp.path("soft.spp");
        fixture(&source, false);
        {
            let f = hdf5::File::open_rw(&source).unwrap();
            let group = f.group("paint").unwrap();
            group.unlink("document.bin").unwrap();
            group
                .link_soft("/unknown-group/opaque", "document.bin")
                .unwrap();
            f.flush().unwrap();
        }
        assert!(matches!(
            read_document(&source),
            Err(SppContainerError::Unsupported(_))
        ));
        let huge = tmp.path("huge.spp");
        File::create(&huge)
            .unwrap()
            .set_len(MAX_CONTAINER_BYTES + 1)
            .unwrap();
        assert!(matches!(
            read_document(&huge),
            Err(SppContainerError::Unsupported(_))
        ));
    }
    #[cfg(unix)]
    #[test]
    fn staged_payload_is_private_before_any_dataset_edit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = Temp::new();
        let source = tmp.path("public-source.spp");
        let target = tmp.path("copy.spp");
        fixture(&source, false);
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
        let snapshot = SourceSnapshot::new(&source).unwrap();
        stage_copy_and_replace(&source, &target, &snapshot, |stage| {
            assert_eq!(
                std::fs::metadata(stage).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::read(stage).unwrap(),
                std::fs::read(&source).unwrap()
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(
            std::fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
    #[cfg(unix)]
    #[test]
    fn private_snapshot_and_new_output_keep_private_permissions_and_detect_resource_changes() {
        use std::{
            io::{Seek, Write},
            os::unix::fs::PermissionsExt,
        };
        let tmp = Temp::new();
        let source = tmp.path("source.spp");
        fixture(&source, false);
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        let target = tmp.path("private-copy.spp");
        save_document_copy(&source, &target, &read_document(&source).unwrap()).unwrap();
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let offset = {
            let f = hdf5::File::open(&source).unwrap();
            f.dataset("unknown-group/opaque").unwrap().offset().unwrap()
        };
        let snapshot = SourceSnapshot::new(&source).unwrap();
        assert_eq!(
            std::fs::metadata(&snapshot.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        snapshot.verify_source(&source).unwrap();
        // Simulate a writer which does not cooperate with advisory source locks.
        let mut raw = OpenOptions::new().write(true).open(&source).unwrap();
        raw.seek(SeekFrom::Start(offset)).unwrap();
        raw.write_all(&[33]).unwrap();
        raw.sync_all().unwrap();
        assert!(matches!(
            snapshot.verify_source(&source),
            Err(SppContainerError::SourceChanged)
        ));
    }

    #[test]
    fn post_commit_directory_sync_failure_is_distinct_from_an_uncommitted_save() {
        let tmp = Temp::new();
        let stage = tmp.path("stage.spp");
        let target = tmp.path("target.spp");
        std::fs::write(&stage, b"new bytes").unwrap();
        std::fs::write(&target, b"old bytes").unwrap();
        let result = publish_stage(&stage, &target, |_| {
            Err(crate::ProjectError::Invalid("injected sync failure".into()))
        });
        assert!(matches!(
            result,
            Err(SppContainerError::CommittedButNotSynced(_))
        ));
        assert_eq!(std::fs::read(&target).unwrap(), b"new bytes");
        assert!(!stage.exists());
    }

    #[test]
    #[allow(unsafe_code)]
    fn plugin_state_is_scoped_and_restored_after_errors() {
        unsafe extern "C" {
            #[link_name = "H5PLget_loading_state"]
            fn flags(output: *mut std::ffi::c_uint) -> std::ffi::c_int;
        }
        hdf5::sync::sync(|| {
            let mut before = 0;
            assert!(unsafe { flags(&mut before) } >= 0);
            let result: Result<()> = with_plugins_disabled(|| {
                let mut during = 1;
                assert!(unsafe { flags(&mut during) } >= 0);
                assert_eq!(during, 0);
                Err(SppContainerError::Unsupported("injected work failure"))
            });
            assert!(result.is_err());
            let mut after = 0;
            assert!(unsafe { flags(&mut after) } >= 0);
            assert_eq!(after, before);
        });
    }
}
