# SPP controlled fixture investigation

Updated 2026-10-20. **SubCraft cannot yet open or save complete editable SPP projects.**
A controlled externally edited SPP has now reopened in Painter; details below.
The owner has made standalone SPP editing the sole target project-format workflow.

## Reproduction and provenance

`scripts/research/painter_spp_fixture.py --painter /absolute/path/to/Painter`
uses the documented local Python endpoint with an original four-vertex quad.
It refuses an existing endpoint, verifies the remote PID before creating a
project, refuses an already open project, and terminates only its own process.
It creates a private temporary directory, full-saves a baseline and a copy,
then reopens the baseline and checks that it is editable and unchanged on disk.
Responses, file reads and startup/operation time are bounded. This is an oracle
harness, not a runtime dependency or an SPP implementation.

Observed reference: Painter **11.1.3**, macOS ARM64, Python API **0.3.5**.
Executable SHA-256:
`43c14c7a679c5bde792460dca975a801e8fd2e1978d06a4e2fb0eb987353bf08`.
Successful runtime evidence:
`ev_e2be826e33200d9e4b873e1c09c1e48feac9eaaaf1c1a5852e8266a909463314`.
REA bound the installed executable with Ghidra available; no decompiled source
was used for these findings.

| Private artifact | Bytes | SHA-256 |
|---|---:|---|
| baseline-full.spp | 12783808 | `3098ac69e4db08cea155dd67902c128f93d5e6d01d31dad8b51e6500e361e34c` |
| same-project-full-copy.spp | 12783855 | `8b89ae403298d2c8ceaf8597a753d8ab00eb61dbcffaffd690e28865fd092a75` |
| paint/document.bin | 2352 | `a376fda81cc46eef28d22da11d3d1923067b4220f6008ee740b62d11b6eb5664` |

Original OBJ SHA-256:
`456971e59144b40afecc9d7d9ab75e3ea953c7a1e821386e65226e49e060ae14`.
The original geometry is embedded in the harness. Fixtures include Painter's
bundled resource payloads and remain private: do not commit SPPs, shaders or
images. Only the owned document/mesh metadata was extracted for investigation;
resource datasets were inventoried by name/size, not extracted.

## Observations and limits

- These two full saves have an HDF5 signature at byte zero. A bounded `h5dump`
  inventory with HDF5 plugins disabled recognizes the container. This does not
  establish compatibility across Painter versions or incremental saves.
- The container separates `/paint/document.bin`, `/paint/scene.assbin`, editor
  state, project settings, resources and texture-cache datasets. Observed byte
  datasets have an `m3_x64_128` two-element U64 attribute. Its name suggests a
  hash, but algorithm, seed, coverage and byte order remain unverified.
- `document.bin` begins `dd 2f 7c 1b`. Little-endian header words include 2352,
  matching its length, and 11, matching visible `Data...` declarations. Their
  semantics remain unproven. Length-prefixed names include `DataDocument`,
  `DataMaterial`, `DataChannel`, `DataStackLayers`, `DataLayerColor`, and
  properties such as `materials`, `maskEnabled`, `perChannelBlending` and `uid`.
  These are evidence of structured state, not a decoded layer graph.
- The mesh dataset begins `ASSIMP.binary-dump.` but reports version **1.7**.
  Its payload does not contain the current upstream scene chunk identifier.
  Upstream Assimp's reader requires **1.0**; shared magic is insufficient to
  claim standard ASSBIN import. The companion original OBJ establishes the
  fixture geometry independently.
- `projectsettings.ini` is binary despite the suffix; UTF-16-like version keys
  are visible. A Qt-style encoding is a hypothesis, not an established parser.
- The public project UUID changed after close/open in this run. An initial
  harness assertion incorrectly assumed stability; it was replaced with
  recording both observations and checking the file digest. Do not infer
  persistent document identity from this API alone.
- Full-copy bytes differ from baseline bytes. Neither a byte difference nor an
  unchanged-byte copy demonstrates a writable semantic format.

## Next acceptance work

1. Generate controlled differential fixtures: one layer rename, opacity change,
   fill, mask, channel and incremental save at a time; retain oracle state.
2. Establish bounded document declarations/instances and mesh schema before
   implementing semantic import. Verify checksum hypotheses against fixtures.
3. Choose an HDF5 integration that preserves unknown objects and is buildable
   on supported platforms; reject external links and unsupported semantics.
4. Implement genuine edits and atomic save, then reopen in Painter and compare
   editable layer state and exported maps. Repeat Painter → SubCraft → Painter →
   SubCraft, including failure preservation. Until then SPP support is incomplete.

## Public contracts consulted

