<h1 align="center">Clue</h1>

<h3 align="center">
    <a href="README-en.md">English</a> | <a href="README.md">中文</a>
</h3>

`clue` is Riddle's package manager and builder. It manages manifests, dependencies, targets, workspaces, lockfiles, caches, packaging, and installation.

## Quick Start

```powershell
clue new hello
Set-Location hello
clue check
clue run
```

Common commands:

```text
clue init|new <path> [--bin|--lib|--workspace]
clue check|build [path] [-p <package>|--workspace] [--bin <name>] [--features a,b|--all-features] [--all-targets] [--locked]
clue run [path] [-p <package>] [--bin <name>|--example <name>] [--features a,b|--all-features] [-- <args>...]
clue test|bench [path] [-p <package>|--workspace] [--test|--bench <name>] [--features a,b|--all-features]
clue add <name> [--version <req>|--path <path>|--git <url>] [--dev]
clue remove <name> [--dev]
clue fetch|update|tree|metadata [path]
clue tree -e features
clue package [--list] [path]
clue publish [--dry-run] [--registry <name>] [path]
clue install [<package>@<version-req>] [--path <path>|--git <url>]
clue uninstall <name>
clue clean [path]
```

Every command accepts `--offline` and `-j/--jobs <N>`. Build commands also expose the applicable `--release`, `--target`, `--features`, and `--no-default-features` options. Use `clue <command> --help` for the exact interface.

## Manifest

```toml
[package]
name = "demo"
version = "0.1.0"
license = "MIT"
publish = ["company"]

[features]
default = ["logging"]
logging = ["dep:log", "log/std"]
conditional-log = ["log?/std"]

[dependencies]
math = { path = "../math", version = "^1.0" }
json = "^1.2"
codec = { git = "https://example.com/codec.git", tag = "v1.0.0" }
log = { version = "^1", optional = true, default-features = false }

[dev-dependencies]
assertions = { path = "../assertions" }

[lib]
path = "src/lib.rid"
crate-type = ["riddlelib", "staticlib", "cdylib"]

[[bin]]
name = "demo"
path = "src/main.rid"
required-features = ["logging"]

[[example]]
name = "basic"
path = "examples/basic.rid"
```

Dependencies may come from paths, git, or a sparse registry and use semver requirements. Table dependencies also support package renaming, `branch`/`tag`/`rev`, registry selection, features, default-feature control, and optional dependencies. Feature entries accept `dep:name` to enable an optional dependency, `name/feature` to forward a dependency feature, and `name?/feature` to forward it only when that dependency is active. `[dev-dependencies]` are loaded only for test, example, and bench targets.

Without explicit target entries, Clue discovers `src/main.rid`, `src/bin/*.rid`, `tests/*.rid`, `examples/*.rid`, and `benches/*.rid`. Select one binary with `--bin`; check and build otherwise process every binary whose required features are enabled. `--all-features` enables every manifest feature, and `--all-targets` also checks or builds test, example, and bench targets.

## Dependencies and Lockfile

`clue fetch` retrieves dependencies and writes `Clue.lock` v3. Normal check/build/fetch operations prefer locked registry versions and git revisions; `clue update` deliberately selects newer matching versions. `--locked` requires an existing lockfile that matches the manifest, features, and local source fingerprints. `--offline` uses only cached data.

The lockfile records package versions, sources, dependencies, enabled features, registry checksums, git revisions, and source fingerprints. Registry archives are SHA-256 verified and unpacked with safe path checks. Caches live under `$CLUE_HOME/registry` and `$CLUE_HOME/git`; `CLUE_HOME` defaults to `.clue` in the user's home directory.

`clue add` and `clue remove` edit TOML without rewriting unrelated layout. `tree -e features` shows locked features, while `metadata` emits publishing metadata and the resolved graph as JSON.

## Workspaces and Scheduling

Use a virtual root manifest:

```toml
[workspace]
crates = ["app", "libs/math"]
```

A workspace owns one root `Clue.lock`. Internal path dependencies must be registered members; external path dependencies remain valid. `-p/--package` selects one member and `--workspace` selects all. Clue schedules topological batches and runs independent packages within a batch according to `--jobs`. Build fingerprints skip unchanged generation and compilation, while OS file locks protect concurrent builds and lockfile replacement.

## Targets and Artifacts

```powershell
clue build
clue build --example basic
clue test --no-run
clue bench
```

Host debug artifacts live in `.clue/build`; release and explicit-target artifacts use `.clue/build/<target>/<profile>`. Binary builds emit C and an executable. Library builds emit C, an object, `.rmeta`, and a default `.rlib`; `crate-type` may additionally request `.a/.lib` and `.so/.dylib/.dll` outputs. Dependencies are compiled as separate libraries; an application emits only its own code and consumer-side monomorphizations, then links dependency `.rlib` archives. `riddlelib` excludes the runtime, which is linked only by final binaries, `staticlib`, and `cdylib`.

When `CC` is set, Clue uses it strictly. Otherwise it probes GCC, Clang, or MSVC candidates that can compile and link C11. Target precedence is `--target`, `RIDDLE_TARGET`, `[build].target`, then the host. Cross builds still require a ridup target component and the target platform's system toolchain.

## Package, Publish, and Install

`clue package` creates `.clue/package/*.cluepkg` while excluding `.git` and `.clue`; `clue package --list` only lists its files. `clue publish --dry-run` packages and validates publish permission without a network request; `clue publish` uploads to the configured registry API. `clue install --path .`, `clue install calculator@^1`, and `clue install --git <url>` build a binary into `$CLUE_HOME/bin`; `clue uninstall <name>` removes it.

Global and project configuration live at `$CLUE_HOME/config.toml` and `.clue/config.toml`, with project values taking precedence:

```toml
[net]
offline = false

[build]
jobs = 4

[registry]
default = "default"

[registries.default]
index = "https://registry.example/index"
api = "https://registry.example"
token = "..."
```

`CLUE_OFFLINE`, `CLUE_JOBS`, `CLUE_REGISTRY_INDEX`, and `CLUE_REGISTRY_TOKEN` override configuration files.

## Runtime

Binary packages use the bundled GC by default. `[runtime].source` may select a C file implementing the `rgc_*` ABI; `[runtime] gc = false` enables owned-memory mode. Runtime configuration belongs only to final binaries and is rejected in libraries.

## Source Layout

- `main.rs`: CLI
- `lib.rs`: public API and command orchestration
- `manifest.rs` / `model.rs`: manifest and domain model
- `package.rs` / `lock.rs`: resolution, cache, registry, git, and lockfile
- `workspace.rs`: workspace graph and scheduling batches
- `project.rs`: source and dependency loading
- `build.rs`: build cache, C toolchain, and artifacts
- `target.rs`: target component configuration
