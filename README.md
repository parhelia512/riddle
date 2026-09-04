<div align="center">
  <img src="resources/logo.svg" alt="Riddle" width="180">

  [GitHub][github] | [文档][docs] | [更新日志][changelog] | [English](README-en.md)
</div>

这是 [Riddle][github] 的主源码仓库，包含格式化工具（`riddle fmt`）、编译器（`riddlec`）、项目工具（`clue`）和语言服务器（`riddle-lsp`）。

Riddle 是一门受 Rust 和 Go 启发的实验性编程语言。`v0.2.3` 提供类型检查、move checker、借用与逃逸分析、unsafe 语义、内置标准库、C 后端、项目工具和 LSP。当前版本仍处于技术预览阶段：语言和工具链仍可能发生不兼容变化。

## 为什么选择 Riddle？

- **可靠性**：值默认移动，`Copy`、借用检查与字段级部分移动在编译期保证内存语义；`std::ops::Drop` 提供确定性析构，覆盖局部变量、参数、模式绑定、迭代项、聚合体字段和闭包环境，并由 drop flag 防止移动后重复析构。

- **性能**：未逃逸的值优先留在栈上；只有当引用超出当前栈帧时，逃逸分析才会把值提升到保守式非移动 GC 堆。存储位置不会改变移动、借用与析构语义，C11 后端则把程序编译成可直接链接的普通 C。

- **生产力**：泛型与 trait、闭包、递归模式匹配、由 `IntoIterator` / `Iterator` 驱动的 `for` 循环、`unsafe` 与 C FFI 一应俱全；配合 `clue` 项目工具和 LSP，从创建项目到运行一路顺畅。

## 工具

- `riddlec`：检查 Riddle 源码并生成 C；
- `riddle fmt`：按统一风格格式化 Riddle 源码，也可用 `--check` 检查格式；
- `clue`：管理 Riddle 包、依赖、工作区、构建目标与安装产物；
- `riddle-lsp`：为编辑器提供诊断、工作区索引、自动导入补全、高级导航、重命名、格式化和语义高亮。

仓库中的 [`editors`](./editors) 目录为 Helix、VS Code、Zed 和 IntelliJ IDEA 2026.1+ 提供 `riddle-lsp` 适配。

## 快速开始

```bash
clue new hello
cd hello
clue check
clue build
clue run
```

`clue fetch` 会解析 path、git 和 sparse registry 依赖并生成 `Clue.lock` v3；普通构建复用锁定版本，`clue update` 才重新求解，`--locked` 和 `--offline` 分别提供锁文件与缓存约束。`clue build` 会保留 `.clue/build/hello.c`，并支持多 bin、lib、example、test 和 bench 目标；完整清单与命令参见 [`app/clue`](./app/clue)。设置 `CC` 时，Clue 只会使用该编译器；否则会探测能完成 C11 编译与链接的 GCC、Clang 或 MSVC 工具链。

## 安装

可从 [GitHub Releases][releases] 下载预编译版本：解压对应平台的 zip，并将二进制所在的目录加入 `PATH`。

从源码构建使用仓库 `rust-toolchain.toml` 固定的 Rust 1.97.1。

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

上述两种方式都会安装 `clue`、`riddle-lsp`、`riddlec` 和 `riddle` 四个二进制。

如果只想构建这四个可安装二进制，请指定根发行包（root distribution package），避免 workspace 中同名开发包重复输出：

```bash
cargo build -p riddle --release --features install-bins --bins
```

## 开发与验证

修改源码后，使用以下命令验证完整 workspace：

```bash
cargo test --workspace --all-targets
cargo check -p riddle --features install-bins --bins
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`install-bins` 只用于根发行包。不要在 workspace 测试命令中添加 `--all-features`，否则根发行包与成员 crate 的 `clue`、`riddlec` 会产生同名输出警告；上面的独立 `cargo check` 会覆盖这四个安装入口。

Clippy 会检查默认启用的全部规则并把告警视为错误，不使用新增的 `#[allow(clippy::...)]` 绕过源码问题。

## 交叉编译

`clue check`、`clue build` 和 `riddlec` 均接受 `--target <triple>`。目标选择优先级依次为命令行参数、`RIDDLE_TARGET` 环境变量、`Clue.toml` 中的 `[build].target`，最后回退到宿主平台。目标组件可通过 ridup 安装：

```powershell
ridup target add aarch64-unknown-linux-gnu
clue build --target aarch64-unknown-linux-gnu
```

首个发布版仅支持以下 7 个目标，不接受其他 triple：

- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `i686-unknown-linux-gnu`
- `x86_64-pc-windows-msvc`
- `i686-pc-windows-msvc`
- `aarch64-pc-windows-msvc`
- `aarch64-apple-darwin`

目标组件与可用的 C 工具链是两个相互独立的状态。`ridup target add` 会安装 Riddle runtime，并询问是否安装配套的 LLVM/Clang；实际链接仍需要目标平台的系统库：Linux 需要 sysroot，Windows MSVC 目标需要 Windows SDK 与 MSVC 库，macOS 需要 Apple SDK。缺少这些组件时，ridup 不会把目标标记为可用。`clue run` 只能运行宿主目标；交叉编译产物需复制到目标系统上运行。

## 获取帮助

教程与已实现的能力参见 [The Riddle Book][docs]；报告 bug、提问或贡献代码请到 [GitHub Issues][issues]。Riddle Book 的源码位于本仓库的 `docs/` 目录。

## 许可证

Riddle 使用 [Apache License 2.0](./LICENSE)。

[github]: https://github.com/riddle-lang/riddle
[docs]: https://riddle-lang.github.io/docs/
[releases]: https://github.com/riddle-lang/riddle/releases
[issues]: https://github.com/riddle-lang/riddle/issues
[changelog]: CHANGELOG.md
