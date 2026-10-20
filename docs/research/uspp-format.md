# `.uspp` format audit (Universal Painter Files)

Read-only source audit, 2026-10-20. This documents the public `.uspp` interchange
package implemented by Universal SPP; it does **not** describe the native `.spp`
format and does not add `.uspp` support to SubCraft.

## Audited source and license

- Repository: [Yeusepe/Universal-Painter-Files](https://github.com/Yeusepe/Universal-Painter-Files)
- Audited `main`/`HEAD` revision: [`379b2fc64dcf299e8e953d43a3c7df246845d044`](https://github.com/Yeusepe/Universal-Painter-Files/tree/379b2fc64dcf299e8e953d43a3c7df246845d044)
- Root [`LICENSE`](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/LICENSE) is MIT, copyright YUCP Studio (2026). The root [`NOTICE.md`](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/NOTICE.md) contains additional project notices and restrictions about bundled content. This audit records format facts and links; it does not copy implementation code or assets.

## Container and manifest

`.uspp` is a ZIP archive. The repository's user documentation describes its
purpose as a portable handoff package used to reconstruct a temporary `.spp`
for a target Painter version, including optional lossy version downgrade. It is
not a Painter-native project and its normal use depends on the conversion tool
and its version profiles. See the upstream [format overview](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/README.md#the-two-file-formats)
and [CLI package description](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/spp_downgrader/README.md#what-a-uspp-contains).

The writer emits these root members:

| ZIP member | Contents |
|---|---|
| `manifest.json` | Tool/extraction metadata, counts/errors; current CLI adds numeric `format_version: 2`, `tool_version: "1.0.0"`, detected `created_version`, `supports_native_upgrade`, and computed `supported_versions` when version detection succeeds. The lower-level extractor's legacy `version: "1.0"` is also present. |
| `metadata.json` | HDF5 and h5py library versions; parsed Painter/project version fields when available; root HDF5 attributes and limited type/order hints. |
| `structure.json` | Recursive group/dataset tree: paths, child names, group attribute names, dataset shape, dtype string, element count and attribute names. |
| `groups.json` | Per-group attributes, selected group creation properties, immediate dataset names and subgroup names. |
| `datasets.json` | Per-dataset path, data byte count, dtype string, HBO detection/header summary, selected creation properties, attribute values/type/order hints, payload path and optional decoded-JSON path. |
| `data/<flattened-path>.bin` | Dataset values read through h5py and serialized as a byte string; ZIP deflate level 1 is used by the packer. `/` and `\\` in dataset paths are replaced by `_` for payload names. |
| `decoded/<flattened-path>.json` | Optional HBO decode output when the lower-level extractor is explicitly run in decode mode; normal CLI `pack` disables HBO decoding. |
| `raster/manifest.json`, `raster/assets/*` | Optional captured raster fallback manifest and content-addressed image blobs; asset metadata carries SHA-256, byte size, archive path and MIME type. Default capture budget is 512 MiB. |

The ZIP manifest is JSON metadata, not a central authoritative schema document.
The implementation has a numeric format version and tool version, but the
package reader shown in the CLI does not gate on `format_version` before
loading the companion JSON files. The supported-version list is generated from
the available adjacent downgrade profiles, not from a promise that all Painter
versions or features are supported.

## Dataset and HDF5 representation

The packer traverses HDF5 using h5py. For datasets it captures the logical
dataset value via `bytes(dataset[()])`; h5py/HDF5 therefore reads the dataset
through its datatype and filter pipeline before the result is put in the ZIP.
These files are not copies of the HDF5 raw storage blocks. Each dataset record
also stores a dtype string and selected shape/layout metadata.

The dataset creation-property record includes shape, maxshape, chunks,
compression and options, shuffle, Fletcher32, scale-offset, fill value,
allocation/fill times, fill-value-defined state, timestamp tracking, attribute
creation settings, layout and datatype byte order when the API query succeeds.
On build, the writer uses these hints to reconstruct datasets, including some
explicit endian choices and common filters. HDF5 datasets' type, extent rules,
layout and filter pipeline are properties of the created dataset; the package
rebuilds those properties rather than preserving the original HDF5 object
encoding byte-for-byte.

The code identifies two HBO header families for metadata: an older 12-byte
little-endian header (`0x1B7C2FDD`, version check and data version) and a
v11-style 8-byte header (`0x69000B11`, object count). This is only a header
summary unless optional decode is requested. The raw dataset blob remains the
normal rebuild input.

Attributes are converted to JSON-compatible values. For root and dataset
attributes, the extractor also records NumPy dtype strings and HDF5 type order
where available. Group attributes are stored as values without the same dtype
and byte-order side tables. Strings may be decoded as UTF-8 or represented as
hex if decoding fails. This is a useful reconstruction hint set, not an
arbitrary HDF5 attribute type serializer.

## Unknown-object and forward-data preservation

The package is **not a lossless unknown-HDF5-object preservation format**:

- Its tree model emits only groups and datasets. It does not define records for
  named datatypes, link kinds/targets, object identity, hard-link aliases or
  references between objects.
- The extractor's default `visititems` walk records groups and datasets. The
  tree builder likewise represents only those object kinds. Link topology and
  unsupported HDF5 object classes are not preserved as first-class data.
- The normal `pack` command explicitly skips datasets under `texture-cache`;
  the builder skips that path as stale derived data. This is an intentional
  omission, not preservation.
- A failed dataset read is recorded in `errors` and represented with an empty
  byte payload, after which packaging can continue. The consumer must treat
  any extraction errors as possible missing project content.
- Reconstruction is from a new HDF5 file using selected property and attribute
  metadata. Unknown creation properties, custom filters, opaque attribute
  encodings, link topology, committed types, references and other unsupported
  features can be lost or fail reconstruction. Exceptions while recreating
  attributes are often caught and skipped.
- Flattening paths into payload filenames can map distinct HDF5 paths to the
  same ZIP member name; paths therefore need collision validation before any
  independent implementation accepts a package.

Thus `.uspp` can preserve the datasets and metadata the tool models for its
version-conversion purpose, but it is unsuitable as SubCraft's future-preserving
editable source format unless unknown objects are separately retained or
rejected. For standalone SPP editing, prefer the staged original-HDF5 copy
approach in [`spp-container-plan.md`](spp-container-plan.md): mutate understood
objects only, preserve untouched container objects, and reject unsupported
semantics before save. SPP work remains first; no USPP product implementation
is proposed here.

## Bounds and trust boundary

The audited pack path reads each HDF5 dataset into memory and the shown USPP
builder reads ZIP members with `ZipFile.read`. The upstream code has a raster
capture byte budget, but that is not a general archive, entry-count, expanded
size, JSON-depth, path-count or per-dataset quota. Treat both the ZIP directory
and all JSON/payload entries as untrusted. A SubCraft reader must preflight ZIP
metadata, reject duplicate or colliding names and unsafe paths, cap compressed
and expanded byte totals and JSON size/depth/counts, then validate every
dataset's declared size, shape product, dtype, attributes and referenced blob
before allocating or mutating a document. Do not allow archive paths to escape
the package extraction context.

## Source paths consulted

- [`uspp_tool.py`: versions and manifest fields](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/spp_downgrader/uspp_tool.py)
- [`spp_extractor.py`: HDF5 traversal and ZIP writer](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/spp_downgrader/spp_extractor/spp_extractor.py)
- [`spp_builder.py`: ZIP reader and HDF5 reconstruction](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/spp_downgrader/spp_builder/spp_builder.py)
- [`raster_manifest.py`: optional raster members and budget](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/spp_downgrader/spp_extractor/lib/raster_manifest.py)
- [`spp_ext_models.py`: extracted record fields](https://github.com/Yeusepe/Universal-Painter-Files/blob/379b2fc64dcf299e8e953d43a3c7df246845d044/spp_downgrader/spp_extractor/spp_ext_models.py)

No `.spp` or `.uspp` fixtures were copied into SubCraft or committed. The small
shallow source checkout used for this audit is private research material under
`/tmp/subcraft-uspp-audit/` and is not a product dependency.
