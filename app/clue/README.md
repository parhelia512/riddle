<h1 align="center">Clue</h1>

<h3 align="center">
    <a href="README-en.md">English</a> | <a href="README.md">中文</a>
</h3>

`clue` 是 Riddle 的包管理器和构建器，管理清单、依赖、目标、工作区、锁文件、缓存、打包与安装。

## 快速开始

```powershell
clue new hello
Set-Location hello
clue check
clue run
```

常用命令：

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

所有子命令都接受 `--offline` 和 `-j/--jobs <N>`；构建类命令还支持 `--release`、`--target`、`--features` 和 `--no-default-features` 中适用的选项。以 `clue <command> --help` 查看精确参数。

## 清单

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

依赖支持 path、git 和 sparse registry。版本使用 semver 约束；表格式依赖还支持 `package` 重命名、`branch`/`tag`/`rev`、`registry`、`features`、`default-features` 和 `optional`。feature 条目可使用 `dep:name` 启用可选依赖、`name/feature` 转发依赖 feature，以及仅在依赖已启用时生效的 `name?/feature`。`[dev-dependencies]` 只在 test、example 和 bench 目标中加载。

未显式声明时，Clue 自动发现 `src/main.rid`、`src/bin/*.rid`、`tests/*.rid`、`examples/*.rid` 和 `benches/*.rid`。多个 bin 可用 `--bin` 选择；`check` 和 `build` 默认处理所有满足 `required-features` 的 bin。`--all-features` 启用清单定义的所有 feature；`--all-targets` 还会检查或构建 test、example 和 bench 目标。

## 依赖与锁文件

`clue fetch` 获取依赖并生成 `Clue.lock` v3。普通 check/build/fetch 优先复用锁定的 registry 版本与 git revision；`clue update` 才重新选择满足约束的最新版本。`--locked` 要求锁文件已经存在且与清单、feature 和本地源码指纹一致，`--offline` 只使用缓存。

锁文件记录包名、版本、source、依赖、启用的 feature、registry checksum、git revision 和源码指纹。registry 包下载后执行 SHA-256 校验，并使用安全路径规则解包。缓存位于 `$CLUE_HOME/registry` 和 `$CLUE_HOME/git`；默认 `CLUE_HOME` 是用户目录下的 `.clue`。

`clue add` 和 `clue remove` 使用 TOML 编辑器修改清单并保留无关布局。`tree -e features` 显示锁定 feature，`metadata` 输出清单发布元数据和锁图 JSON。

## 工作区与调度

虚拟工作区根清单：

```toml
[workspace]
crates = ["app", "libs/math"]
```

工作区只维护根目录的一个 `Clue.lock`。内部 path 依赖必须注册为成员；外部 path 依赖仍可直接使用。`-p/--package` 选择成员，`--workspace` 选择全部成员。Clue 按依赖拓扑分批执行，并在同一批内按 `--jobs` 并行；构建指纹避免重复生成和编译，OS 文件锁保护并发构建与锁文件替换。

## 目标与产物

```powershell
clue build
clue build --example basic
clue test --no-run
clue bench
```

宿主 debug 产物位于 `.clue/build`；release 和显式目标位于 `.clue/build/<target>/<profile>`。二进制构建生成 C 和可执行文件。库构建生成 C、目标文件、`.rmeta` 和默认 `.rlib`；`crate-type` 还可请求 `.a/.lib` 与 `.so/.dylib/.dll`。依赖库先独立编译，应用只生成自身与消费方单态化代码，再链接依赖 `.rlib`；`riddlelib` 不携带 runtime，最终二进制、`staticlib` 和 `cdylib` 才链接 runtime。

设置 `CC` 时 Clue 严格使用该编译器；否则自动探测能够完成 C11 编译和链接的 GCC、Clang 或 MSVC 工具链。目标选择顺序是 `--target`、`RIDDLE_TARGET`、`[build].target`、宿主平台。交叉构建仍需要 ridup 目标组件和对应平台的系统工具链。

## 打包、发布与安装

`clue package` 在 `.clue/package` 生成 `.cluepkg`，排除 `.git` 和 `.clue`；`clue package --list` 只列出将被打包的文件。`clue publish --dry-run` 会执行打包与发布权限校验但不联网；实际 `clue publish` 将归档上传到所选 registry API。`clue install --path .`、`clue install calculator@^1` 和 `clue install --git <url>` 构建二进制并安装到 `$CLUE_HOME/bin`；`clue uninstall <name>` 删除它。

全局或项目配置分别位于 `$CLUE_HOME/config.toml` 和 `.clue/config.toml`，项目配置优先：

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

环境变量 `CLUE_OFFLINE`、`CLUE_JOBS`、`CLUE_REGISTRY_INDEX` 和 `CLUE_REGISTRY_TOKEN` 覆盖配置。

## 运行时

二进制默认使用内置 GC。`[runtime].source` 可指定实现 `rgc_*` ABI 的 C 文件；`[runtime] gc = false` 启用所有权内存模式。运行时配置只属于最终二进制，库不能声明 `[runtime]`。

## 源码布局

- `main.rs`：CLI；
- `lib.rs`：公共 API 与命令编排；
- `manifest.rs` / `model.rs`：清单与领域模型；
- `package.rs` / `lock.rs`：解析、缓存、registry、git 与锁文件；
- `workspace.rs`：工作区依赖图和调度批次；
- `project.rs`：源码与依赖加载；
- `build.rs`：构建缓存、C 工具链和产物；
- `target.rs`：目标组件配置。
