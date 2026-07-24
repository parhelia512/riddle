<p align="center">
  <img src="resources/logo.svg" alt="Riddle" width="180">
</p>

<h1 align="center">Riddle</h1>

<h3 align="center">
    <a href="README-en.md">English</a> | <a href="README.md">中文</a>
</h3>

Riddle is an experimental programming language inspired by Rust and Go. Version
`v0.1.1` provides type checking, a move checker, borrow and escape analysis,
unsafe semantics, a bundled standard library, a C backend, project tooling, and
an LSP.

This is a technology preview. The language and toolchain may still change
without compatibility guarantees. Tutorials and implemented capabilities are
available in [The Riddle Book](https://riddle-lang.github.io/docs/).

## Language Features

- Values move by default, with `Copy`, borrow checking, and field-level partial
  moves; after destructuring a struct in `match`, unmoved sibling fields remain
  usable.
- `std::ops::Drop` provides deterministic destruction for local variables,
  parameters, pattern bindings, iteration items, aggregate fields, and closure
  environments, with drop flags preventing double destruction after moves.
- Non-escaping values stay on the stack when possible. Escape analysis promotes
  values to a conservative, non-moving GC heap when references outlive the
  current stack frame; storage location does not change move, borrow, or drop
  semantics.
- Generics and traits, closures, recursive pattern matching,
  `IntoIterator` / `Iterator`-driven `for` loops, `unsafe`, C FFI, and C11 code
  generation are supported.

## Tools

- `riddlec`: checks Riddle source and generates C;
- `clue`: creates, checks, builds, and runs Riddle projects;
- `riddle-lsp`: provides editor diagnostics and semantic highlighting.

The [`editors`](./editors) directory contains `riddle-lsp` integrations for
Helix, VS Code, Zed, and IntelliJ IDEA 2026.1+.

## Installation

Prebuilt releases are available from [GitHub Releases](https://github.com/riddle-lang/riddle/releases).
Extract the archive for your platform and add its binary directory to `PATH`.

Building from source requires a recent Rust stable toolchain:

```bash
git clone --depth 1 https://github.com/riddle-lang/riddle.git
cd riddle
cargo install --path . --features install-bins --force --target-dir "${TMPDIR:-/tmp}/riddle-install"
```

The PowerShell equivalent is:

```powershell
git clone --depth 1 https://github.com/riddle-lang/riddle.git
Set-Location riddle
cargo install --path . --features install-bins --force --target-dir "$env:TEMP\riddle-install"
```

Both methods install `clue`, `riddle-lsp`, and `riddlec`.

## Quick Start

```bash
clue new hello
cd hello
clue check
clue build
clue run
```

`clue build` keeps `.clue/build/hello.c`. When `CC` is set, Clue uses it
strictly; otherwise it searches for `cc`, `gcc`, `clang`, and versioned command
names, plus `clang-cl` and `cl` on Windows. A candidate must compile and link
C11 successfully. `clue run` performs the same build before running the program.

## License

Riddle is distributed under the [Apache License 2.0](./LICENSE).
