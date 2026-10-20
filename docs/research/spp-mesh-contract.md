# Native SPP geometry and document adapter contract

Status: integration research, 2026-10-20. This note records the interfaces an
SPP-to-SubCraft adapter must satisfy. It does not establish that SubCraft can
open or save an editable SPP project. The current native SPP backend reads the
understood `document.bin` record subset and performs bounded same-length edits;
the observed triangle geometry, node references and material table now have bounded
codecs, but this does not yet decode a complete project or create a `Session`.

## Existing SubCraft mesh contract

The canonical type is `subcraft_mesh::Mesh` (`crates/mesh/src/mesh.rs`). Its
parallel vertex arrays are `positions: Vec<Vec3>`, `normals: Vec<Vec3>`,
`uvs: Vec<Vec2>` and `tangents: Vec<Vec4>`. Triangles use `indices`; each
triangle has a `tri_material` slot, while `submeshes` group contiguous triangle
ranges by slot. Optional `tri_object` indices refer to `objects`. `materials`
is a list of names; it does not hold imported Painter shaders or PBR settings.

The supported construction route is `MeshBuilder` with `Corner` values, one
`material(name)` per target material slot, optional `object(name)` boundaries,
and `push_triangle(material_slot, [corner; 3])`, followed by `build()` and
`Mesh::validate()`. The builder drops degenerate/non-finite triangles, requires
at least one valid triangle, and fails when every triangle lacks UVs. It fills
missing normals, computes MikkTSpace tangents, deduplicates identical corners,
sorts triangles into material-slot ranges, and removes unused material slots.
That means an adapter must retain its source-material-to-built-slot mapping;
the final slot index is not guaranteed to equal an input material ID when
slots are unused.

Canonical convention is right-handed, +Y up, counter-clockwise fronts, and UV
origin at top-left. OBJ import explicitly flips its bottom-left V coordinate;
glTF input is already used as top-left. The controlled quad and documented Painter mesh export retain OBJ bottom-left UV
values. The reference adapter must flip V to the SubCraft convention; wider
UV-set/UDIM behavior remains unverified. The adapter must establish it with controlled Painter geometry and a
paint/export comparison before flipping or preserving V. It must also establish
scene transforms, winding, units, normals and object/material assignment from
owned fixtures. Do not feed the observed `ASSIMP.binary-dump.` payload to the
standard Assimp 1.0 reader: the fixture reports version 1.7 and does not match
the public 1.0 scene-chunk contract.

The mesh importer API currently supports external OBJ, glTF/GLB and desktop FBX
through `subcraft_mesh::import_mesh`; these are not SPP project import. Meshes
without usable UVs cannot be painted and must fail with a clear diagnostic
unless an independently specified unwrap workflow is added.

## Material slots and texture sets

`Document::from_material_slots(names, resolution)` creates exactly one
`TextureSet` per name, with `material_slot` equal to the corresponding mesh
slot. Each `TextureSet` stores its own UUID, name, resolution, channel
definitions and bottom-to-top layer vector. `TextureSet::new` starts with the
default Metal/Rough channels: Base Color, Height, Roughness, Metallic and
Normal. Other representable `ChannelKind`s include Ambient Occlusion, Emissive,
Opacity, Specular and User0–User3, but their SPP identifiers, component layouts
and texture-set semantics are not yet mapped. SubCraft's `ChannelFormat`
choices are 8-bit, 16-bit, f16 and f32; no SPP-to-format rules have been
established.

Every SPP material/texture set that is imported must map to a valid built mesh
material slot, including a policy for multiple SPP sets that share a material
or unused slots. Preserve set names and channel order/availability where
verified. Painter material shader parameters are not represented by
`Mesh::materials`; preserve them in source-side state or reject unsupported
material constructs instead of silently discarding them.

The external `Session::new(mesh, resolution)` is inappropriate for an imported
SPP document: it creates a fresh default document and adds a new `Layer 1` paint
layer to every set. The integration entry point is
`Session::from_parts(mesh, document, resources, label)`. It asserts the
document has at least one texture set, initializes selection from the first set
and its last top-level layer, builds scene caches, and starts history from the
provided document. Validate nonempty sets and all material-slot references
before calling it; malformed project data must not reach this assertion.

## What the current record semantics can map

`subcraft_project::spp_document::SppDocumentRecords` is a bounded, neutral
record tree, not a SubCraft `Document`. `spp_semantics::enumerate_layers`
currently recognizes only records named `DataLayerColor` with the observed
`enabled`, `label`, `perChannelBlending` and `uid` fields. Its `SppLayerInfo`
contains a numeric source UID, label, enabled flag and Base Color opacity. The
opacity mapping supports the observed `DataBlending` schema with type-12
channel mask `1` or one exact observed type-22 Base Color token. Type-12
combined Base Color masks, unknown type-22 tokens, malformed blending records,
duplicate UIDs and invalid opacity values are rejected. These observations
must not be generalized to every Painter version or channel.

The helper walks nested object-valued fields in encoded field order and returns
the matching color records in depth-first order. It does **not** establish
which enclosing records mean a material, stack, group, mask or paint order.
Therefore this enumeration is not sufficient to populate
`TextureSet.layers`: SubCraft stores each layer stack bottom-to-top and supports
paint, fill and folder nodes, but no inspected SPP structure has yet been
proven to map to that ordering or node kind. A layer adapter must first decode
the structural parent/child/ordering relations and test them against Painter's
visible stack.

