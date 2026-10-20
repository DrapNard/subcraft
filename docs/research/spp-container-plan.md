# Next SPP container integration

Status: design checkpoint, 2026-10-20. No HDF5 dependency is currently added to
SubCraft. Genuine SPP open/save remains unimplemented.

## Established prerequisites

- Bounded Rust reflective dataset decoder/encoder, with exact private-corpus
  roundtrips and synthetic corruption tests.
- Raw U8 document dataset checksums verified across six owned fixtures:
  MurmurHash3 x64/128, seed `0xF13A0239`, ordered unsigned 64-bit words.
- A layer-name edit made outside Painter and rehashed in a copied container
  reopens editable in Painter. See [evidence](spp-roundtrip.md).

## Container backend candidate

Prefer the official HDF5 library behind a narrow, isolated Rust adapter. Review
and pin the maintained [metno Rust binding](https://github.com/metno/hdf5-rust)
before adding dependencies. Its [sys crate documents static builds](https://docs.rs/crate/hdf5-metno-sys/0.12.3)
and currently describes HDF5 2.0.0 as the bundled version; verify the actual pinned
source and license notices. This is a candidate, not a dependency decision or
cross-platform build result. The older aldanor wrapper's historical bundled
version is not sufficient evidence for current system HDF5 2.x compatibility.

A source build needs CMake and a C toolchain. Keep Linux, Windows and macOS
packaging explicit and retain the community build path. Browser support must
not accidentally acquire an unavailable native dependency.

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
   then fsync and atomically replace. Any error/cancel keeps the source intact.

## Next acceptance fixtures

Broaden beyond a blank paint layer: original authored strokes, Fill, mask, group,
multiple channels and texture sets, resource references, and incremental saves.
Then map mesh/document data into SubCraft's existing Session without flattening,
retain unsupported artist data, and prove edited roundtrips in both directions.
The one controlled rename is a prerequisite result, not a general format claim.

The optional private Rust test requires six `*.document.bin` extracts and matching
`*.checksum.txt` files containing the two decimal attribute words. No private
Painter data is bundled:

```sh
scripts/dev/low-impact env SUBCRAFT_SPP_RESEARCH_DIR=/absolute/private/corpus \
  cargo test --locked -p subcraft-project --test spp_owned_corpus -- --ignored
```
