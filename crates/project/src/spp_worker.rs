//! One-request native SPP worker. Resource isolation, not a privilege sandbox.
//! The editor-facing host decodes bounded Rust payloads; HDF5 is entered only in
//! the hidden worker dispatch. Linux limits are required before any file is parsed.
use crate::{spp_container, spp_document::SppDocumentRecords, spp_scene::SppScene};
use std::{
    ffi::OsString,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
const MAGIC: &[u8; 8] = b"SCSPP001";
const READ: u8 = 1;
const SAVE: u8 = 2;
const ACK: u8 = 3;
const FAILURE: u8 = 4;
const MAX_DOCUMENT: usize = crate::spp_document::MAX_DATASET_BYTES;
const MAX_SCENE: usize = 64 * 1024 * 1024;
const MAX_ERROR: usize = 4096;
const MAX_FRAME: usize = 49 + MAX_DOCUMENT + MAX_SCENE;
const TIMEOUT: Duration = Duration::from_secs(120);
type Result<T> = std::result::Result<T, String>;

/// Source identity travels with both editable datasets. Unknown records remain intact.
#[derive(Clone, Debug)]
pub struct WorkerProject {
    pub records: SppDocumentRecords,
    pub scene: SppScene,
    source_digest: [u8; 32],
}
impl WorkerProject {
    pub fn source_digest(&self) -> [u8; 32] {
        self.source_digest
    }
}
#[derive(Debug)]
struct Frame {
    kind: u8,
    digest: [u8; 32],
    document: Vec<u8>,
    scene: Vec<u8>,
}
impl Frame {
    fn empty(kind: u8) -> Self {
        Self {
            kind,
            digest: [0; 32],
            document: Vec::new(),
            scene: Vec::new(),
        }
    }
    fn read(mut input: impl Read) -> Result<Self> {
        let mut header = [0; 49];
        input
            .read_exact(&mut header)
            .map_err(|e| format!("worker header: {e}"))?;
        if &header[..8] != MAGIC {
            return Err("invalid SPP worker protocol".into());
        }
        let kind = header[8];
        let document_len = u32::from_le_bytes(header[9..13].try_into().unwrap()) as usize;
        let scene_len = u32::from_le_bytes(header[13..17].try_into().unwrap()) as usize;
        let digest = header[17..49].try_into().unwrap();
        let valid = match kind {
            READ => {
                (document_len == 0 && scene_len == 0 && digest == [0; 32])
                    || ((24..=MAX_DOCUMENT).contains(&document_len)
                        && (512..=MAX_SCENE).contains(&scene_len))
            }
            SAVE => {
                (24..=MAX_DOCUMENT).contains(&document_len)
                    && (512..=MAX_SCENE).contains(&scene_len)
            }
            ACK => document_len == 0 && scene_len == 0,
            FAILURE => {
                (1..=MAX_ERROR).contains(&document_len) && scene_len == 0 && digest == [0; 32]
            }
            _ => false,
        };
        if !valid {
            return Err("invalid SPP worker frame lengths or kind".into());
        }
        let mut document = vec![0; document_len];
        let mut scene = vec![0; scene_len];
        input
            .read_exact(&mut document)
            .and_then(|_| input.read_exact(&mut scene))
            .map_err(|e| format!("worker payload: {e}"))?;
        let mut extra = [0];
        if input.read(&mut extra).map_err(|e| e.to_string())? != 0 {
            return Err("trailing SPP worker bytes".into());
        }
        Ok(Self {
            kind,
            digest,
            document,
            scene,
        })
    }
    fn write(&self, mut output: impl Write) -> Result<()> {
        output
            .write_all(MAGIC)
            .and_then(|_| output.write_all(&[self.kind]))
            .and_then(|_| output.write_all(&(self.document.len() as u32).to_le_bytes()))
            .and_then(|_| output.write_all(&(self.scene.len() as u32).to_le_bytes()))
            .and_then(|_| output.write_all(&self.digest))
            .and_then(|_| output.write_all(&self.document))
            .and_then(|_| output.write_all(&self.scene))
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())
    }
}

/// `executable` must be the trusted SubCraft executable with its pre-GUI worker dispatch.
pub fn read_project(executable: &Path, source: &Path) -> Result<WorkerProject> {
    let response = run(
        executable,
        &[source.as_os_str().to_owned()],
        Frame::empty(READ),
        TIMEOUT,
    )?;
    if response.kind != READ || response.document.is_empty() {
        return Err("unexpected SPP read response".into());
    }
    Ok(WorkerProject {
        records: SppDocumentRecords::decode(&response.document).map_err(|e| e.to_string())?,
        scene: SppScene::decode(&response.scene).map_err(|e| e.to_string())?,
        source_digest: response.digest,
    })
}

