# SubCraft

**A native, open-source 3D texture painter in Rust.**

[Source](https://github.com/DrapNard/subcraft) · [Getting started](docs/getting-started.md) ·
[Development](docs/development.md) · [Integration status](docs/integration/kalium-merge.md)

This is DrapNard's SubCraft fork, combining **SubCraft as the main application**
with **all useful Kalium functionality**, through verified existing equivalents
and integrated ports. This full feature transfer is still in progress. The editor,
session model, tiled texture engine, GPU viewport and existing tools remain
SubCraft's. We are building one app rather than maintaining two separate editors.

**Status: early development, based on SubCraft 0.1.7.** Expect incomplete features
and format limits. This source integration is not a production release or a
claim of complete Substance Painter compatibility.

## Full feature transfer

| SubCraft foundation | Kalium contributions |
|---|---|
| GPU PBR viewport, HDRI/studio lighting, 2D UV view | Bounded archive and raster loading principles |
| Sparse copy-on-write f16 tiled textures | Decoded-memory, tree and file limits |
| Layer folders, per-channel blending, masks and effects | Failure-safe staging and SPP filename protection |
| Projection brushes, symmetry, pen paths and parts workflow | Procedural Roughness graphs |
| Baking, generators, smart materials, shelf and export | Controlled SPP format research and acceptance evidence |

The [capability checklist](docs/integration/kalium-capabilities.md) tracks every
required area; the [integration record](docs/integration/kalium-merge.md) lists actual support,
verification and remaining differences. Kalium's journal backend, native format
and CPU renderer are not installed as a second application inside SubCraft.

## Priority: real SPP open and save

The target project workflow is **open, edit and save genuine `.spp` files locally**.
**SPP import and writing are not implemented yet.** SubCraft's current editable
format remains `.subcraft` during development. Kalium project and brush formats
are not supported; the interoperability target is genuine `.spp`.
Saving to an `.spp` filename is rejected before writing to protect Painter files.

The carried-over research harness creates private, original-geometry fixtures
through Painter's documented API. Full and incremental saves, layer rename,
opacity and visibility have verified Painter reopen observations. Container
recognition is not a semantic importer. See [SPP research](docs/research/spp-roundtrip.md).
A Painter-dependent bridge or flattened texture extraction does not meet the
standalone editable SPP goal.

## Painting and materials

- Paint and erase directly on a 3D mesh or in the UV view; brush alphas, symmetry,
  lazy mouse, jitter, stencils and polygon fill.
- Editable pen paths with Bézier handles and undo/redo.
- Paint and fill layers, folders, 32 blend modes and per-channel opacity.
- Masks with paint, fill, image, generator, Levels and Invert effects; editable
  content-effect stacks and material projections.
- PBR channels, parts/island/material selection, hiding and isolation.
- Material and smart-mask shelf, personal library, texture-set import and
  existing CC0 starter normals and brush tips.

Painting and compositing are CPU-authoritative. The GPU renders the material
preview and receives changed texture tiles. This is not GPU-native painting.

## Viewport, baking and export

- Metal/roughness PBR, tangent normals, height bump, studio reflections, HDRI
  lighting and channel/mesh-map inspection.
- CPU mesh-map baking: tangent and world normals, position, ID, AO, curvature
  and thickness; optional high-poly cage projection and padding.
- Packed texture exports for common metal/roughness workflows and engines,
  with appropriate color/data channel handling.
- Docked editor panels, undo/history, recent files, autosave and recovery copies.

Upstream feature descriptions and historic measurements are preserved in
[implementation progress](docs/progress.md). They are not substitutes for the
current fork's test and runtime results.

## Run the desktop app

Requires **Rust 1.95 or newer**. SubCraft retains its Rust 2024 workspace and
existing egui/wgpu architecture. No Kalcite installation is required.

```bash
git clone https://github.com/DrapNard/subcraft.git
cd subcraft
cargo run --release
```

Useful launch options:

```bash
cargo run --release -- --demo
cargo run --release -- path/to/model.obj
cargo run --release -- path/to/project.subcraft
```

On Linux, install the native windowing/graphics dependencies described in
[development](docs/development.md). A working Vulkan/Metal/DX12 backend or
supported software renderer is needed. The old browser build is not the target
of this integration.

## Development and verification

For low-impact builds on this workstation, prefix commands with
`scripts/dev/low-impact`. It uses nice 19, four available Linux CPUs and four
build/test workers. Heavy development commands are serialized.

```bash
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo build --locked --release
```

GPU tests may skip when no adapter is available; a skipped render test does not
prove rendering correctness. Native file dialogs, tablet input, all supported
platforms and production performance need separate runtime acceptance. Existing
screenshots are historical unless the integration record explicitly identifies
a new capture.

## Transferred workflows

A separate persistent **Activity** panel records project open/save and export
outcomes. Headless commands include `inspect`, `validate`, `export`, `export-all`,
`uv-mask` and `bake-mesh-maps`; run `subcraft --help` for arguments. These use
SubCraft's session, bake and export systems. SPP remains unsupported.

## License, credits and product policy

The **combined application is AGPL-3.0-only**. The affected app, UI and project
crates carry that declaration; see [AGPL license](LICENSE-AGPL-3.0).
Original SubCraft code by **Aalzard and the SubCraft contributors** retains its
MIT OR Apache-2.0 permissions and notices. Kalium work is by **DrapNard and the
Kalium contributors**. See [source and third-party notices](THIRD_PARTY_NOTICES.md).
Existing starter-pack credits remain with the assets.

The official combined product includes no generative AI image or material
features. AI-assisted development is permitted with review. No proprietary Adobe
source, shaders, icons or bundled resources are introduced by this merge.
SubCraft is independent and is not affiliated with or endorsed by Adobe or
ArtCraft.
