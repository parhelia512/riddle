<h1 align="center">Clue</h1>

<h3 align="center">
    <a href="README-en.md">English</a> | <a href="README.md">中文</a>
</h3>

`clue` 用于管理和构建 Riddle 项目。

```bash
# 初始化目录，不覆盖已有清单或入口文件
clue init <path> [--bin|--lib]

# 创建新项目目录
clue new <path> [--bin|--lib]

# 检查整个项目，不生成 C
clue check [path] [--target <triple>]

# 生成 C 并构建 .clue/build/<package>[.exe]
clue build [path] [--target <triple>]

# 构建并运行二进制项目
clue run [path] [--target <triple>] [-- <args>...]
```

二进制项目是默认类型。Clue 支持在 `Clue.toml` 中声明本地路径依赖，暂不解析 registry、版本或 git 依赖。外部模块和路径依赖的诊断会指向原始源码文件。Riddle LSP 使用相同的项目加载器，并支持未保存文件。

设置 `CC` 时 Clue 会严格使用指定的 C 编译器；否则会尝试 `cc`、`gcc`、`clang`、带版本后缀的 GCC/Clang，Windows 还会尝试 `clang-cl` 和 `cl`。候选必须能够完成 C11 编译和链接。解析后的路径和版本会参与构建指纹。库项目只保留生成的 `.clue/build/<package>.c`，不会链接可执行文件。

二进制项目默认使用 Riddle 内置 GC，也可以通过一个实现 `rgc_init`、`rgc_alloc`、`rgc_realloc`、`rgc_free` 和 `rgc_collect` 的 C 源文件替换：

```toml
[runtime]
source = "runtime/custom_gc.c"
```

运行时选择属于最终二进制包，库项目不能声明 `[runtime]`。

## 目标平台

目标选择优先级为 `--target`、`RIDDLE_TARGET`、`Clue.toml` 的 `[build].target`、宿主平台：

```toml
[build]
target = "aarch64-unknown-linux-gnu"
```

当前只支持 `x86_64-unknown-linux-gnu`、`aarch64-unknown-linux-gnu`、`i686-unknown-linux-gnu`、`x86_64-pc-windows-msvc`、`i686-pc-windows-msvc`、`aarch64-pc-windows-msvc` 和 `aarch64-apple-darwin`，其他 triple 会被拒绝。

交叉构建二进制项目需要先运行 `ridup target add <triple>`。目标组件提供 Riddle runtime；C 工具链状态另行检查，Linux 需要目标 sysroot，Windows MSVC 目标需要 Windows SDK 与 MSVC 库，macOS 需要 Apple SDK。`clue run` 不会在宿主机上执行交叉产物。

## Rust API

该 crate 公开项目创建、检查、构建和分析 API，供 LSP 等工具使用。使用 `init` 可以初始化已有目录；`new` 和 `init` 都不会覆盖已有清单或目标入口文件。

## 源码布局

- `main.rs`：CLI 参数解析和命令分发；
- `lib.rs`：项目操作和分析 API；
- `project.rs`：项目创建、模板和依赖加载；
- `manifest.rs`：`Clue.toml` 序列化与解析；
- `build.rs`：编译和构建缓存；
- `target.rs`：目标组件和 C 工具链配置。
