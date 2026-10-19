# Development guide

Read README.md, CLAUDE.md, docs/progress.md and docs/integration/kalium-merge.md.

- SubCraft is the main app and repository. Keep one Session/document/render/UI
  architecture; Every useful Kalium capability must have a verified SubCraft equivalent or an
  integrated implementation. A project importer and safety checks alone do not
  complete this merge. Track all gaps in docs/integration/kalium-capabilities.md.
- Owner resource constraint: **at most four CPU cores, low priority**. Run builds,
  tests, benchmarks and other CPU-heavy work with `scripts/dev/low-impact`.
  Serialize heavy jobs; do not start an unrestricted Cargo command. On Linux
  the helper pins the command and descendants to four available CPUs, nice 19,
  and sets four Cargo jobs, Rayon workers and test threads.
- The combined app is AGPL-3.0-only. Preserve original MIT/Apache and resource
  notices; no Adobe source, shaders, icons or bundled resources.
- No image generation or generative AI product features.
- Keep editable SPP open/save as the first format priority. Detection, renamed
  archives, flattening and a Painter-dependent bridge are not SPP support.
- Treat imported data as untrusted. Validate dimensions, allocations, recursion,
  identifiers and references before mutation. Preserve unsupported artist data
  or reject explicitly; never silently drop it on save.
- Include regression tests for fixes and actual runtime evidence for behavior
  claims. Record skipped GPU tests separately from rendered acceptance.
- Do not commit private SPP fixtures or Painter resources.
