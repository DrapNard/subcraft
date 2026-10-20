# Next SPP container integration

Status: experimental backend checkpoint, 2026-10-20. The optional native
`spp-hdf5` backend now reads and writes the understood document and scene datasets in
staged copies. Complete SPP project open/edit/save in SubCraft is unfinished.
Length-changing writes reject; artist resources and unknown metadata are retained; derived render caches are invalidated on edits.

## Coherent project transactions

`read_project` returns document records and scene decoded from the same bounded
source snapshot, with a retained SHA-256 source identity. `save_project_copy`
encodes both, rejects length changes and stale sources, and writes both changed
datasets plus checksums into one atomic staged copy. Derived caches are
invalidated once using the original records. Both datasets are reopened and
verified before publication. No-op copies avoid native writes and are byte-exact
in the synthetic regression. This is a container API, not Session integration or
full SPP editor support. The combined path has synthetic regression coverage;
Painter runtime evidence currently covers the individual edits described below.

## Established prerequisites

- Bounded Rust reflective dataset decoder/encoder, with exact private-corpus
  roundtrips and synthetic corruption tests.
- Raw U8 document dataset checksums verified across six owned fixtures:
  MurmurHash3 x64/128, seed `0xF13A0239`, ordered unsigned 64-bit words.
- A layer-name edit made outside Painter and rehashed in a copied container
  reopens editable in Painter. See [evidence](spp-roundtrip.md).

## Container backend

