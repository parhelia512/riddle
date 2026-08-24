# Changelog

## [0.2.2] - 2026-08-25

### Added

- `riddle fmt` formats Riddle files or standard input, checks formatting without writing, and shares its implementation with LSP formatting requests.
- Dynamic trait objects, associated types, comments and documentation comments, control-flow patterns, and loop expressions are supported across the compiler and editor tooling (`b418f16`, `c3c58be`, `4df93b0`).
- LSP workspace indexing and richer completion edits now cover project modules, auto-imports, macros, and editor formatting (`c3c58be`, `4df93b0`).
- The root distribution package now installs `clue`, `riddle-lsp`, `riddlec`, and `riddle`.

### Changed

- Stable and nightly archives include the `riddle` command alongside the existing compiler, project, and language-server binaries.
- Release and cross-target CI now use the repository's Rust 1.97.1 toolchain and validate the complete distribution entry points (`cfc7b49`, `d720322`, `b677277`, `baf8645`).
- Formal release validation and published platform builds resolve dependencies from the committed `Cargo.lock`.
- VS Code, IntelliJ IDEA, and Zed package metadata is aligned with the `0.2.2` release.

### Fixed

- Generic expected types now flow through unit enum variants such as `Option::None`, including explicit enum type arguments; explicit arguments on payload variants are no longer silently ignored.
- `riddle fmt` now rejects parser and lexer errors with a non-zero status before writing, while its output preserves comments, attributes, intentional blank lines, and the source line-ending style for supported syntax.
- `riddle fmt` keeps glob imports, array const generics, nested unit values, and dereference expressions lexically tight instead of inserting whitespace that changes the intended layout.
- `riddle fmt` keeps generic arguments tight in `impl`, trait, `dyn`, and `where` headers instead of treating them as comparison operators.
- Pattern checking, move-state convergence, dynamic dispatch lowering, and associated-type substitutions no longer accept the previously reported invalid states (`b418f16`, `4df93b0`).

### Maintenance

- Standard-library, Clue dependency, release documentation, and CI configuration were synchronized for the release (`d22e74a`, `15a3729`, `471dc39`).

## [0.2.1] - 2026-08-11

### Added

- LSP now supports hover, definition and implementation navigation, references, prepare-rename, rename, workspace indexing, editor features, and automatic imports (`b624bc6`, `3ff5802`).
- Procedural macros now support package discovery, derive/function/attribute exports, source mapping, diagnostics, token-stream expansion, caching, and long-lived workers (`06d9516`, `1c00493`, `d127d1e`).
- Tuple types and tuple lowering are implemented, and the compiler was reorganized into dedicated syntax, type, HIR, MIR, and workspace layers (`7a2830b`, `d9751c7`).
- Rust-style formatting and token-quoting macros are supported, with compile-time format validation and mapped FFI diagnostics (`67c1a17`).
- Rust-style `panic!`, `assert!`, `assert_eq!`, `assert_ne!`, `debug_assert*`, `todo!`, `unimplemented!`, and `unreachable!` macros now share expansion, formatting, source-location, and abort diagnostics.

### Changed

- Print grammar and parser integration were updated so formatted output is handled consistently by the compiler and LSP (`f0270ed`, `67c1a17`).
- `clue` gained workspace/package selection, lock-file generation, and no-GC runtime configuration (`7a2830b`, `d127d1e`).
- Cross-crate integration tests were moved under the root test target, and the compiler was split into smaller modules (`7a2830b`, `d9751c7`).

### Fixed

- Moving through an explicit dereference of a non-`Copy` reference is now rejected with `E0308`, while `Copy` values remain readable (`c4aed74`).
- Escaping reference temporaries are promoted to stable heap storage instead of becoming dangling stack references; generic function values now participate in substitution (`38609a2`, issues [#2](https://github.com/riddle-lang/riddle/issues/2) and [#3](https://github.com/riddle-lang/riddle/issues/3)).
- Pattern matching, generic unrolling, loop move-state convergence, closure captures, recursive reference-flow analysis, and reference casts no longer accept unsound or incorrectly repeated states (`fe6a730`, `2662775`).
- Impl, method, const-generic, and `Self` substitutions are kept separate, preventing shadowed generic names from producing incorrect monomorphized symbols (`c212e69`).
- Function parsing and callable-trait handling now preserve `move` closures, mutable receivers/parameters, and `impl Fn` bounds; related LSP visibility and semantic-token issues were corrected (`ab18431`).
- LSP protocol integration now limits completion triggers, filters code-action kinds, registers watched files when supported, respects private field visibility, and invalidates semantic-token caches by project revision (`9a08e86`).
- C string comparisons are evaluated before the temporary string is dropped, avoiding reads from released storage (`4e46c6a`).
- No-GC builds now use a bundled allocator runtime, and proc-macro output stays compatible with the selected runtime (`c25038f`).
- Proc-macro host/build failures were fixed across the host protocol, Windows output handling, generated names, FFI string conversion, source-map ranges, and diagnostic propagation (`1c00493`, `0e9e6be`, `67c1a17`).
- Duplicate definitions are diagnosed with `E0064` instead of silently entering the scope graph (`b0af790`, issue [#8](https://github.com/riddle-lang/riddle/issues/8)).
- LSP rename and smoke tests now use canonical temporary paths and valid file URIs (`1f812cf`, `32d24fc`).

### Maintenance

- GitHub Actions versions, submodule checkout, Windows/macOS investigation jobs, and pull-request CI were updated; the temporary debug workflow was removed after the proc-macro investigation (`7abbae6`, `7d49c14`, `26a020c`, `64fcb20`, `9900729`, `5ae3b69`).

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
