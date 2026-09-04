# Changelog

## [Unreleased]

### Added

- New standard function-like macro `vec!`: `vec![a, b, c]` builds a `Vector` and pushes each element (values move in; trailing commas and nested `vec!` calls work), `vec![elem; count]` expands to the new `std::vector::Vector::from_elem(elem, count)` (requires `T: Clone`, cloning one element per slot), and empty `vec![]` expands to a `Vector::new()` block whose element type is inferred from the binding annotation or later usage. Registered in `STANDARD_FUNCTION_MACROS`, so LSP completion and navigation cover it.
- Range expressions `a..b` / `a..=b` parse at statement-expression precedence (looser than arithmetic, matching Rust) and desugar to calls of `std::ops::range` / the new `std::ops::range_inclusive`; `std::ops::RangeInclusive` implements `Iterator`/`IntoIterator` with correct empty and single-element inclusive semantics. `for i in 0..5 { .. }` works end to end.
- Format placeholders gained positional and named forms: `{0}` / `{1}` reference arguments explicitly (and may repeat), `{name}` implicitly captures a call-site local, both optionally with `:?` (`{value:?}`); implicit `{}` keeps its own sequential counter, and the arity / index errors are reported on the format string.
- `str`/`String` gained `split` (returns `Vector<String>`), `replace`, `to_ascii_uppercase`, and `to_ascii_lowercase`.
- `Vector<T>` gained `insert`, `remove` (returns and shifts, non-`Copy` safe), `contains` (`T: PartialEq`), `sort` (`T: PartialOrd`, insertion sort), and `retain`.
- `Iterator` gained the `collect` default method (returns `Vector<Self::Item>`), and `std::iter` gained the `skip` adapter and `min` / `max` (return `Option<Item>`, require `Item: PartialOrd`).
- `HashMap` gained `get_or_insert(key, default) -> &mut V`.
- `std::parse` gained `parse_i64`, `parse_u64`, `parse_usize`, and `parse_with_radix` (2–36) with overflow-checked accumulation.
- `std::time` gained `Duration` (`from_secs` / `from_millis` / `as_millis` / `as_secs`) and `sleep`, backed by the `riddle_sleep_ms` runtime shim (`Sleep` on Windows, `nanosleep` elsewhere).
- New `std::random` module: `random_u32`, `random_u64`, `random_bool`, and `random_below`, backed by `riddle_random_u32` / `riddle_random_u64` shims (xorshift seeded from `GetTickCount` on Windows, `/dev/urandom` on POSIX).
- `std::fs` gained `exists`, `metadata` (`FileMetadata { size, is_file, is_dir }`), and `read_dir` (returns `Vector<String>`, Win32 `FindFirstFile` / POSIX `dirent` with an overflow-count retry protocol).
- Raw pointers of the same pointee and mutability now support `==` / `!=` by address in the type checker, MIR comparison lowering, and the C backend; `p == 0usize as *const T` is the null check (this also unblocks handling allocation failures in std).
- Test suites gained `tests/escape_analysis` (stack retention vs. GC promotion vs. `E0310` under no-GC) and `tests/frontend` (range parsing and precedence, bracket-lambda disambiguation, match-arm comma recovery, malformed-input recovery).
- The language server's quick-fix catalog grew from two to nine actions, each derived from its published diagnostic: `E0031` reassignment now offers to add `mut` to the offending `let` binding (located through the syntax tree, closest same-function declaration wins; tuple/struct patterns are skipped), `E0007` offers `Add field \`x\`` inserting `x: todo!()` before the struct/variant literal's closing brace (comma and indentation follow the literal's existing shape), `E0039` offers `Add \`P\` arm` / `Add wildcard arm` appending the missing pattern (taken from the diagnostic message) with a diverging `todo!()` body at the arm indentation, `E0051` removes the empty `use` declaration together with its line, `E0056` rewrites the explicit destructor call `x.drop()` into `drop(x)`, and `E0050` unresolved names offer both `Did you mean \`name\`?` (Damerau-Levenshtein over the names visible at the reference, with a distance budget that grows with identifier length) and up to five `Import \`x\` from \`a::b\`` actions reusing the completion auto-import route machinery (hidden items filtered out).
- The language server now serves `source.organizeImports` code actions: top-level `use` declarations are sorted (plain imports before `pub use` re-exports, then by path text), duplicates removed, and empty declarations dropped. Blocks containing comments or attributes keep their order and are only deduplicated in place, uses interleaved with items are left untouched, and no action is offered when nothing would change.
- The language server's inlay hints now cover lambda parameters and multiline method chains. Unannotated lambda parameters (bracket-lambda or `fun` lambdas) show their call-site-inferred type — `[v -> v * 2]` passed where `impl Fn(i32) -> i32` is expected displays `v: i32` — and each link of a method chain broken across lines shows the substituted result type after its closing paren (`xs.iter()` newline `.map(f)` displays `: Map<..>`, IntelliJ-style; single-line chains stay clean). Hovering an unannotated lambda parameter shows the inferred type instead of `_`, and hovering a method call (including its signature help) renders the instantiated signature with substituted receiver and return types (`fun show(self: Value) -> i32`) instead of the declared trait signature. The type checker records these call-site signatures in a new `TypeCheckResult::method_signatures` map (keyed by the callee expression, resolved for inference variables, and cached/replayed by the incremental checker).
- `Clue.toml` manifests get first-class language-server support. Schema-aware diagnostics flag unknown keys (`CLUE0003`, warning), invalid values — bad semver versions, wrong types, `path`+`git` combinations, multiple git references, unknown `crate-type`s, missing `package.name` — (`CLUE0004`) and TOML syntax errors (`CLUE0002`) with precise spans, computed from the open buffer alongside the existing project-level `CLUE0001` errors. Completions offer section headers (`[package]`, `[dependencies]`, …), the keys of each known section (already-present keys excluded), and value literals (`true`/`false`, the three `crate-type`s); hover documents every key including dependency sub-keys; document symbols list sections and their keys. The VS Code extension now activates for `Clue.toml` files (`workspaceContains:**/Clue.toml`) and routes them to the same server.
- The language server now serves `textDocument/selectionRange`: nested selection ranges are derived from the syntax tree by walking ancestors from the token under each requested position, collapsing levels that share their extent so every step grows the selection (the editor's "expand selection" command). `textDocument/documentLink` links every file-based `mod foo;` of the top level to the module file the compiler would load (`foo.rid` or `foo/mod.rid` next to the document; missing or ambiguous targets are skipped), and `textDocument/diagnostic` serves pull diagnostics computed by the same project-aware, cancellation-cooperative pipeline as the pushed diagnostics, for clients that prefer pulling.
- Completion got context awareness. Typing directly inside an `impl Trait for T` body now offers the trait's not-yet-implemented required methods as snippet completions carrying the declared signature (`area` inserts `fun area(self) -> f64 {\n    $0\n}`) plus its missing associated types; already-provided members are filtered out. Declaration keywords (`let`, `struct`, `impl`, …) are only offered at statement and item starts — expression positions keep the control and value keywords — and statement starts additionally offer `match` / `ifelse` / `forin` templates as LSP snippets.
- Two analysis-backed quick fixes joined the language server's catalog: `E0026` ("impl for `X` of trait `T` missing method `m`") offers `Implement \`m\` from \`T\``, inserting a `todo!()` stub rendered from the trait's declared signature (receiver form, parameter and return types, generics) at the impl block's closing brace, replacing the block's trailing whitespace so both single-line and multi-line impls end up with one method per line; and `E0013` ("unknown method `x` on type Y") offers `Did you mean \`x\`?` by Damerau-Levenshtein over the methods of the receiver type's impls plus every visible trait method, renaming the called identifier in place. Both reuse the E0050 analysis-backed code action pipeline (gated on the presence of fixable diagnostics, cancellation-aware, stale-analysis responses dropped).
- `riddlec` accepts multiple input files with `--backend c`: every file is macro-expanded, merged into one combined package (files can reference each other's top-level items directly), compiled once, and emitted as a single C file. The combined source map feeds code generation, so panic locations in multi-file programs point at the original module files and lines. Single-file behavior is unchanged.
- Slice references cross `unsafe extern "C"` boundaries as a pointer/length pair: a `&[T]` parameter (sized, non-`str` element) is declared and called as two C parameters — `const T*` plus `size_t` for `&[T]`, `T*` plus `size_t` for `&mut [T]` — following the usual C convention instead of exposing the `riddle_slice` fat-pointer struct. Unsized elements and slice return values stay rejected with precise diagnostics.
- Constant items evaluate to compile-time values through a new const evaluator (integer and boolean literals, arithmetic, comparison and bitwise operators, unary negation and complement, casts, and references to other constants, with the existing cycle detection guarding recursion). Evaluated constants now work as array lengths in types, array-repeat lengths, and const-generic arguments — `const N: usize = 4; let arr: [i32; N]` and `len([0; N])` both compile, where the repeat and type positions previously demanded literals.
- The MIR C backend gained an end-to-end panic-location regression test (`panic_locations_resolve_to_the_original_module_file`) and the cast matrix a cross-layer unit test (`cast_matrix_tests`) pinning every documented `as` pair the type checker admits to a MIR lowering, so checker-side extensions can no longer silently reach the unsupported-cast internal error.
- The mark-sweep GC runtime tracks objects in a dynamic registry instead of a single intrusive linked list: exact-pointer lookups (`rgc_free` / `rgc_realloc`) go through an address-hash table, interior-pointer marking binary-searches an address-sorted index rebuilt per collection, and sweep visits only registered slots — removing the O(heap) scans. The collection threshold now grows with the live set (`next = max(1 MiB, live * 2)`) instead of re-collecting at a fixed 1 MiB, and `RGC_DEBUG_STATS=1` prints collection statistics to stderr. Public `rgc_*` ABI is unchanged.

### Fixed

- `E0046` no longer does double duty: the "cannot construct an infinite type" error now has its own code, `E0067`, documented with its own example. This removes the message-sniffing workaround in the diagnostic helper and the risk of LSP tooling misrouting unsafe-context quick fixes onto inference errors.
- Panic runtime locations in multi-file builds point at the original module file. Lowering previously computed `panic!` line/column against the combined multi-package source and stamped the entry file's name on every panic; the MIR `Panic` instruction now records the call-site offset (with the combined-source position as fallback), and the C backend resolves it through the loaded package's source map — generated macro output anchors at its original call site — so the emitted `riddle_panic` reports the real file, line, and column.
- `std::env::args()` / `args_os()` now work regardless of which package calls them. The generated C entry point unconditionally initializes the process-argument runtime; previously it only did so when the entry module itself referenced `argc`/`argv`, so a call reaching args through a dependency package left the POSIX accessors reading zeroed globals (the Windows runtime happened to hide this by lazily parsing `GetCommandLineW`). `clue` already always links `args_runtime.c`; only raw `riddlec` users must follow the updated compile hint in the generated header.
- Integer and float literal diagnostics distinguish unsupported wide-type suffixes from unknown ones: `42i128` / `1.0f16` now report "integer/float literal suffix `…` is not supported" with the supported scalar set (and a matching help note), instead of the misleading "unknown suffix".

### Changed

- `clue` no longer leaves a `Clue.lock.guard` file in the project root: the write lock now lives at `.clue/lock-guard` (clue's private, gitignored state directory). The lock still serializes concurrent writers of `Clue.lock` — it must stay a stable sidecar path because the lock file itself is replaced atomically by rename — it is just no longer visible next to the manifest.
- The `E0005` inference failure ("cannot infer type argument(s)") now names the unresolved type parameter when exactly one is left (`cannot infer type argument \`T\` for function \`new\``), replaces the arity-flavored note with one about determining the type from a call argument or an explicit annotation, and — when the failed call sits under an unannotated `let` binding — attaches a secondary label on the binding (`consider giving \`t\` an explicit type`). Bindings introduced by standard macro expansions (`__riddle_*` temporaries, e.g. the hidden `Vector::new` inside `vec![]`) are skipped so the hint lands on the user-facing binding; this mirrors rustc's `E0282` "consider giving \`v\` an explicit type" help.
- A failed macro expansion in expression position no longer cascades into misleading `E0040` missing-expression, `E0045`, or `E0059` diagnostics; the unexpanded call lowers to a placeholder and only the proc-macro error is reported.
- `base.name(args)` where `name` is neither a method nor a callable field now reports exactly one `E0013` unknown-method error instead of pairing it with a misleading `E0006` unknown-field diagnostic.
- A block-bodied `match` arm may omit its trailing comma (its `}` ends the arm); a missing comma used to derail the parser into a cascade of expression errors.
- Generic bound-dispatch calls compile correctly: trait-method calls on a generic `T` (`value.lt(&current)` with `T: PartialOrd`) and trait-dispatched operators (`==` / `<` on a generic `T`) inside generic functions used to miscompile when an operand was a match payload binding — its storage was lazily materialized inside one match arm and read from sibling arms that never executed it, reading an indeterminate pointer. Address-taken match payload bindings now materialize their storage at the binding site (which dominates every later use), mirroring `let` storage: GC heap when the analysis proves escape, stack otherwise. `std::iter::{min, max}` are now available.
- Method calls directly on iterator adapter types (`taken.count()`, `skipped.count()`) resolve: the receiver type is resolved through pending inference variables before method lookup, so chained constructors like `take(range.into_iter(), 2)` no longer leave the adapter's iterator parameter unresolved when `Iterator`-impl lookup runs.

- Bracket lambdas `[params -> body]` provide a short anonymous-function form: `[it -> it * 2]` (single parameter, conventionally named `it`), `[acc, v -> acc + v]`, `[v: &i32 -> *v > 3]`, `[(left, _) -> left]`, zero-parameter `[ -> body]`, block bodies `[v -> { ... }]`, and `move [v -> base + v]`. Parameter and return types infer from the expected callable signature. A bracket group containing a top-level `->` is a lambda; otherwise it stays an array literal or index, and `expr [params -> body]` in postfix position calls `expr` with the lambda (typically a method: `values.map [v -> v * 2]`). Bracket lambdas lower to the same closure representation as `fun(...)`; generics and `where` clauses remain `fun`-only.
- Collections gained `remove`: `HashMap` uses backward-shift deletion for its linear-probe table, `TreeMap` performs CLRS red-black deletion with delete-fixup plus arena compaction, and `HashSet`/`TreeSet` forward to their backing maps.
- `Iterator` provides the default methods `count`, `nth`, `fold`, `for_each`, `all`, `any`, `find`, and `position`; `std::iter` additionally offers the eager `map_into` / `filter_into`, `DoubleEndedIterator` with `SliceIter::next_back`, and the lazy adapters `enumerate` / `take` / `zip` (constructible via free functions and iterable in `for` loops); `Vector::from_iterator` collects any iterator.
- `std::convert::From` is available; `?` falls back to `From` impls when no `Into` impl matches and now also propagates through `Option` operands inside functions returning `Option`.
- Calls through callable struct fields (`self.f(x)` where `f` holds a closure or a generic parameter constrained by `Fn`/`FnMut`/`FnOnce`) are supported; impl callable bounds are satisfied structurally by closure signatures with kind widening.
- Lazy `Iterator::map` / `Iterator::filter` store their callable in an adapter and chain through `for` loops.
- `std::fs` introduces `FsFile` (`open` / `create` / `append` / `read` / `write` / `flush` / `read_to_string`), `FsError`, and whole-file `read_to_string` / `write` helpers, backed by new `riddle_fs_*` runtime shims; `Drop` closes the handle.
- `String` implements `From<&str>`.
- End-to-end behavior tests cover collection removal, iterator combinators, `?` conversions, and file round-trips (`tests/mir/std_behavior.rs`).

### Changed

- The parser accepts trailing commas in function parameter lists.
- Method calls on values that satisfy a where-clause trait bound now dispatch statically to the implementing impl instead of lowering to an invalid indirect call.
- Where-clause associated-type bindings (`I: Iterator<Item = T>`) bind type parameters that appear in no argument, including across generic function calls into MIR monomorphization.
- Trait default method bodies substitute `Self` and supertrait associated types (e.g. `Iterator::Item`) from the implementing impl when monomorphized through a generic-call path.
- Closure and named-function arguments now drive inference of generic parameters that appear only in callable bounds or in where-clause associated-type bindings (`I: Iterator<Item = T>`).
- `for` loops resolve the iterable type (and its `IntoIterator` associated types) through pending inference variables before trait lookup.
- Trait default method bodies substitute associated types (`Self::Item`) from the implementing impl during monomorphization, both in the type checker and the MIR.
- `E0061`'s message now reads "`?` requires a Result or Option value as its operand".

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