The backend pins `hdf5-metno` 0.14.1 and `hdf5-metno-sys` 0.12.3. Local Linux
validation uses the installed HDF5 2.2.0 library. The locked optional source
package is `hdf5-metno-src` 0.10.4, whose headers identify HDF5 2.2.0 (the earlier
online build notes described 2.0.0). Its license is retained under `licenses/`.
The maintained [binding](https://github.com/metno/hdf5-rust) wraps the official C
library; it is not enabled by default or required for browser/community builds.

```sh
scripts/dev/low-impact cargo test --locked -p subcraft-project --features spp-hdf5 --lib
```

`spp-hdf5-static` forwards the binding's static source build feature. It requires
CMake and a C toolchain; static linkage and Windows/macOS packaging have not yet
been verified. Linux CI has a dedicated optional-backend test job. The native
metadata parser is not a sandbox. An opt-in Linux worker now applies resource
limits before parsing; Session/UI integration still remains.

## Native worker boundary

Build the desktop binary with `--features spp-worker` to enable the hidden
`--spp-worker` dispatch before GUI/log initialization. Default and browser builds
remain independent of HDF5. The host API is `spp_worker::{read_project,
save_project_copy}`; it starts the trusted executable directly, without a shell.
The native backend remains available for research callers but is not suitable for
parsing arbitrary files in the GUI process.

Each invocation serves one framed request. It checks the version, operation,
lengths and trailing bytes; document payloads are capped at 64 KiB and scene
payloads at 64 MiB. The host bounds stdout, drains stderr without retention,
enforces a 120-second wall deadline, kills on failure and reaps the child.
The host configures mimalloc to reserve arenas in 32 MiB increments at exec,
rather than its 1 GiB default; otherwise the address-space cap would already be
exceeded before parsing. Worker startup rejects an over-budget address space. The
parent owns a private snapshot directory and removes it even on worker failure.
Linux workers select at most four CPUs from their allowed affinity mask, set
nice 19, and lower hard/soft limits to the smaller of their inherited limit and:
512 MiB address space, 90 CPU seconds, 128 MiB per output file, 32 file descriptors
and no core dumps. Other operating systems reject until an equivalent resource
boundary is implemented. These limits need broader maximum-size corpus coverage.
This is resource/process isolation, not a privilege or filesystem sandbox.

Save requests retain the full source SHA-256, compare it in the worker, and use
the combined atomic container transaction. Re-read the saved file to obtain a
fresh revision for another save. A crash/timeout can happen after atomic publish
and before acknowledgement: callers must inspect the destination rather than
assume no commit occurred. A forcibly killed save may leave its private staging
file beside the destination; parent snapshot cleanup does not remove that file.
SPP Session provenance, recovery and user-facing open/save are still unfinished.

Local acceptance uses the actual worker-enabled desktop executable and an owned
full (not sparse) uniform-fill SPP: read both datasets and source digest, save a
byte-exact no-op copy, change uniform Base Color plus UV together, reopen and
compare both encoded datasets, preserve the source, and reject an external
source-metadata change without changing the existing destination. All ten
private corpus tests and 105 project unit tests pass with `spp-worker`. This is
native-worker acceptance; no additional Painter-render or GUI acceptance is
claimed for the worker in this checkpoint. A `/proc` runtime observation found
one worker thread, four allowed CPUs and ~130 MiB startup virtual address space;
the hard limits match those listed above. The initial 1 GiB-reservation failure
was reproduced before the arena configuration fix.

```sh
scripts/dev/low-impact cargo build --locked -p subcraft --features spp-worker
scripts/dev/low-impact env SUBCRAFT_SPP_RESEARCH_DIR=/absolute/private/corpus \
  SUBCRAFT_SPP_WORKER_EXE=/absolute/path/to/target/debug/subcraft \
  cargo test --locked -p subcraft-project --features spp-worker \
  --lib --test spp_owned_corpus -- --include-ignored
```

## Retained full archives and destination revisions

The Linux `spp_retained::RetainedProject` API captures the complete source into a
private 0700 directory with a 0600 file before native parsing. Its Rust copier
caps archive bytes, checks the signature, hashes the copy, rehashes the same
locked source descriptor and rejects a changing source. Nonblocking open plus
regular-file checks reject FIFOs before the worker timeout is available. The
source handle/lock is released after capture. Shared contexts retain the private
archive until their last clone drops; no unknown resource is reconstructed from
the editable records/mesh. Archive capture is synchronous IO and should run in
an application background job when connected to Session.

`save_copy` requires an explicit destination policy: absent, or an expected full
SHA-256. The worker separately checks the immutable base archive and destination
revision. It validates the destination before staging and rechecks at commit.
Absent destinations publish with an atomic no-clobber hard link; a newly appearing
file is never replaced. Existing destinations retain their permissions. Revision
checks/cooperative locks cannot make replacement a filesystem-wide compare-and-swap
against a noncooperating writer in the final check-to-rename interval.

Guarded saves return the digest of the verified staged archive. The retained API
captures/reopens the saved destination and checks that digest plus both editable
datasets before returning a fresh context for repeated saves. After a successful
publish acknowledgement, refresh failures are reported as committed/context-refresh
failures. A crash before acknowledgement remains ambiguous as described above. An original-path conflict can still
be saved to a new path using the retained base. Attempts to overwrite the private
base itself (including hard-link aliases) reject.

The actual desktop executable passes a private original 12.1.5 uniform-fill case:
delete the source, reject in-place save to the missing original, create a genuine
byte-exact SPP recovery file, change color and UV together, save twice using fresh
contexts, reject stale contexts and external metadata changes, then Save As from
the retained archive. The original owned fixture remains unchanged. This proves
backend retention/recovery-copy behavior, not Session/autosave/UI recovery or
Painter acceptance of a newly expanded feature set.

Worker exec now marks all descriptors above stdio CLOEXEC using Linux
`close_range` (kernel 5.11+), requiring `/proc` for startup resource checks. A raw
Rust binding mutex surrounds spawn until exec completes, without initializing
HDF5 in the host. This prevents native file close/reopen from racing transient
fork inheritance. Regression tests cover an explicitly inheritable library-like
descriptor, destination creation/change during staging, no-clobber publication,
in-place permissions, private-archive lifetime and FIFO rejection. The initial
parallel run exposed an HDF5 lock failure while a worker-test child was alive;
closing inherited descriptors plus the spawn mutex fixes that reproduction.

Remaining integration: map the verified SPP document into Session/history without
flattening, retain this archive in snapshots, persist genuine SPP autosave copies
plus origin/revision metadata, and restore those contexts safely. Broader strokes,
channels, masks, resources, transforms and length-changing writes remain required.

## Preservation and safety contract

1. Open read-only first. Cap source bytes and inspect structural metadata before
   reading datasets. Reject external links/storage, unsupported filters, VLEN
   types and object references in any data to be decoded. Bound object/link/attr
   counts, paths, rank, dimension products, decoded bytes and recursion.
2. Perform native container work away from the UI thread with process memory/time
   limits. Application limits alone do not bound a native library's metadata
   parsing allocations. Disable automatic third-party filter/plugin loading.
3. Stage a byte-for-byte source copy; edit only understood datasets. Keep every
   unrelated group, dataset, attribute and resource. Do not reconstruct the whole
   container from the current neutral document model.
4. Preserve original dataset datatype and creation properties. The observed
   document is contiguous: a different encoded length cannot simply extend it.
   Recreating it requires explicit attribute/property preservation and separate
   Painter acceptance. See the official [dataset guide](https://support.hdfgroup.org/documentation/hdf5/latest/_h5_d__u_g.html).
5. Treat link semantics separately from object copying; [H5Ocopy](https://support.hdfgroup.org/documentation/hdf5/latest/group___h5_o.html)
   does not automatically establish safe preservation of external/soft links.
6. Recalculate the understood checksum, flush/close, reopen and validate staging,
   then fsync and atomically replace. Pre-commit failures preserve the target. A post-replacement directory-sync
   failure returns `CommittedButNotSynced`, explicitly reporting that commit occurred.

## Next acceptance fixtures

Broaden beyond a blank paint layer: original authored strokes, Fill, mask, group,
multiple channels and texture sets, resource references, and incremental saves.
Then map mesh/document data into SubCraft's existing Session without flattening,
retain unsupported artist data, and prove edited roundtrips in both directions.
The controlled three-property Rust → Painter → Rust round trip is a prerequisite
result, not a general format claim.

The optional private Rust test requires six `*.document.bin` extracts and matching
`*.checksum.txt` files containing the two decimal attribute words. No private
Painter data is bundled:

```sh
scripts/dev/low-impact env SUBCRAFT_SPP_RESEARCH_DIR=/absolute/private/corpus \
  cargo test --locked -p subcraft-project --test spp_owned_corpus -- --ignored
```


### Current source identity protection

Read from a private bounded snapshot while retaining a shared source lock.
Disable HDF5 plugins only within the serialized native operation, then restore
previous plugin state. Documents carry a private full-container SHA-256 identity;
reject saves from an older source, including resource-only changes. Recheck the
staged copy and live source before replacement. Preserve output permissions.
These checks detect cooperating and observed noncooperating changes; advisory
locks cannot prevent every hostile external writer race.

Browser CI builds remain enabled. Pages deployment now requires the repository
variable `ENABLE_GITHUB_PAGES=true`, because this fork has no configured Pages
site; this does not change browser application behavior.


### Scene checkpoint

`read_scene_bytes` verifies the bounded contiguous U8 scene dataset and checksum.
`read_scene` and `save_scene_copy` decode/write the understood 1.7 triangle subset
with the same snapshot identity and atomic staging protections as document edits.
Stages are private before copying any payload; output permissions are applied at
commit. Hierarchy references and material indices are checked against their
respective tables. Unknown node metadata, material properties and scene tail
remain lossless. Size-changing scene writes still reject. This is geometry I/O,
not a paint-layer importer or editor Session. See the [adapter contract](spp-mesh-contract.md)
and [Painter UV export/reverse evidence](spp-roundtrip.md).


### Derived-cache invalidation is required for rendered edits

An independently rendered fill regression showed that rewriting a document and
its checksum alone leaves Painter exporting stale pixels. Changed document/scene
saves now remove derived dataset links only under the validated `/texture-cache`
hierarchy. Groups and attributes are retained. No-op saves retain cache payloads.
Explicit persistent cache pins and unsupported cache graphs reject atomically;
artist data outside the cache remains untouched. The controlled fill's exported
PNG changes from RGB `[32,96,159]` to `[64,96,159]`, matching the edited source.
General pinned and painted-layer preservation still needs owned acceptance.