/// Preserve the source revision and commit both datasets together. Reread the
/// saved path to establish a fresh context before a subsequent save to that path.
pub fn save_project_copy(
    executable: &Path,
    source: &Path,
    target: &Path,
    project: &WorkerProject,
) -> Result<()> {
    let request = Frame {
        kind: SAVE,
        digest: project.source_digest,
        document: project.records.encode().map_err(|e| e.to_string())?,
        scene: project.scene.encode().map_err(|e| e.to_string())?,
    };
    let response = run(
        executable,
        &[source.as_os_str().to_owned(), target.as_os_str().to_owned()],
        request,
        TIMEOUT,
    )?;
    if response.kind != ACK {
        return Err("unexpected SPP save response".into());
    }
    Ok(())
}

/// Save a retained base archive only if the destination has the expected revision.
pub fn save_project_copy_guarded(
    executable: &Path,
    source: &Path,
    target: &Path,
    project: &WorkerProject,
    revision: spp_container::SppTargetRevision,
) -> Result<[u8; 32]> {
    let guard = match revision {
        spp_container::SppTargetRevision::Absent => "absent".to_owned(),
        spp_container::SppTargetRevision::Digest(digest) => {
            digest.iter().map(|byte| format!("{byte:02x}")).collect()
        }
    };
    let request = Frame {
        kind: SAVE,
        digest: project.source_digest,
        document: project.records.encode().map_err(|e| e.to_string())?,
        scene: project.scene.encode().map_err(|e| e.to_string())?,
    };
    let response = run(
        executable,
        &[
            source.as_os_str().to_owned(),
            target.as_os_str().to_owned(),
            guard.into(),
        ],
        request,
        TIMEOUT,
    )?;
    if response.kind != ACK {
        return Err("unexpected guarded SPP save response".into());
    }
    Ok(response.digest)
}
fn parse_revision(value: &std::ffi::OsStr) -> Result<spp_container::SppTargetRevision> {
    let text = value.to_str().ok_or("invalid SPP destination revision")?;
    if text == "absent" {
        return Ok(spp_container::SppTargetRevision::Absent);
    }
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid SPP destination digest".into());
    }
    let mut digest = [0; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(spp_container::SppTargetRevision::Digest(digest))
}

