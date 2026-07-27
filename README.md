<p align="center">
  <img src="resources/logo.svg" alt="Riddle" width="180">
</p>

<h1 align="center">Riddle</h1>

<h3 align="center">
    <a href="README-en.md">English</a> | <a href="README.md">中文</a>
</h3>

Riddle 是一门受 Rust 和 Go 启发的实验性编程语言。`v0.1.1` 提供类型检查、move checker、借用与逃逸分析、unsafe 语义、内置标准库、C 后端、项目工具和 LSP。

当前版本是技术预览：语言和工具链仍可能发生不兼容变化。教程与已实现能力见 [The Riddle Book](https://riddle-lang.github.io/docs/)。

## 语言能力

- 值默认移动，支持 `Copy`、借用检查和字段级部分移动；结构体经 `match` 解构后，未移动的兄弟字段仍可继续使用；
- `std::ops::Drop` 提供确定性析构，覆盖局部变量、参数、模式绑定、迭代元素、聚合字段和闭包环境，并用 drop flag 防止移动后重复析构；
- 非逃逸值优先留在栈上，引用越过当前栈帧时由逃逸分析提升到保守式非移动 GC 堆；存储位置不会改变移动、借用和析构语义；
- 支持泛型与 trait、闭包、递归模式匹配、`IntoIterator` / `Iterator` 驱动的 `for`、`unsafe`、C FFI 和 C11 代码生成。

## 工具

- `riddlec`：检查 Riddle 源码并生成 C；
- `clue`：创建、检查、构建和运行 Riddle 项目；
- `riddle-lsp`：为编辑器提供诊断和语义高亮。

仓库中的 [`editors`](./editors) 目录提供 Helix、VS Code、Zed 和 IntelliJ IDEA 2026.1+ 的 `riddle-lsp` 适配。

## 安装

预编译版本可从 [GitHub Releases](https://github.com/riddle-lang/riddle/releases) 下载。解压对应平台的 zip，并把二进制所在目录加入 `PATH`。

从源码安装需要较新的 Rust stable。

Bash：

```bash
git clone --depth 1 https://github.com/riddle-lang/riddle.git
cd riddle
cargo install --path . --features install-bins --force --target-dir "${TMPDIR:-/tmp}/riddle-install"
```

PowerShell：

```powershell
git clone --depth 1 https://github.com/riddle-lang/riddle.git
Set-Location riddle
cargo install --path . --features install-bins --force --target-dir "$env:TEMP\riddle-install"
```

两种方式都会安装 `clue`、`riddle-lsp` 和 `riddlec`。

## 快速开始

```bash
clue new hello
cd hello
clue check
clue build
clue run
```

`clue build` 会保留 `.clue/build/hello.c`。设置 `CC` 时 Clue 会严格使用它；否则自动寻找系统中的 `cc`、`gcc`、`clang` 及其版本化命令，Windows 还支持 `clang-cl` 和 `cl`。候选必须能完成 C11 编译和链接。`clue run` 会先完成相同构建，再运行该程序。

## 交叉编译

`clue check`、`clue build` 和 `riddlec` 接受 `--target <triple>`。Clue 的目标选择优先级是命令行、`RIDDLE_TARGET`、`Clue.toml` 中的 `[build].target`、宿主平台。使用 ridup 时可以安装目标组件：

```powershell
ridup target add aarch64-unknown-linux-gnu
clue build --target aarch64-unknown-linux-gnu
```

首版严格限制为以下 7 个目标，不接受其他 triple：

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `i686-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`
- `i686-pc-windows-msvc`
- `aarch64-pc-windows-msvc`
- `aarch64-apple-darwin`

目标组件和 C 工具链是两个独立状态。`ridup target add` 安装 Riddle runtime，并询问是否安装匹配的 LLVM/Clang；真正链接仍需要目标平台的系统库：Linux 需要 sysroot，Windows MSVC 目标需要 Windows SDK 和 MSVC 库，macOS 需要 Apple SDK。ridup 不会把缺少这些组件的目标报告为可用。`clue run` 只运行宿主目标；交叉产物需要复制到目标系统运行。

## 许可证

Riddle 使用 [Apache License 2.0](./LICENSE)。
