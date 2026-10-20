//! Retained complete SPP archives for editor provenance and genuine SPP recovery.
//! Unknown resources remain in a private, shared archive rather than being reconstructed
//! from the editable projection. Native reads and writes always run in the worker.
use crate::{
    spp_container::{MAX_CONTAINER_BYTES, SppTargetRevision},
    spp_worker::{self, WorkerProject},
};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
type Result<T> = std::result::Result<T, String>;

#[derive(Debug)]
struct Archive {
    directory: PathBuf,
    path: PathBuf,
    digest: [u8; 32],
}
impl Drop for Archive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
fn hash(file: &mut File) -> Result<([u8; 32], u64)> {
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        count += n as u64;
        if count > MAX_CONTAINER_BYTES {
            return Err("SPP archive exceeds retained-source limit".into());
        }
        digest.update(&buffer[..n]);
    }
    Ok((digest.finalize().into(), count))
}
impl Archive {
    fn capture(source: &Path) -> Result<Arc<Self>> {
        // NONBLOCK prevents an attacker swapping a regular file for a FIFO from hanging
        // the editor before the worker's timeout applies. It has no effect on regular IO.
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(source)
            .map_err(|e| e.to_string())?;
        let meta = input.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || !(8..=MAX_CONTAINER_BYTES).contains(&meta.len()) {
            return Err("SPP source must be a bounded regular archive".into());
        }
        input
            .try_lock_shared()
            .map_err(|_| "SPP source is in use")?;
        let directory =
            std::env::temp_dir().join(format!("subcraft-spp-retained-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|e| e.to_string())?;
        let path = directory.join("source.spp");
        let mut archive = Self {
            directory,
            path,
            digest: [0; 32],
        };
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&archive.path)
            .map_err(|e| e.to_string())?;
        let mut digest = Sha256::new();
        let mut count = 0_u64;
        let mut buffer = [0; 65536];
        loop {
            let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if count == 0 && (n < 8 || buffer[..8] != *b"\x89HDF\r\n\x1a\n") {
                return Err("SPP source does not have the supported HDF5 signature".into());
            }
            count += n as u64;
            if count > MAX_CONTAINER_BYTES {
                return Err("SPP source grew beyond the archive limit".into());
            }
            output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            digest.update(&buffer[..n]);
        }
        archive.digest = digest.finalize().into();
        if count != meta.len()
            || input.metadata().map_err(|e| e.to_string())?.len() != count
            || hash(&mut input)? != (archive.digest, count)
        {
            return Err("SPP source changed while capturing its archive".into());
        }
        output.sync_all().map_err(|e| e.to_string())?;
        drop(output);
        // Drop the original source handle and lock here, not when the editor closes.
        Ok(Arc::new(archive))
    }
    fn reject_alias(&self, target: &Path) -> Result<()> {
        let source = std::fs::metadata(&self.path).map_err(|e| e.to_string())?;
        match std::fs::metadata(target) {
            Ok(meta) if meta.dev() == source.dev() && meta.ino() == source.ino() => {
                Err("cannot overwrite a retained SPP base archive".into())
            }
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Cheap clones retain the complete immutable base archive independently of its
/// original path. Editable records remain outside history until Session integration.
#[derive(Clone, Debug)]
pub struct RetainedProject {
    pub project: WorkerProject,
    archive: Arc<Archive>,
    original_path: PathBuf,
}
impl RetainedProject {
    pub fn open(executable: &Path, source: &Path) -> Result<Self> {
        let original_path = std::path::absolute(source).map_err(|e| e.to_string())?;
        let archive = Archive::capture(&original_path)?;
        let project = spp_worker::read_project(executable, &archive.path)?;
        if project.source_digest() != archive.digest {
            return Err("retained SPP archive identity mismatch".into());
        }
        Ok(Self {
            project,
            archive,
            original_path,
        })
    }
    pub fn original_path(&self) -> &Path {
        &self.original_path
    }
    pub fn source_digest(&self) -> [u8; 32] {
        self.archive.digest
    }

    /// Write a genuine SPP recovery/Save-As copy, with an explicit destination revision.
    /// A successful return supplies a fresh retained context for repeated saves.
    /// On failure the destination may already have committed; inspect it before retrying.
    pub fn save_copy(
        &self,
        executable: &Path,
        target: &Path,
        revision: SppTargetRevision,
    ) -> Result<Self> {
        self.archive.reject_alias(target)?;
        let committed_digest = spp_worker::save_project_copy_guarded(
            executable,
            &self.archive.path,
            target,
            &self.project,
            revision,
        )?;
        let fresh = Self::open(executable, target)
            .map_err(|e| format!("SPP committed but context refresh failed: {e}"))?;
        if fresh.source_digest() != committed_digest
            || fresh.project.records != self.project.records
            || fresh.project.scene.encode().map_err(|e| e.to_string())?
                != self.project.scene.encode().map_err(|e| e.to_string())?
        {
            return Err("SPP committed but destination changed before context refresh".into());
        }
        Ok(fresh)
    }
    /// Protect the original destination even though the base archive is private.
    pub fn save_original(&self, executable: &Path) -> Result<Self> {
        self.save_copy(
            executable,
            &self.original_path,
            SppTargetRevision::Digest(self.archive.digest),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "subcraft-retained-test-{}.spp",
            uuid::Uuid::new_v4()
        ));
        let mut bytes = b"\x89HDF\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(b"opaque original artist data");
        std::fs::write(&path, bytes).unwrap();
        path
    }
    #[test]
    fn archive_survives_source_deletion_and_clones_and_is_private() {
        let source = fixture();
        let archive = Archive::capture(&source).unwrap();
        let captured = std::fs::read(&archive.path).unwrap();
        let path = archive.path.clone();
        let directory = archive.directory.clone();
        assert_eq!(
            std::fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(archive.reject_alias(&path).is_err());
        let alias = directory.join("alias.spp");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(archive.reject_alias(&alias).is_err());
        let cloned = archive.clone();
        drop(archive);
        std::fs::remove_file(source).unwrap();
        assert_eq!(std::fs::read(&cloned.path).unwrap(), captured);
        drop(cloned);
        assert!(!directory.exists());
    }
    #[test]
    #[allow(unsafe_code)]
    fn capture_rejects_fifo_without_waiting_for_a_writer() {
        let path =
            std::env::temp_dir().join(format!("subcraft-retained-fifo-{}", uuid::Uuid::new_v4()));
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid terminated filename; creates only this test's private FIFO.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(Archive::capture(&path).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn capture_rejects_nonarchives_and_sparse_oversized_files() {
        let source = fixture();
        std::fs::write(&source, b"not an SPP archive").unwrap();
        assert!(Archive::capture(&source).is_err());
        OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(MAX_CONTAINER_BYTES + 1)
            .unwrap();
        assert!(Archive::capture(&source).is_err());
        std::fs::remove_file(source).unwrap();
    }
}
