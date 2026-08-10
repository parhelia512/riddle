<h1 align="center">Clue</h1>

<h3 align="center">
    <a href="README-en.md">English</a> | <a href="README.md">中文</a>
</h3>

`clue` 用于管理和构建 Riddle 项目。

```bash
# 初始化目录，不覆盖已有清单或入口文件
clue init <path> [--bin|--lib|--workspace]

# 创建新项目目录
clue new <path> [--bin|--lib|--workspace]

# 检查整个项目，不生成 C
clue check [path] [--package <name>] [--workspace] [--target <triple>]

# 生成 C 并构建 .clue/build/<package>[.exe]
clue build [path] [--package <name>] [--workspace] [--target <triple>]

# 构建并运行二进制项目
clue run [path] [--package <name>] [--target <triple>] [-- <args>...]
```

二进制项目是默认类型。Clue 支持在 `Clue.toml` 中声明本地路径依赖，暂不解析 registry 或 git 依赖。外部模块和路径依赖的诊断会指向原始源码文件。Riddle LSP 使用相同的项目加载器，并支持未保存文件。

## 工作区

根目录可以使用虚拟工作区清单注册所有子 crate：

```toml
[workspace]
crates = ["app", "libs/math"]
```

每个已注册目录都必须有自己的 `Clue.toml`，依赖仍在子 crate 清单中声明：

```toml
[dependencies]
math = { path = "../libs/math" }
```

根清单只负责注册 crate，不是一个可编译包。根目录的 `clue check` 和 `clue build` 会按依赖顺序处理所有注册 crate；在子目录执行时默认只处理当前 crate，`--workspace` 强制处理整个工作区，`--package <name>` 选择单个 crate。工作区运行多个二进制时使用 `clue run --package <name>`。

工作区只生成一个根目录 `Clue.lock`。其中的本地包使用 `path = "..."` 记录相对根目录的路径，并记录包版本和本地依赖；子 crate 不生成自己的锁文件。工作区内的 path 依赖也必须在 `workspace.crates` 中注册，工作区外的本地依赖可以直接使用。

设置 `CC` 时 Clue 会严格使用指定的 C 编译器；否则会尝试 `cc`、`gcc`、`clang`、带版本后缀的 GCC/Clang，Windows 还会尝试 `clang-cl` 和 `cl`。候选必须能够完成 C11 编译和链接。解析后的路径和版本会参与构建指纹。库项目只保留生成的 `.clue/build/<package>.c`，不会链接可执行文件。

二进制项目默认使用 Riddle 内置 GC，也可以通过一个实现 `rgc_init`、`rgc_alloc`、`rgc_realloc`、`rgc_free` 和 `rgc_collect` 的 C 源文件替换：

```toml
[runtime]
source = "runtime/custom_gc.c"
```

要完全移除 GC、根扫描和 `rgc_*` ABI，可以启用所有权内存模式：

```toml
[runtime]
gc = false
```

该模式使用 `riddle_alloc`、`riddle_realloc` 和 `riddle_free` 管理有所有者的堆值，并在所有者结束时确定性释放。编译器会拒绝需要让栈上值活过其作用域的引用逃逸（E0310）。`gc = false` 不能与 `source` 同时使用。

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
- `workspace.rs`：工作区成员、依赖图和选择；
- `lock.rs`：根 `Clue.lock` 读写；
- `build.rs`：编译和构建缓存；
- `target.rs`：目标组件和 C 工具链配置。
