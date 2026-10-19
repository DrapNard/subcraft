# Licensing and source provenance

## Combined SubCraft / Kalium application

The main app is SubCraft, as selected by the owner on 2026-10-19. It incorporates
Kalium project-safety and interoperability work from DrapNard/Kalium.
The combined desktop application and modified `subcraft-app`, `subcraft-ui`,
`subcraft-project`, `subcraft-doc` and `subcraft-compositor` crates are distributed under **AGPL-3.0-only**;
see `LICENSE-AGPL-3.0`. Kalium contribution copyright belongs to DrapNard and
Kalium contributors. Source provenance and supported mappings are recorded in
`docs/integration/kalium-merge.md`.

## Original SubCraft code

Base: DrapNard/subcraft (fork of Aalzard/subcraft), commit
`71e49fde7e4e1dba188a6589c9e5a139d1e3d9d5`.
Copyright (c) Aalzard and the SubCraft contributors.
Original SubCraft code remains available under its MIT OR Apache-2.0 terms;
the complete notices are retained in `LICENSE-MIT` and `LICENSE-APACHE`.
The combined AGPL application does not revoke those original permissions.
Unmodified workspace crates retain their original license declarations.

## Existing bundled resources

Retain the per-resource upstream notices: starter maps are dedicated under CC0
in `assets/starter-pack/CREDITS-CC0.txt`; fonts and icon source notices remain
with their upstream resources. No Adobe source, shaders, branding assets or
bundled project resources are incorporated by this merge. SPP research fixtures
remain private. No images were generated for this integration.
