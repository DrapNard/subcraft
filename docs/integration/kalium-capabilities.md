# Complete Kalium capability transfer

Owner clarification, 2026-10-20: **SubCraft is the main app and must receive all
useful Kalium functionality.** Import and archive safety alone are insufficient.
The first priority is genuine editable SPP and everything required for it.

A row is complete only with evidence for the full artist workflow. Existing
SubCraft source or historical tests establish a candidate equivalent, not
integration acceptance. Nothing below is waived by choosing a different engine.

## Priority 0 — standalone SPP

| Capability | Current evidence | Required next evidence |
|---|---|---|
| Real editable open/save | Missing in both apps | SubCraft → Painter → SubCraft edited round trip |
| Container/record contract | Owned HDF5 fixtures; controlled layer differences | Bounded semantic decoder and checksum verification |
| Layers, channels, masks, fills, materials and mesh | SubCraft models exist; SPP mappings missing | Editable data matches oracle state and exported maps |
| Opaque and unknown records/resources | Preservation policy only | Unknown objects survive known edits byte/semantically intact |
| Atomic save and original preservation | Native safety changes pending validation | Failed/canceled SPP writes preserve original; real SPP staging |
| Full/incremental saves | Painter-created oracle fixtures verified historically | Supported parser/writer corpus in both directions |
| Unsupported/corrupt data | No generic semantic decoder yet | Bounded rejection without source mutation |

## Full transfer checklist

| Kalium capability | Candidate SubCraft equivalent / port status |
|---|---|
| Eight PBR channels and linear color/data conventions | Existing 13-channel model; import/composition parity pending |
| Paint/Fill/Group, masks, visibility/opacity/channel switches | Existing richer layers/effects; nested migration parity pending |
| Normal, arithmetic and color blend modes | Existing 32-mode family; exact legacy semantics pending |
| AO-linked seeded procedural Roughness graph | Typed graph/compositor port in progress; schema/UI/migration acceptance pending |
| 3D/UV brushes, alpha tips, eraser and stroke history | Existing projection/tiled brush engine; equivalent workflow evidence pending |
| UV-boundary clipping, seam strokes and UV pan/zoom | Existing engine candidates; numerical/input parity pending |
| Tolerance flood fill, clear/reset, masks | Audit exact flood-fill behavior; polygon fill alone is not equivalent |
| Native brush presets and bounded `.kbrush` import | Existing SubCraft presets; Kalium preset migration pending |
| OBJ/glTF/GLB, material slots and bindings | Existing importers; shared binding semantics pending |
| Extra UV sets and supplied tangent handedness | Kalium retains these; mapping/persistence parity pending |
| Multiple independent Texture Sets and per-set resizing | Existing sets; shared/unbound source sets and selections pending |
| CPU reference rendering/picking and headless preview | Existing CPU depth/picking plus GPU renderer; offline reference workflow audit pending |
| Experimental compute stamp tool | Existing Kalium prototype; compatibility/validation decision pending, not GPU painting |
| AO/world-normal/position/material-ID/high-low bakes | Existing richer baker; exact CLI options, transforms and sidecars pending |
| Deterministic seeds, cancellation and work/memory budgets | Existing candidates; transferred edge cases pending |
| Generic, Godot ORM, Unity metallic, glTF exports | Existing template engine; exact profiles/settings/parity pending |
| Painter Python export preset JSON | Port pending; not SPP or SPEXP support |
| Export resolution, normal convention, alpha routing, padding | Existing partial equivalents; all options/parity pending |
| Headless inspect/validate/export-all/preview/UV/bake commands | Port pending; GUI actions do not substitute for automation |
| SPP identification and brush inspection CLI | Port pending, with explicit limits |
| Kalium v1–v9 migration, stable IDs and selection | Partial v9 adapter; legacy and all semantics pending |
| Unknown field retention and decoded/extension budgets | Bounds port in progress; rejection is not retention parity |
| Durable edit journal and bounded replay | Missing; adapt to SubCraft Commands/History |
| Checkpoint generations, digest identity, project locks | Snapshot recovery exists; journal/generation guarantees pending |
| Rejected recovery preservation and manual checkpoint | Audit/port pending; prove crash/reopen behavior |
| Recent projects, missing entries, atomic preferences | Existing navigation; bounded/forward-compatible persistence pending |
| Compact functional dock UI and independent pane restore | Existing docks; workspace-version/empty-layout/modal safeguards pending |
| Persistent artist Activity pane/log | Missing; separate from undo history and diagnostics |
| CLI/artifact/downstream engine acceptance | Kalium tests exist; transfer to the combined app pending |
| Source packaging, Linux/Windows/macOS checks and licenses | Fork CI updated; current combined results pending |

## Acceptance rule

Every row needs source integration or a demonstrated equivalent, regression
coverage, persistence/recovery where relevant, and actual runtime evidence for
UI/rendered behavior. Maintain explicit differences until resolved. SPP success
requires actual edited files accepted by Painter; compilation, detection,
flattening, renaming archives, unchanged-byte copying and a Painter-dependent
bridge do not satisfy it. The original full S0–S15 goal remains active.
