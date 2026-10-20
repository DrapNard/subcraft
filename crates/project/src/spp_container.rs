//! Experimental native SPP document I/O, not a complete project importer.
//!
//! Only the observed unfiltered, contiguous U8 document dataset is read. Other
//! payloads stay opaque in a staged source copy. Size-changing writes reject;
//! mesh, stroke, fill and resource mapping into the editor remain unfinished.
//! Native HDF5 metadata parsing is not a sandbox: callers should run this backend
//! in an isolated worker with memory/time limits before exposing arbitrary files.
use crate::{
    spp_checksum::spp_dataset_checksum,
    spp_document::{MAX_DATASET_BYTES, SppDocumentRecords},
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
const DOCUMENT: &str = "/paint/document.bin";
const CHECKSUM: &str = "m3_x64_128";

#[derive(Debug, thiserror::Error)]
pub enum SppContainerError {
    #[error("SPP container I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("SPP native container: {0}")]
    Native(#[from] hdf5::Error),
    #[error("SPP document: {0}")]
    Records(#[from] crate::spp_document::SppDocumentError),
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
            "missing child or non-hard/oversized link on document path",
        ));
    }
    Ok(())
}

#[allow(unsafe_code)]
fn checked_dataset(file: &hdf5::File) -> Result<Dataset> {
    hard_child(&file.group("/")?, "paint")?;
    hard_child(&file.group("/paint")?, "document.bin")?;
    let ds = file.dataset(DOCUMENT)?;
    let dims = ds.shape();
    if dims.len() != 1 || dims[0] > MAX_DATASET_BYTES || dims[0] < 24 {
        return Err(SppContainerError::Unsupported(
            "document dimensions exceed accepted subset",
        ));
    }
    if ds.dtype()?.to_descriptor()? != TypeDescriptor::Unsigned(IntSize::U1) {
        return Err(SppContainerError::Unsupported(
            "document is not a U8 byte dataset",
        ));
    }
    let props = ds.create_plist()?;
    let external_count = hdf5::sync::sync(|| {
        // SAFETY: live dataset creation property-list ID, locked HDF5 call.
        unsafe { hdf5_sys::h5p::H5Pget_external_count(props.id()) }
    });
    if external_count != 0 || props.layout() != Layout::Contiguous || !ds.filters().is_empty() {
        return Err(SppContainerError::Unsupported(
            "external, virtual, chunked or filtered document storage",
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

fn publish_stage(
    stage: &Path,
    target: &Path,
    sync: impl FnOnce(&Path) -> std::result::Result<(), crate::ProjectError>,
) -> Result<()> {
    crate::replace_file(stage, target).map_err(|e| SppContainerError::Staging(e.to_string()))?;
    sync(target).map_err(|e| SppContainerError::CommittedButNotSynced(e.to_string()))
}

/// Save same-sized record edits in an atomic staged source copy. All untouched
/// datasets and attributes are preserved. Unsupported encodings/size changes fail
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
        let (stage, mut output) = crate::create_staging_file(target)
            .map_err(|e| SppContainerError::Staging(e.to_string()))?;
        let result = (|| {
            let mut input = File::open(&snapshot.path)?;
            let count =
                std::io::copy(&mut (&mut input).take(MAX_CONTAINER_BYTES + 1), &mut output)?;
            if count > MAX_CONTAINER_BYTES || count != input.metadata()?.len() {
                return Err(SppContainerError::SourceChanged);
            }
            output.sync_all()?;
            // Release the staging handle before HDF5 opens it on Windows.
            drop(output);
            let staged = read_snapshot(&stage)?;
            if staged.container_digest != snapshot.digest {
                return Err(SppContainerError::SourceChanged);
            }
            let file = hdf5::File::open_rw(&stage)?;
            {
                let ds = checked_dataset(&file)?;
                ds.write_raw(&encoded)?;
                ds.attr(CHECKSUM)?
                    .write_raw(&spp_dataset_checksum(&encoded))?;
            }
            file.flush()?;
            file.close()?;
            let verified = read_snapshot(&stage)?;
            if verified.original_bytes != encoded || verified.records != document.records {
                return Err(SppContainerError::SourceChanged);
            }
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