- [Painter project API](https://experienceleague.adobe.com/en/docs/substance-3d-dev/painter-python/api/substancepainter-package/project): create/open, editable state, full/incremental save modes and save-as-copy.
- [Painter remote control](https://experienceleague.adobe.com/en/docs/substance-3d-dev/painter-python/tutorials/remote-control): local scripting endpoint and request encoding.
- [Painter command lines](https://experienceleague.adobe.com/en/docs/substance-3d-painter/using/pipeline-and-integration/configuration/command-lines): launch options.
- [Assimp binary definitions](https://github.com/assimp/assimp/blob/master/code/Common/assbin_chunks.h) and [loader](https://github.com/assimp/assimp/blob/master/code/AssetLib/Assbin/AssbinLoader.cpp): comparison against the public 1.0 contract, not a schema for the observed 1.7 payload.

## Controlled layer variants

The harness `--layer-variants` option was actually run on Painter 11.1.3.
Evidence: `ev_9103b526e1ee5f9a8ea8d63430f4086bab29c70c1cff9b7c187364633aea473c`.
Starting with one visible layer at opacity 1, it saves a rename to `Kalium01`,
then Base Color opacity 0.375, then visibility false as separate full copies.
A new full save is subsequently changed to visible and saved incrementally.
All four files reopen editable with exactly the recorded name, opacity and
visibility, and unchanged archive digests. The private corpus is in
`/tmp/kalium-spp-variants`; its manifest contains hashes and expected states.
This validates the oracle corpus, not Kalium editing or a decoded format.

Independent checksum investigation: standard MurmurHash3 x64_128 seed zero on
the first fixture's entire raw `document.bin` yields
`d2cd27a722e3420f c768b76a4b9c66f9`, unlike the observed attribute
`cbdf8b6a556e4bba 7ee9df3eb0736710`. Swapping output words and common byte-order
representations does not match. Other seeds/input coverage remain unknown.
The public-domain [SMHasher reference](https://github.com/aappleby/smhasher/blob/master/src/MurmurHash3.cpp)
was the comparison contract; no Adobe hash implementation was inspected.

## SubCraft priority checkpoint — 2026-10-20

SubCraft is the main app. The owner requires all useful Kalium capabilities,
with SPP and its dependencies first; the complete merge is still in progress.

A reusable bounded declaration probe is now in
`scripts/research/spp_reflective_probe.py`. It reads at most 64 KiB, validates
the observed header version/size and scans capped class/property declarations;
it does not claim to follow the object graph. Unknown property codes remain
reported as unknown. Three original synthetic tests cover bounds, conflicting
schemas and unknown-code retention. The probe was rerun on four private variants.

Controlled byte comparison (one owned 11.1.3 fixture, not a general schema):
visibility toggles a single byte at `0x8bf` between `ff` and `00`. The opacity
edit creates one 20-byte candidate `DataBlending` value record containing class
index 10, mode 2, channel mask 1 and float 0.375. The label is length-prefixed.
Full and incremental saves representing the same visible/0.375-opacity state
have identical document dataset digests. Container offsets change independently.

The opacity document's `m3_x64_128` attribute is
`5646412abc127b90 03c900ca1c628b3f`. A bounded standard MurmurHash3 x64-128
comparison of six common seeds, limited header/suffix removal and length-prefix
representations did not match; results agreed with FFmpeg libavutil's independent
implementation. This strengthens only that bounded negative, not another hash
hypothesis. Checksum coverage/seed and general object/reference grammar remain
unknown. No edited SPP has yet been accepted by Painter.


## 2026-10-20: bounded reflective dataset codec

`scripts/research/spp_document_codec.py` now walks class declarations and object
boundaries rather than fixed layer offsets. On the five privately retained
baseline/rename/opacity/hidden/incremental owned fixtures, decode/reencode is
byte-identical (2352 or 2372 bytes). Renaming the baseline layer to the oracle
name reproduces the rename dataset exactly; disabling the opacity fixture layer
reproduces the hidden dataset exactly. Adding the observed Base Color opacity
record at 0.375 also reproduces the opacity dataset exactly. No private datasets
are committed.

Observed framing: pointer objects carry absolute end offsets and class indices;
first class occurrences carry inline property definitions. By-value array members
omit the end-offset word. Type 15 and 21 consume eight bytes in this corpus;
boolean values are 00/ff. Nonempty type-11 tags and unknown property codes reject.
Header words whose meanings remain unknown are retained unchanged. These are
limited observations from blank paint-layer fixtures, not a general format spec.

Six original synthetic research tests pass, including exact reencoding, semantic
edits, truncation and bounds. This is **dataset-level research only**. HDF5
checksums, opaque resource/container preservation, mesh and broader layer/action
semantics and edited SPP files reopened in Painter remain unresolved. The codec
is not wired as an SPP importer or writer in the application.


## 2026-10-20: checksum verified and edited fixture reopened

The standard MurmurHash3 x64/128 mixing arithmetic was independently inverted
for each of six owned fixture datasets. All six recover the same initial state:
`(0xF13A0239, 0xF13A0239)`. Forward hashing the raw dataset bytes with that seed
matches both ordered unsigned 64-bit attribute words in every fixture. Swapped
word/byte interpretations do not yield a shared initial state. The synthetic
inverse probe is in `scripts/research/spp_hash_probe.py`; its tests cover known
vectors and all short tail lengths, without brute-forcing a seed range.

This agrees with the independently published, unofficial interoperability
[checksum helper](https://github.com/Yeusepe/Universal-Painter-Files/blob/main/spp_downgrader/spp_builder/spp_builder.py#L1186-L1190).
The algorithm contract remains the public-domain
[SMHasher reference](https://github.com/aappleby/smhasher/blob/master/src/MurmurHash3.cpp),
not proprietary Adobe code. This observation establishes the checksum for the
observed U8 document datasets, not all possible HDF5 datatypes or future formats.

`crates/project/src/spp_document.rs` ports the bounded neutral record codec to
Rust. It preserves reserved header words, unknown property names of supported
types, and raw float bits, recomputes object offsets and rejects unresolved
encodings. Six private document extracts reencode byte-for-byte in Rust.
`crates/project/src/spp_checksum.rs` implements the verified checksum parameters.
No project/container importer is exposed through these experimental modules.

An owned baseline copy was changed outside Painter: its layer name was encoded
as `SubCraft`, and its document checksum recalculated. The file size stayed
12,783,808 bytes, with 27 byte differences (name, checksum, and HDF5 object-header
metadata). Source SHA-256 stayed
`ab393eb6c32ccd0eb096241a39db2ef3a08dd57599d97b8389a6fc685b2cb56e`;
edited SHA-256 was
`f1f859e9ca1458ef0f16881a306811587838d8b9ad986a5f1d5c7977b474af47`.
The private research patch used the installed HDF5 C library on a copied fixture;
it never replaced the original or exported resource datasets.

REA controlled runtime evidence:
`ev_8a0a3d89b5079e0e864183c1b9323c4194c780f80fd4b599d24860c77c0f0e7c`.
The owned Painter process reopened the edited copy in edition state, reported
`["SubCraft", true, 1.0]` for layer name/visibility/Base Color opacity, closed it,
and confirmed the edited archive digest was unchanged by opening. Probe exit 0.
Process-tree observation was incomplete and unrelated processes were left alone;
no filesystem snapshot was requested in REA. The probe's explicit hash checks
are narrower evidence than a full filesystem trace.

**Remaining acceptance:** actual SubCraft SPP container I/O and document/mesh/
resource mapping, nonblank strokes/fills/masks/channels, unknown-data preservation,
atomic save/failure handling, and reverse edited round trips. This successful
single-name fixture edit is not general SPP support or app open/save acceptance.


Current verification: 43 project unit tests pass, including seven record-codec
and two checksum tests. The opt-in private Rust corpus test passes for all six
owned extracts, checking both exact reencoding and the HDF5 attribute checksum.
Eight synthetic Python research tests pass. Full workspace compilation passes.
All local heavy validation uses nice 19 and four CPUs. The next container work
is specified in [the integration checkpoint](spp-container-plan.md).


## Native property round trip — 2026-10-20

The optional Rust `spp-hdf5` backend edited an owned SPP copy: layer name
`SubCraft`, visibility false and Base Color opacity 0.625. Painter reopened it,
reported those values, changed them to `Painter2`, true and 0.25, and saved a
full copy. Rust decoded the returned properties correctly. The original source
and Rust-edited input were unchanged by the corresponding copy operations.

Rust output SHA-256:
`239484a5719f93146d1be4425c07018869e6aae3d1e1c79c2bd4484cb8db53ec`.
Painter return SHA-256:
`eb77c4d7b398cb26339c90b9f8307890aa6ee4bf71edf38d1befc4d0e71e8c39`.
REA evidence: `ev_9fe617b718c49cab8033a70a844dc4fb89e9558a9e4c5a6bc6807b48a4870175`, exit 0.
The return records identify last-save version 12.1.5; their source creation
version was 11.1.3. Executable identity was unavailable to this probe, process
sampling was incomplete, and filesystem snapshots were not collected. Explicit
probe SHA checks provide narrower evidence than a complete filesystem trace.

The returned document introduced 16-byte record types 4 and 22. Both now round
trip as opaque bytes; the public Universal Painter Files primitive tables and
complete owned-record boundaries corroborate the widths. Layer Base Color
mapping accepts only the observed exact type-22 token; unknown combinations
reject. This does not establish arbitrary channel semantics.

Verification: 65 project unit tests and three private corpus integration tests
pass with `spp-hdf5`, including reverse properties, archive preservation and
stale-source rejection. Private fixtures remain outside Git.

Five original controlled mesh fixtures (baseline, vertex, UV, normal and triangle
changes) were saved by an owned Painter 12.1.5 process. Evidence:
`ev_054dfcf6ad9a5dcc995faf490770404ce2be8c38bf9f76b1c9d05c4a163a1052`, exit 0.
The harness and synthetic tests are committed; mesh decoding is not implemented.

Remaining: mesh/resource/Session mapping, strokes/fills/masks, broader channel
coverage, length-changing writes, isolated native parsing and full editor
open/edit/save acceptance. USPP is the subsequent phase; see [audit](uspp-format.md).