// The parent owns snapshot cleanup even if the parser crashes or is killed.
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("subcraft-spp-worker-{}", uuid::Uuid::new_v4()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|e| format!("SPP worker scratch: {e}"))?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(executable: &Path, paths: &[OsString], request: Frame, timeout: Duration) -> Result<Frame> {
    let scratch = Scratch::new()?;
    let mut command = Command::new(executable);
    command
        .arg("--spp-worker")
        .args(paths)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // mimalloc v3 otherwise reserves 1 GiB before main, exceeding RLIMIT_AS.
        // Its size options are KiB; keep the parser's actual address-space cap meaningful.
        .env("MIMALLOC_ARENA_RESERVE", "32768")
        .env("MIMALLOC_RESERVE_OS_MEMORY", "0")
        .env("MIMALLOC_RESERVE_HUGE_OS_PAGES", "0")
        .env("TMPDIR", &scratch.0)
        .env("TMP", &scratch.0)
        .env("TEMP", &scratch.0)
        .env("RAYON_NUM_THREADS", "4")
        .env("OMP_NUM_THREADS", "4");
    #[cfg(target_os = "linux")]
    close_inherited_fds(&mut command);
    let mut child = {
        // This is the binding's Rust mutex, without entering/initializing HDF5.
        // Keep HDF5 file close/reopen calls from racing fork-to-exec: a C library
        // descriptor inherited briefly by the child can otherwise retain a file lock.
        let _spawn_guard = hdf5_sys::LOCK.lock();
        command.spawn()
    }
    .map_err(|e| format!("start SPP worker: {e}"))?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let writer_sender = sender.clone();
    std::thread::spawn(move || {
        let _ = writer_sender.send((false, request.write(stdin).map(|_| Vec::new())));
    });
    let reader_sender = sender.clone();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take((MAX_FRAME + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                if bytes.len() > MAX_FRAME {
                    Err("SPP worker output limit".into())
                } else {
                    Ok(bytes)
                }
            });
        let _ = reader_sender.send((true, result));
    });
    // Drain stderr without retaining project bytes or allowing an unbounded buffer.
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut stderr.take(u64::MAX), &mut std::io::sink());
    });
    drop(sender);
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let mut output = None;
        let mut written = false;
        loop {
            while let Ok((is_output, message)) = receiver.try_recv() {
                let bytes = message?;
                if is_output {
                    output = Some(bytes);
                } else {
                    written = true;
                }
            }
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                if !status.success() {
                    return Err(format!("SPP worker exited with {status}"));
                }
                if written && output.is_some() {
                    let response = Frame::read(output.take().unwrap().as_slice())?;
                    if response.kind == FAILURE {
                        return Err(String::from_utf8(response.document)
                            .map_err(|_| "invalid worker error encoding")?);
                    }
                    return Ok(response);
                }
            }
            if Instant::now() >= deadline {
                return Err("SPP worker timed out".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    // Always reap, including malformed output, broken pipe and timeout paths.
    let _ = child.kill();
    let _ = child.wait();
    result
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn close_inherited_fds(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: pre_exec calls only the async-signal-safe close_range syscall; it
    // does not allocate or acquire a lock. CLOEXEC preserves Rust's exec-error pipe
    // until exec while closing every non-stdio descriptor in the executed worker.
    unsafe {
        command.pre_exec(|| {
            if libc::syscall(
                libc::SYS_close_range,
                3_u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Hidden native entry point. Must execute before application logging/GUI setup.
pub fn worker_main(paths: &[OsString]) -> Result<()> {
    apply_limits()?;
    let response = (|| {
        let request = Frame::read(std::io::stdin().lock())?;
        match (request.kind, paths) {
            (READ, [source]) if request.document.is_empty() => {
                let project =
                    spp_container::read_project(Path::new(source)).map_err(|e| e.to_string())?;
                Ok(Frame {
                    kind: READ,
                    digest: project.source_digest(),
                    document: project.records.encode().map_err(|e| e.to_string())?,
                    scene: project.scene.encode().map_err(|e| e.to_string())?,
                })
            }
            (SAVE, [source, target]) | (SAVE, [source, target, _]) => {
                let revision = paths
                    .get(2)
                    .map(|value| parse_revision(value))
                    .transpose()?;
                let records =
                    SppDocumentRecords::decode(&request.document).map_err(|e| e.to_string())?;
                let scene = SppScene::decode(&request.scene).map_err(|e| e.to_string())?;
                let mut project =
                    spp_container::read_project(Path::new(source)).map_err(|e| e.to_string())?;
                if project.source_digest() != request.digest {
                    return Err("SPP source revision changed".into());
                }
                project.records = records;
                project.scene = scene;
                let digest = match revision {
                    Some(revision) => spp_container::save_project_copy_guarded(
                        Path::new(source),
                        Path::new(target),
                        &project,
                        revision,
                    ),
                    None => spp_container::save_project_copy(
                        Path::new(source),
                        Path::new(target),
                        &project,
                    )
                    .map(|_| [0; 32]),
                }
                .map_err(|e| e.to_string())?;
                let mut response = Frame::empty(ACK);
                response.digest = digest;
                Ok(response)
            }
            _ => Err("invalid SPP worker request".into()),
        }
    })();
    let response = response.unwrap_or_else(|error: String| {
        let mut bytes = error.into_bytes();
        bytes.truncate(MAX_ERROR);
        // Truncation may bisect UTF-8. Re-encode before sending a bounded error.
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        while text.len() > MAX_ERROR {
            text.pop();
        }
        let mut frame = Frame::empty(FAILURE);
        frame.document = text.into_bytes();
        frame
    });
    response.write(std::io::stdout().lock())
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn apply_limits() -> Result<()> {
    // Reject an incorrectly launched worker before lowering the address-space ceiling
    // below mappings already reserved by its allocator. The host configures this at exec.
    let statm = std::fs::read_to_string("/proc/self/statm").map_err(|e| e.to_string())?;
    let pages = statm
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .ok_or("SPP worker address-space query failed")?;
    // SAFETY: sysconf takes a constant query and returns no borrowed memory.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0
        || pages
            .checked_mul(page_size as u64)
            .is_none_or(|n| n >= 512 * 1024 * 1024)
    {
        return Err(
            "SPP worker allocator exceeds its address-space budget; launch through the worker host"
                .into(),
        );
    }
    // SAFETY: valid initialized CPU sets/rlimit structs and current-process IDs.
    unsafe {
        let mut allowed: libc::cpu_set_t = std::mem::zeroed();
        let size = std::mem::size_of::<libc::cpu_set_t>();
        if libc::sched_getaffinity(0, size, &mut allowed) != 0 {
            return Err("SPP worker affinity query failed".into());
        }
        let mut selected: libc::cpu_set_t = std::mem::zeroed();
        let mut count = 0;
        for cpu in 0..libc::CPU_SETSIZE as usize {
            if libc::CPU_ISSET(cpu, &allowed) && count < 4 {
                libc::CPU_SET(cpu, &mut selected);
                count += 1;
            }
        }
        if count == 0
            || libc::sched_setaffinity(0, size, &selected) != 0
            || libc::setpriority(libc::PRIO_PROCESS, 0, 19) != 0
        {
            return Err("SPP worker scheduling limits failed".into());
        }
        for (resource, ceiling) in [
            (libc::RLIMIT_AS, 512 * 1024 * 1024),
            (libc::RLIMIT_CPU, 90),
            (libc::RLIMIT_FSIZE, spp_container::MAX_CONTAINER_BYTES),
            (libc::RLIMIT_NOFILE, 32),
            (libc::RLIMIT_CORE, 0),
        ] {
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(resource, &mut current) != 0 {
                return Err("SPP worker limit query failed".into());
            }
            let cap = (ceiling as libc::rlim_t)
                .min(current.rlim_cur)
                .min(current.rlim_max);
            let limit = libc::rlimit {
                rlim_cur: cap,
                rlim_max: cap,
            };
            if libc::setrlimit(resource, &limit) != 0 {
                return Err("SPP worker resource limits failed".into());
            }
        }
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn apply_limits() -> Result<()> {
    Err("SPP worker resource isolation is currently supported only on Linux".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_rejects_truncation_trailing_bytes_and_oversized_lengths() {
        let mut bytes = Vec::new();
        Frame::empty(READ).write(&mut bytes).unwrap();
        assert_eq!(Frame::read(bytes.as_slice()).unwrap().kind, READ);
        for end in 0..bytes.len() {
            assert!(Frame::read(&bytes[..end]).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(Frame::read(trailing.as_slice()).is_err());
        for (offset, value) in [(8, 255), (9, 255), (13, 255), (17, 1)] {
            let mut invalid = bytes.clone();
            invalid[offset] = value;
            assert!(Frame::read(invalid.as_slice()).is_err());
        }
        let mut huge = bytes;
        huge[9..13].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(Frame::read(huge.as_slice()).is_err());
    }
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(unsafe_code)]
    fn host_does_not_inherit_library_file_descriptors() {
        use std::os::{
            fd::{AsRawFd, FromRawFd, OwnedFd},
            unix::fs::PermissionsExt,
        };
        let root =
            std::env::temp_dir().join(format!("subcraft-worker-fd-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let data = std::fs::File::create(root.join("data")).unwrap();
        // SAFETY: duplicate a live descriptor without CLOEXEC to model a C-library fd.
        let inherited = unsafe { libc::fcntl(data.as_raw_fd(), libc::F_DUPFD, 128) };
        assert!(inherited >= 128);
        // SAFETY: fcntl returned a new owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(inherited) };
        let ack = root.join("ack");
        Frame::empty(ACK)
            .write(std::fs::File::create(&ack).unwrap())
            .unwrap();
        let script = root.join("worker");
        std::fs::write(&script, "#!/bin/sh\ncat >/dev/null\nif [ -e \"/proc/self/fd/$2\" ]; then exit 42; fi\nexec cat \"$3\"\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            run(
                &script,
                &[fd.as_raw_fd().to_string().into(), ack.into_os_string()],
                Frame::empty(READ),
                Duration::from_secs(3)
            )
            .unwrap()
            .kind,
            ACK
        );
        drop(fd);
        drop(data);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn host_reaps_timeout_and_rejects_malformed_output() {
        use std::os::unix::fs::PermissionsExt;
        let root =
            std::env::temp_dir().join(format!("subcraft-worker-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let script = root.join("worker");
        std::fs::write(&script, "#!/bin/sh\nexec sleep 10\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let start = Instant::now();
        assert!(
            run(&script, &[], Frame::empty(READ), Duration::from_millis(100))
                .unwrap_err()
                .contains("timed out")
        );
        assert!(start.elapsed() < Duration::from_secs(3));
        std::fs::write(&script, "#!/bin/sh\nprintf garbage\n").unwrap();
        assert!(run(&script, &[], Frame::empty(READ), Duration::from_secs(3)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