SubCraft's `Layer` model also has fields not covered by the current semantic
helpers: per-channel blend modes and opacity, paint rasters or fill sources,
masks/effects, geometry parts, content effects and folder children. Only
visibility, label and observed Base Color layer opacity currently have direct
record editing helpers. Do not synthesize paint layers from `DataLayerColor`
metadata or present an empty layer as an imported paint layer. For an initial
read-only visual preview, explicitly label any unmapped data and keep the
original record tree and container available for save preservation.

## Integration interfaces to add next

1. **Bounded scene decoder:** a project-crate result that owns canonical mesh
   geometry plus explicit material/source mappings. Validate counts, finite
   coordinates, triangle indices, UV availability/range policy, hierarchy
   depth, material references and memory budget before constructing a
   `MeshBuilder`. The `scene.assbin` 1.7 subset is now decoded using controlled differential
   evidence. General transforms, extra components, animation and additional
   scene layouts remain outside the accepted subset.
2. **Document adapter:** accept decoded `SppDocumentRecords`; return a validated
   SubCraft `Document`, `ResourceStore`, and a stable source-UID ↔ `LayerId`
   mapping only for semantics actually understood. Preserve the complete
   neutral source tree alongside the editable mapping. Fail if an unsupported
   SPP construct would be lost or misrepresented.
3. **Session source attachment:** pass the validated mesh/document/resources
   through `Session::from_parts`, then attach SPP source identity and retained
   neutral records through an explicit app/project API. `Session` currently
   tracks a `.subcraft` path and has no SPP document, resource archive or
   save-back field. Avoid treating the `.spp` path as a native `.subcraft`
   project path.
4. **Edit/save mapping:** translate only supported SubCraft commands back to
   matching source UIDs, preserve all untouched records and archive resources,
   and detect conflicting source revisions. Track newly created/deleted or
   unsupported layers and block save until their serialization is supported.
   Container save currently rejects length-changing document writes, so edits
   that grow labels or add records require separately verified HDF5 dataset
   replacement with attribute/type/creation-property preservation.
5. **Acceptance:** add original owned fixtures covering each mapped structure,
   exact document and mesh roundtrips, unchanged source on failure, SubCraft
   edit/save/reopen, and Painter reopen plus reverse reimport. Keep private
   Painter archives out of the repository. Do not report SPP open/save as
   supported until these checks exercise the actual application path.

Likely API boundary: a project-level `SppImportData` can hold the validated
`Mesh`, `Document`, `ResourceStore`, retained `SppDocumentRecords`, source
archive identity and UID-to-`LayerId` map. The app layer can consume that value
to build a `Session` with `from_parts` and preserve the SPP source context. Keep
container parsing in `project`, SubCraft semantic conversion near the project
adapter, and session lifecycle/edit command wiring in `app`; do not introduce
an egui or wgpu dependency into project/app crates.

## Observed scene 1.7 node and mesh-reference layout

The checksum-verified owned structural OBJ/MTL fixtures establish this layout
independently of the earlier single-object geometry variations. This describes
only the observed uncompressed scene 1.7 subset; it is not a projection from
Assimp 1.0. All integer fields below are little-endian.

Each node is a length-delimited block. Its leading `u64` is the complete node
block size, including that eight-byte length. The payload is a `u32` UTF-8 name
byte count and name, sixteen raw `f32` transform components, then a `u32`
child-count, a `u32` mesh-reference count, a second `u32` mesh-reference vector
count, the `u32` mesh indices, a second `u32` child-vector count, and that many
recursively length-delimited node blocks. The duplicate vector counts match
their preceding declared counts in all inspected nodes. The bytes left inside
the node block after its child blocks are opaque metadata and must be retained
exactly. Matrix values are serialized data only; applying them to geometry has
not been established by this observation.

The original one-object quad has a root with one child and no mesh references;
the child references mesh index 0. The two-object fixture has root children
`ObjectA` and `ObjectB`; they reference indices 0 and 1 respectively. Both
corresponding mesh chunks have `material_index = 0`, confirming that distinct
objects can share a material. The two-slot one-object fixture has one child
referencing indices `[0, 1]`; its two mesh chunks have raw material indices
`[1, 0]` in serialized order. Thus node mesh references and mesh
`material_index` are distinct fields. The bounded material suffix codec now maps those indices to `RedSlot` and
`BlueSlot` name properties. Private checks compare the indexed triangles against
the original MTL-assigned geometry. Do not infer material association from source
declaration order or mesh names. Mapping to Painter texture-set records remains
a separate acceptance requirement.

The bounded neutral decoder rejects count mismatches, out-of-range mesh
indices, truncated blocks, and excess depth or aggregate nodes and references.
It retains each node's opaque metadata and preserves transform float bits
without assigning metadata semantics or applying transforms.


## Geometry adapter checkpoint

`build_reference_mesh` now converts the verified identity-transform subset to
SubCraft's canonical mesh, using stored normalized positions and flipped V as in
OBJ import. It retains explicit source material-to-compacted-slot and source
mesh-to-object mappings. Unsupported transforms, instances, missing ownership,
degenerate geometry, absent material names and oversized geometry reject.
Synthetic and private fixture tests cover triangle geometry, UVs, object names,
source slot associations and canonical mesh validation. This geometry result is
not an imported paint document or editor Session. General transforms, original
units and complete resource/layer mapping remain required for full SPP support.
