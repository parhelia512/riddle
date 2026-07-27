# Changelog

## [0.2.0] - 2026-07-28

### Added

- Target-aware builds in `clue` and `riddlec`, with runtime components for the seven supported target triples.
- Project-aware LSP diagnostics, completion, semantic tokens, inlay hints, and code actions.
- Editor packages for Helix, VS Code, Zed, and IntelliJ IDEA 2026.1 or newer.
- Expanded language and standard-library support for generics, traits, closures, operator overloading, deterministic `Drop`, arrays, slices, strings, and vectors.

### Changed

- The C backend and bundled conservative GC runtime now support the expanded language surface without an external GC dependency.
- Release artifacts now include host toolchains, target runtime packages, an installation manifest, and editor extensions.

### Fixed

- Reinitializing a binding after moving it no longer leaves the binding marked as moved.
- Source-install validation builds only the root distribution package, avoiding duplicate workspace binary outputs.
