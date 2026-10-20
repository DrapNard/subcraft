# Kalium → SubCraft integration

**Current format policy (2026-10-20):** the owner abandoned all Kalium formats.
The native `.kalium` importer and in-progress `.kbrush` adapter have been removed.
Useful editor capabilities are still transferred into SubCraft. Genuine SPP
open/edit/save is the priority and remains unimplemented. Earlier migration
notes below are historical and superseded.


## Owner decision and architecture

2026-10-19: analyze both repositories, transfer all useful Kalium capabilities into SubCraft,
rewrite the README, commit and publish. The owner explicitly selected
**Subcraft as the main app**. Destination: `DrapNard/subcraft`.

Base revision: `71e49fde7e4e1dba188a6589c9e5a139d1e3d9d5`.
Kalium reference: `311b551` on `DrapNard/Kalium/main`.
The earlier reverse-direction experiment is preserved only on Kalium's
`integration/kalium-side-subcraft-ports` branch, not represented as this merge.

Keep SubCraft's Session/Command/History, tiled COW f16 image model, per-channel
layer graph, CPU brush/compositor, GPU renderer and UI. No second editor or
whole Kalium dependency is introduced. Ports must meet the existing crate
DAG and preserve original files when an operation fails.

The standalone editable SPP goal remains active. Neither source repository
currently supplies a genuine SPP semantic importer/writer.

## Initial integration work (not the complete merge)

- Port Kalium's bounded ZIP directory preflight principles before constructing
  an allocating ZIP archive; cap entry sizes and aggregate retained data.
- Validate document/tree/raster dimensions and counts before expensive work;
  reject malformed rasters, duplicate tiles, non-finite values and trailing data.
- Use unique sibling save staging and preserve the original on write failure;
  reject SPP filenames before creating or replacing anything.
- Import supported Kalium native documents into editable SubCraft structures;
  source file remains unchanged and Save As is required.
- Route file-dialog, launch, recent-file and desktop drop through the same import
  decision. Protect keyboard actions while unsaved/recovery modals are pending.
- Carry forward the original SPP oracle harness and documented observations.
- Direct fork update validation, issue links and welcome links to DrapNard's
  repository, preserving strict package-origin verification.

## Scope and remaining work

The loader and importer implementation and supported mappings are being
validated in this integration. No completion claim should be based on source
inspection or an agent report alone. Record concrete command/runtime evidence
below before publishing a verified result.

The owner clarified that **all useful Kalium functionality is required**, not
just import and safety. The [capability checklist](kalium-capabilities.md) tracks
missing CLI/export workflows, durable journals/checkpoints, procedural Roughness
graphs, Activity, forward metadata preservation, workspace safeguards and format
semantics. SubCraft equivalents require evidence at the same scope. None of
these gaps is intentionally excluded from completion. SPP and its dependencies
are the first priority; unsupported data must survive or fail explicitly before
a destructive save.

No Adobe resources or new generated images are copied. Original SubCraft art
and per-resource licenses remain as upstream; the integration adds code and
research metadata. Mixed-source licensing is recorded in the root notices and
AGPL license without revoking upstream MIT/Apache permissions.

## Evidence

Current scoped validation: **96 passed, 0 failed** across the SubCraft document,
compositor and project package test targets, including new procedural graph
math/composition, AO dependencies, native graph save/reopen and Kalium graph
reference migration. This is not full-app or SPP acceptance. The probe's three
synthetic Python tests also pass. Workspace compilation (`cargo check --locked --workspace`) also passes.
Full workspace test execution and platform/runtime checks remain separate gates. All heavy commands use nice 19 and four CPUs.
Original upstream results and Kalium's former 614-test result do not prove this
combined app. The complete merge remains in progress.


## 2026-10-20 transfer increment (before format scope change)

Added persistent per-user Activity (separate from undo), bounded Kalium v1/v2
brush import with embedded alpha tips and saved paint/mask behavior, plus CLI
inspect/validate, exports, UV masks and selected-map baking. Brush import is
exposed in Brush presets and Shelf → Brushes. Brush radius conversion is an
approximation across the different engines; unsupported semantics reject.

`cargo test --locked -p subcraft-app --lib --test brush_presets`, through the
low-impact wrapper: **45 passed, zero failed**. This includes source-file
immutability after CLI operations. Six synthetic SPP research tests pass.
Native dialog interaction and Activity screenshots remain unverified. Headless
preview and full transfer rows remain in progress; no full SPP support claim.


## SPP focus after owner clarification

Removed the Kalium project parser and all Kalium-specific open routes, filters
and preset migration work. Explicit load/save guards reject `.kalium` and
`.kbrush`; no native archive is written under those extensions. Core editor
ports remain: Activity, headless workflows and editable Roughness graphs.
Workspace compilation passes after removal (`cargo check --locked --workspace`,
low-impact wrapper). The UI unit-test build was interrupted during dependency
compilation to serialize validation of the new format direction; it is not a
passing UI/runtime result. Genuine SPP read/write remains unimplemented.


Validation after format removal: `cargo test --locked -p subcraft-app
-p subcraft-project --lib --test procedural_graph` passed **76 tests**
(39 app units, 3 graph command tests, 34 project units). Includes explicit
Kalium format rejection and SPP source-preservation guards. Six synthetic Python
research tests pass. All commands ran via the four-core nice-19 wrapper.
