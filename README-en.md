<div align="center">
  <img src="resources/logo.svg" alt="Riddle" width="180">

  [GitHub][github] | [Documentation][docs] | [Changelog][changelog] | [中文](README.md)
</div>

This is the main source code repository for [Riddle][github]. It contains the
compiler (`riddlec`), project tooling (`clue`), and a language server
(`riddle-lsp`).

Riddle is an experimental programming language inspired by Rust and Go. As of
`v0.2.0`, it provides type checking, a move checker, borrow and escape
analysis, unsafe semantics, a bundled standard library, a C backend, project
tooling, and an LSP. This is a technology preview: the language and toolchain
may still change without compatibility guarantees.

## Why Riddle?

- **Reliability:** Values move by default. `Copy`, borrow checking, and
  field-level partial moves are enforced at compile time; `std::ops::Drop`
  provides deterministic destruction for local variables, parameters, pattern
  bindings, iteration items, aggregate fields, and closure environments, with
  drop flags preventing double destruction after moves.

- **Performance:** Non-escaping values stay on the stack. Only values whose
  references outlive the current stack frame are promoted by escape analysis to
  a conservative, non-moving GC heap. Storage location never changes move,
  borrow, or drop semantics, and the C11 backend emits plain, linkable C.

- **Productivity:** Generics and traits, closures, recursive pattern matching,
  `IntoIterator` / `Iterator`-driven `for` loops, `unsafe`, and C FFI are all supported — paired with
  `clue` project tooling and an editor LSP, from project creation to running.

## Tools

- `riddlec`: checks Riddle source and generates C;
- `clue`: manages Riddle packages, dependencies, workspaces, build targets, and installed artifacts;
- `riddle-lsp`: provides editor diagnostics and semantic highlighting.

The [`editors`](./editors) directory contains `riddle-lsp` integrations for
Helix, VS Code, Zed, and IntelliJ IDEA 2026.1+.

## Quick Start

```bash
clue new hello
cd hello
clue check
clue build
clue run
```

`clue fetch` resolves path, git, and sparse-registry dependencies into
`Clue.lock` v3. Normal builds reuse locked versions, while `clue update`
re-resolves them; `--locked` and `--offline` constrain the lockfile and cache.
`clue build` keeps `.clue/build/hello.c` and supports multiple bin, lib,
example, test, and bench targets; see [`app/clue`](./app/clue) for the complete
manifest and command reference. When `CC` is set, Clue uses it strictly;
otherwise it probes a GCC, Clang, or MSVC toolchain that can compile and link C11.

## Installation

Prebuilt releases are available from [GitHub Releases][releases]: extract the
archive for your platform and add its binary directory to `PATH`.

Building from source uses Rust 1.97.1 as pinned by `rust-toolchain.toml`.

Bash:

```bash
git clone --depth 1 https://github.com/riddle-lang/riddle.git
cd riddle
cargo install --path . --features install-bins --force --target-dir "${TMPDIR:-/tmp}/riddle-install"
```

PowerShell:

```powershell
git clone --depth 1 https://github.com/riddle-lang/riddle.git
Set-Location riddle
cargo install --path . --features install-bins --force --target-dir "$env:TEMP\riddle-install"
```

Both methods install `clue`, `riddle-lsp`, and `riddlec`.

To build only the installable binaries, target the root distribution package so
the workspace's development packages do not emit duplicate binaries:

```bash
cargo build -p riddle --release --features install-bins --bins
```

## Development and Verification

After changing the source, validate the complete workspace with:

```bash
cargo test --workspace --all-targets
cargo check -p riddle --features install-bins --bins
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`install-bins` is only for the root distribution package. Do not add
`--all-features` to the workspace test command: that enables root and member
`clue`/`riddlec` binaries with identical output names. The separate
`cargo check` above covers all three installation entry points.

Clippy checks every default-enabled lint and treats warnings as errors. Source
issues must not be bypassed with new `#[allow(clippy::...)]` attributes.

## Cross-compilation

`clue check`, `clue build`, and `riddlec` accept `--target <triple>`. Clue
selects a target from the command line, `RIDDLE_TARGET`, `[build].target` in
`Clue.toml`, then the host platform, in that order. Ridup installs target
components:

```powershell
ridup target add aarch64-unknown-linux-gnu
clue build --target aarch64-unknown-linux-gnu
```

The first release is strictly limited to these seven targets; no other triple
is accepted:

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `i686-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`
- `i686-pc-windows-msvc`
- `aarch64-pc-windows-msvc`
- `aarch64-apple-darwin`

A target component and a working C toolchain are two separate states. `ridup
target add` installs the Riddle runtime and offers to install matching
LLVM/Clang, but linking still needs the target platform libraries: a Linux
sysroot, the Windows SDK and MSVC libraries for MSVC targets, or an Apple SDK
for macOS. Ridup does not mark a target as ready until these requirements are
met. `clue run` only runs the host target; run a cross-built artifact on its
target system.

## Getting Help

Tutorials and implemented capabilities are documented in
[The Riddle Book][docs]; report bugs, ask questions, or contribute via
[GitHub Issues][issues]. The Riddle Book source lives in the `docs/` directory
of this repository.

## License

Riddle is distributed under the [Apache License 2.0](./LICENSE).

[github]: https://github.com/riddle-lang/riddle
[docs]: https://riddle-lang.github.io/docs/
[releases]: https://github.com/riddle-lang/riddle/releases
[issues]: https://github.com/riddle-lang/riddle/issues
[changelog]: CHANGELOG.md
