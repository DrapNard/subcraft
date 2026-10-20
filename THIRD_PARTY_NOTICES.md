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


## Optional native SPP container backend

The experimental `spp-hdf5` feature uses `hdf5-metno` 0.14.1 and
`hdf5-metno-sys` 0.12.3 ([upstream](https://github.com/metno/hdf5-rust)),
under their MIT OR Apache-2.0 terms. System-linked builds use the installed
HDF5 library; package its applicable notices with distributed binaries.
The `spp-hdf5-static` feature can build HDF5 from the locked
`hdf5-metno-src` 0.10.4 sources (HDF5 2.2.0). HDF5's full notice is retained in
`licenses/HDF5-LICENSE.txt`; it identifies The HDF Group and the University of
Illinois and includes the 3-clause BSD terms. Static/platform packaging has not
yet been accepted. Default application/browser builds do not enable this backend.

Universal Painter Files was reviewed as an MIT-licensed format reference for
future USPP compatibility. Its source/assets are not bundled or executed by
SubCraft. The pinned audit and links are in `docs/research/uspp-format.md`.
