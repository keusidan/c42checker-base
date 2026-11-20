# c42checker-base

42 Tokyo (C / 42 Norm) 向け検証ツール群を統合実行する、Rust 製の LSP(Language Server Protocol) サーバーおよび共通基盤です。

`c42checker-vscode` および `c42checker-neovim` のバックエンドとして動作します。

## ビルド・インストール

```bash
cargo build --release
cargo install --path .
```

ビルドすると `c42checker-lsp` バイナリが生成されます。

## テスト

```bash
cargo test
```

## macOS 対応

macOS (Apple Silicon / Intel) に対応しています。

```bash
xcode-select --install
brew install llvm gcc universal-ctags
pipx install norminette
```

* GUI エディタからの起動時にも `~/.local/bin`、`~/.cargo/bin`、`~/Library/Python/*/bin`、`/opt/homebrew/bin`、`/opt/homebrew/opt/llvm/bin`、`/usr/local/bin`、`/usr/local/opt/llvm/bin` と `SDKROOT` (`xcrun --show-sdk-path`) を自動で補完します。
* `/usr/bin/gcc` (Apple clang) や `/usr/bin/ctags` (BSD 版) をスキップし、Homebrew の GNU GCC や Universal Ctags を自動検出します。
* `valgrind` / ASan `detect_leaks=1` が非対応の環境向けに、macOS 標準の `leaks -atExit` を使うステップ `leaks` (`leaks (macOS)`) を備えています。

## 主な LSP コマンド (`workspace/executeCommand`)

* `c42checker.run` — 段階 0 (準備) → 段階 1 (静的検査) → 段階 2 (動的検査) の fail-fast 実行
* `c42checker.runStep` — 単一ステップ実行
* `c42checker.cancel` — 実行中断
* `c42checker.detectTools` — 外部ツール検出・ステップ/トグル定義の取得
* `c42checker.syncPrototypes` — Universal Ctags を用いたプロトタイプ自動同期
* `c42checker.regenerateCompdb` — `compile_commands.json` / `.clangd` の再生成
* `c42checker.cleanWorkDir` — `.c42checker/` の削除
* `c42checker.getContext` — デバッグ・ワークスペースコンテキスト取得
