# Riddle

这是一个大量受到 Rust 和 Go 启发的语言，吸取了其中的大部分内容并重新设计而形成的语言。

关于语言的教程请查看 [The Riddle Book](https://riddle-lang.github.io/docs/)

## Install

```powershell
cargo install --path . --features install-bins --force --target-dir "$env:TEMP\riddle-install"
```

会安装 `clue`、`riddle-lsp` 和 `riddlec`。

实现的约束应该严格小于被实现的类型:Paterson condition

## Todo

- [ ] 实现宏 `#[...]`