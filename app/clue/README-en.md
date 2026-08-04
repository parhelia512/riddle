<h1 align="center">Clue</h1>

<h3 align="center">
    <a href="README-en.md">English</a> | <a href="README.md">中文</a>
</h3>

Clue manages and builds Riddle projects.

```bash
# Initialize a directory without overwriting an existing manifest or entry file
clue init <path> [--bin|--lib]

# Create a project in a new directory
clue new <path> [--bin|--lib]

# Check the whole project without generating C
clue check [path] [--target <triple>]

# Generate C and build .clue/build/<package>[.exe]
clue build [path] [--target <triple>]

# Build and run a binary package
clue run [path] [--target <triple>] [-- <args>...]
```

Binary projects are the default. Clue supports local path dependencies declared in
`Clue.toml`; it does not resolve registry, version, or git dependencies.
Diagnostics from external modules and path dependencies point to their original
source files. The Riddle LSP uses the same project loader, includes unsaved files,
and refreshes diagnostics for every open document after a change.

Binary builds strictly use `CC` when set. Otherwise Clue tries `cc`, `gcc`,
`clang`, version-suffixed GCC/Clang executables, and, on Windows, `clang-cl` and
`cl`. A candidate must compile and link a C11 probe. Its resolved path and version
participate in the build fingerprint. Library builds keep the generated
`.clue/build/<package>.c` without linking an executable.

Binary packages use the bundled GC by default. A package can replace it with one
C source file implementing `rgc_init`, `rgc_alloc`, `rgc_realloc`, `rgc_free`,
and `rgc_collect`:

```toml
[runtime]
source = "runtime/custom_gc.c"
```

To remove GC, root scanning, and the `rgc_*` ABI entirely, enable owned-memory
mode:

```toml
[runtime]
gc = false
```

This mode uses `riddle_alloc`, `riddle_realloc`, and `riddle_free` for owned heap
values and releases them deterministically when their owner ends. The compiler
rejects references that would require stack storage to outlive its scope
(E0310). `gc = false` cannot be combined with `source`.

Runtime selection belongs to the final binary package; library packages cannot
declare `[runtime]`.

## Targets

Target selection precedence is `--target`, `RIDDLE_TARGET`, `[build].target` in
`Clue.toml`, then the host platform:

```toml
[build]
target = "aarch64-unknown-linux-gnu"
```

The current release supports only `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, `i686-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`, `i686-pc-windows-msvc`,
`aarch64-pc-windows-msvc`, and `aarch64-apple-darwin`. Every other triple is
rejected.

Before cross-building a binary package, run `ridup target add <triple>`. The
target component supplies the Riddle runtime; C-toolchain readiness is checked
separately. Linux needs a target sysroot, Windows MSVC targets need the Windows
SDK and MSVC libraries, and macOS needs an Apple SDK. `clue run` does not run a
cross-built artifact on the host.

## Library API

The crate exposes project operations plus project analysis for tools such as the LSP:

```rust
use clue::{ProjectKind, build, check, new, run};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new("hello");
    new(root, ProjectKind::Binary)?;
    check(root)?;
    build(root)?;
    run(root, &[])?;
    Ok(())
}
```

Use `init` instead of `new` to initialize an existing directory. Both functions
refuse to overwrite an existing manifest or target entry file.

## Source Layout

- `main.rs`: CLI argument parsing and command dispatch
- `lib.rs`: public project operations and analysis API
- `project.rs`: project creation, templates, and dependency loading
- `manifest.rs`: `Clue.toml` serialization and parsing
- `build.rs`: compilation and build cache
- `target.rs`: target components and C-toolchain configuration
