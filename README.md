<!--
草案(Issue #253)。Claudeが下書きしたもの。確認が要る箇所は「TODO」のコメントで示す。
-->

# SCITL Task Companion

チャットでタスクの内容・締切を聞き取り、進捗の報告もチャットで行うタスク管理アプリです。
SCITLは **S**tandalone **C**hat **I**nterface for **T**asks with **L**Ms の略です。

<!-- TODO: スクリーンショット -->

## 特徴

- **チャットが操作の中心**: 専用の入力フォームではなく、会話を通してタスクを登録・更新します。
  全体を見渡す「総合チャット」と、個別のタスクに紐づく「タスクチャット」があります
- **ローカル完結**: サーバーを立てず単体で動きます。通信するのは、利用者が登録したLLM APIと
  外部ツールサーバー(MCP)だけです
- **LLMを選ばない**: OpenAI互換・Anthropic・Geminiの各形式のAPIに対応します。llama.cpp・
  LM Studio・Ollama等のローカル推論サーバーもOpenAI互換として使えます
- **外部ツール(MCP)**: stdio・Streamable HTTPのMCPサーバーを登録し、ツールをモデルに使わせられます
- **データを閉じ込めない**: データはSQLiteに保存し、Markdownへ書き出せます
- **秘密情報はOSの資格情報ストアへ**: APIキー等は設定ファイルに書かず、OSの資格情報ストアに保存します
- 表示言語は日本語・英語

## 対応OS

| OS | 状況 |
|---|---|
| Linux | 対応。Secret Service(GNOME Keyring・KWallet等)が動いている必要があります |
| Windows | 対応予定。現在はビルドできません(#176) |
| macOS | 非対応。秘密情報の保存先を用意していません |

<!-- TODO: Linuxで動作を確かめたディストリビューション -->

## ビルド

### 前提

- Rust(stable)
- Node.js 22
- Tauri 2のシステム依存。Ubuntu 24.04では以下です(他の環境は
  [Tauriの案内](https://v2.tauri.app/start/prerequisites/)を参照)

  ```sh
  sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev \
    librsvg2-dev libxdo-dev libssl-dev
  ```

<!-- TODO: Rustの最低バージョンを決めるか(今はピン留めしていない) -->

### 開発用に起動する

```sh
npm --prefix frontend ci
cd crates/scitl-tauri
npm ci
npm run tauri dev
```

フロントエンドの開発サーバー(Vite、`localhost:1420`)はTauri CLIが起動します。

### 配布用にビルドする

```sh
cd crates/scitl-tauri
npm run tauri build
```

成果物は`target/release/bundle/`に出ます。

### テスト・lint

CIと同じものを手元で走らせられます(`.github/workflows/ci.yml`)。

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

cd frontend
npm run typecheck
npm run lint
npm run build
```

**`frontend/src/bindings/`は`cargo test`が生成します。** Rust側の型から作るTypeScriptの型定義なので、
手で直さず、Rust側を変えたら`cargo test`で生成し直してコミットしてください。

## 使い方

<!-- TODO: 初回起動からタスクを1つ作るまでの流れ(プロバイダーの登録 → モデルの選択 → 総合チャットで話しかける) -->

### データの置き場所

GUIとCLIは同じデータディレクトリを使います。

| OS | 場所 |
|---|---|
| Linux | `~/.local/share/net.niigo.scitl/` |
| Windows | `%APPDATA%\net.niigo.scitl\` |

中には、データベース(`scitl.sqlite3`)・設定(`config.toml`)・添付ファイル(`attachments/`)・
Markdownの書き出し(`export/`)が入ります。APIキーは`config.toml`には入りません。

### CLI

GUIを開かずにタスクを確認・操作できるCLIがあります。出力はJSONです。
既定ではGUIと同じデータディレクトリを開くので、先にGUIを一度起動してください。

```sh
cargo run -p scitl-cli -- task list          # タスクの一覧
cargo run -p scitl-cli -- task show 1        # タスクの詳細
cargo run -p scitl-cli -- chat show --task 1 # タスクチャットの発言(--task を省くと総合チャット)
cargo run -p scitl-cli -- --data-dir DIR task list
```

ほかに`task rename`・`archive`・`unarchive`・`delete`があります(`--help`で一覧)。
外部のLLMツールにシェル経由で使わせることも想定しているため、応答の生成・設定の変更は持ちません。

開発者向けには、応答の生成・設定・プロバイダーやMCPサーバーの登録・Markdownの書き出し等を足した
`scitl-debug-cli`があります。詳しくは`docs/spec/architecture/cli.md`を見てください。

## ドキュメント

設計の文書は`docs/spec/`にあります。まず索引の`docs/spec/index.md`を読み、触る領域の文書だけを
読む使い方を想定しています。

- `docs/spec/principles.md` — 実装が変わっても守る設計原則
- `docs/spec/architecture/` — 領域ごとの方針(LLMとの通信、秘密情報、WebViewとの境界など)
- `docs/spec/data-model/` — データベースの形
- `docs/spec/tools.md` — モデルに公開するツール
- `docs/spec/ui.md` — 画面

## 開発の進め方

- ブランチはGit Flow(`main` / `develop` / `feature/*` / `release/*` / `hotfix/*`)です。
  `develop`から`feature/*`を切ってPRを出してください
- 作業項目はGitHub Issuesで管理しています
- `CLAUDE.md`・`.claude/skills/`はClaude Code向けの運用の決まりです

<!-- TODO: 外部からのコントリビュートを受け付けるか -->

## ライセンス

[MIT License](LICENSE)
