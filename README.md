<p align="center">
  <img src="resources/logo.svg" alt="Riddle" width="180">
</p>

# Riddle

Riddle 是一门受 Rust 和 Go 启发的实验性编程语言。`v0.1.1` 提供类型检查、move checker、借用与逃逸分析、unsafe 语义、内置标准库、C 后端、项目工具和 LSP。

当前版本是技术预览：语言和工具链仍可能发生不兼容变化。教程与已实现能力见 [The Riddle Book](https://riddle-lang.github.io/docs/)。

## 工具

- `riddlec`：检查 Riddle 源码并生成 C；
- `clue`：创建、检查、构建和运行 Riddle 项目；
- `riddle-lsp`：为编辑器提供诊断和语义高亮。

仓库中的 [`editors`](./editors) 目录提供 Helix、VS Code、Zed 和 IntelliJ IDEA 2026.2+ 的 `riddle-lsp` 适配。

## 安装

预编译版本可从 [GitHub Releases](https://github.com/riddle-lang/riddle/releases) 下载。解压对应平台的 zip，并把二进制所在目录加入 `PATH`。

从源码安装需要较新的 Rust stable。Bash：

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

`clue build` 会保留 `.clue/build/hello.c`，并调用 `CC` 或系统中的 `cc`、`gcc`、`clang` 生成同目录下的本机可执行文件。`clue run` 会先完成相同构建，再运行该程序。

## 许可证

Riddle 使用 [Apache License 2.0](./LICENSE)。
