# 仕様の索引

SCITLの確定方針を書いた文書の案内。実装を始める前にこの文書を読み、触る領域の文書だけを読む。
文書とコードが食い違う場合は、どちらが正しいかを確かめてから直す。

使っている技術: Tauri 2(Rust + WebView)、React + TypeScript + Vite、SQLite(`rusqlite`、WAL)、
`reqwest` + `rustls`、`keyring-core`、設定はTOML・言語ファイルはJSON(理由は`architecture/tech-stack.md`)。

## 文書ごとの読む場面

「Opus」の列は、その文書の領域を変える差分がCLAUDE.md「Opusでレビューする条件」に当たるか。

| 文書 | 読む場面 | Opus |
|---|---|---|
| `principles.md` | 設計判断をするとき。該当する節だけを読む | 節による |
| `architecture/tech-stack.md` | 依存の追加・技術の置き換え、ライセンスの判断 | 実行時依存の追加は当たる |
| `architecture/cli.md` | `scitl-cli`・`scitl-debug-cli`のコマンドを触る | 秘密情報の受け取り方を変えるなら当たる |
| `architecture/llm-adapter.md` | プロバイダーのアダプタ、モデルの能力、イベント列、失敗の種類 | 通信先・通信方式を変えるなら当たる |
| `architecture/prompt-shape.md` | システムプロンプト、ユーザー発言の囲み、日時・状態の伝え方、操作の記録、固定文言 | 囲み・予約タグの形を変えるなら当たる |
| `architecture/transcript.md` | 履歴の組み立て、送った形の保存、思考の送り返し、間引き | 保存の形を変えるなら当たる |
| `architecture/concurrency.md` | 多重起動の防止、同期と非同期の境界、応答生成の停止、途中経過の通知 | 2つ目の起動から届く引数を使うなら当たる |
| `architecture/network-secrets.md` | HTTPクライアント、平文http、秘密情報、資格情報ストア | 当たる |
| `architecture/webview-boundary.md` | IPCコマンド、CSP・Tauriの権限、外部リンク | 「CSP / Tauri権限設定」の見出しの内容を変えるなら当たる |
| `architecture/sanitize.md` | 外部から来た文字列・自由入力を、モデル・画面・端末・ファイルへ出す | 当たる |
| `architecture/i18n.md` | 画面の文言、言語ファイル、表示言語 | 当たらない |
| `architecture/attachments.md` | 添付の受け取り・正規化・置き場所・表示・モデルへの渡し方 | 当たらない |
| `architecture/export.md` | Markdownエクスポート | 当たらない |
| `data-model/tables.md` | 型と形式、`tasks`・`task_steps`・`attachments`、索引、PRAGMAと排他、マイグレーション | 当たる |
| `data-model/messages.md` | `messages`、ターン境界、操作の記録、`turn_transcripts`。`architecture/transcript.md`と対で読む | 当たる |
| `tools.md` | LLMに公開するツールのスキーマ、引数検証、履歴への載せ方、外部(MCP)ツールの公開 | 公開する操作・権限を変えるなら当たる |
| `ui.md` | 画面を触るとき(必ず読む) | 当たらない |

## ファイルを跨ぐ不変条件

1つの文書だけを読んで変えると壊れる条件。変えるときは、挙げた文書をすべて読む。

- **前に送った部分を書き換えない**: 毎回変わるもの(日時・状態)をシステムプロンプトや直近の発言に
  添えない。`architecture/transcript.md`・`architecture/prompt-shape.md`・`architecture/llm-adapter.md`
  (Anthropic形式の思考ブロック)
- **ユーザー発言の囲みの中には、利用者が書いたものだけを置く**: 添付・操作の記録・通知は囲みの外に置く。
  `architecture/prompt-shape.md`・`architecture/attachments.md`
- **保存するデータは書き換えず、無害化は出力先へ出す直前に出力先ごとに掛ける**: 送った形の保存は
  無害化済みのまま持つので、無害化の規則を変えたら保存の形の版を上げる。`architecture/sanitize.md`・
  `architecture/transcript.md`
- **WebViewからファイルのパスを受け取らない**: `architecture/attachments.md`「受け取り方」・
  `architecture/export.md`・`architecture/cli.md`(端末は例外)
- **秘密情報に触れるのは`secrets.rs`だけ、HTTPは`net::hardened_client`だけを通る**:
  `architecture/network-secrets.md`・`architecture/cli.md`・`architecture/llm-adapter.md`

## ワークスペース構成

Tauri(Rust製のコア + WebView上のフロントエンド)で構築する。バックエンドの責務は
すべてRust側に置く(`principles.md` 4節「UI層に秘密情報と外部通信を持たせない」)。

```
SCITL-2.0/
├── Cargo.toml                      # workspace root
├── crates/
│   ├── scitl-core/                 # UI非依存のコアライブラリ
│   │   └── src/
│   │       ├── db/                 # tasks/steps/messages/attachmentsのrepository
│   │       ├── attachments/        # 添付の種別の判定・実体の置き場所・送信前の添付(architecture/attachments.md)
│   │       ├── export/             # Markdownエクスポート(architecture/export.md)
│   │       ├── llm/                # types(イベント列), adapter trait, providers/(方言ごとのアダプタ)
│   │       │   ├── error.rs            # 失敗の種類
│   │       │   ├── capabilities.rs     # モデル能力の解決(architecture/llm-adapter.md)
│   │       │   ├── prompt.rs           # 予約タグ・ユーザー発言の囲みとその説明・無害化(architecture/prompt-shape.md・sanitize.md)
│   │       │   └── token_estimate.rs   # トークン数の見積もり
│   │       ├── tools/              # registry(面別スキーマ生成), args検証, 各ツール
│   │       ├── orchestration/      # 応答生成と、会話をモデル・画面へ渡す形の組み立て
│   │       │   ├── turn.rs             # 1ターンの処理フロー(送信・編集・再試行の入口)
│   │       │   ├── turn_context.rs     # 1ターンが受け取る文脈
│   │       │   ├── turn_request.rs     # 各ラウンドで送るものの組み立て
│   │       │   ├── turn_event.rs       # 途中経過の通知
│   │       │   ├── turn_error.rs       # LLM呼び出しの失敗をエラー発言にする
│   │       │   ├── tool_limits.rs      # 1ターンのツール呼び出しの上限
│   │       │   ├── tool_record.rs      # ツール実行記録の形
│   │       │   ├── mcp_access.rs       # ターンから見た外部ツールサーバー
│   │       │   ├── history.rs          # モデルへ送る履歴の組み立て
│   │       │   ├── history_trim.rs     # 履歴の間引き(architecture/transcript.md「間引きの位置」)
│   │       │   ├── transcript.rs       # 送った形の保存の形(architecture/transcript.md「送った形のまま積む」)
│   │       │   ├── system_prompt.rs    # システムプロンプト
│   │       │   ├── prompt_defaults.rs  # 設定で書き換えられるプロンプトの既定の文面
│   │       │   ├── operations.rs       # 応答生成以外の経路での操作と記録
│   │       │   ├── preview.rs          # 送信内容のプレビュー
│   │       │   └── chat_view.rs        # 画面に渡す会話の行
│   │       ├── mcp/                # 外部ツールサーバーのクライアント(stdio / streamable_http)
│   │       ├── net.rs              # 全HTTP経路が通るクライアント設定(architecture/network-secrets.md)
│   │       ├── secrets.rs          # OS資格情報ストアへの唯一の入口
│   │       ├── config.rs           # 参照のみを持つ設定(TOML)
│   │       ├── paths.rs            # データディレクトリの場所と中の並び(GUI・CLI共通)
│   │       ├── files.rs            # 書きかけのファイルを完成した名前で残さない書き込み
│   │       ├── settings/           # 設定・登録の操作(規則・検証・秘密情報の出し入れ・保存)
│   │       ├── error.rs            # コア全体の失敗の種類(CoreError)
│   │       ├── blocking.rs         # 非同期層からブロッキング処理を呼ぶ入口(architecture/concurrency.md)
│   │       ├── in_flight.rs        # 同じ対象への処理を同時に1本に絞る(タスクごとの応答生成)
│   │       ├── link.rs             # 本文中のリンクを開く前の判定とOSへの委譲(architecture/webview-boundary.md)
│   │       ├── text.rs             # 出力先を知らない文字単位の部品(architecture/sanitize.md)
│   │       └── i18n.rs             # 言語ファイルを引く入口と表示言語の一覧(architecture/i18n.md)
│   ├── scitl-cli/                  # タスクの確認・操作と会話の表示。scitl-coreのみに依存
│   ├── scitl-debug-cli/            # 旧debug_cli.py相当。scitl-cliのコマンドに、応答生成・設定・登録を足す
│   └── scitl-tauri/                # 薄いIPCシェル
│       ├── tauri.conf.json         # CSP・devCsp(変更のたびにOpusレビュー対象)
│       └── src/commands/
├── frontend/                       # React + TypeScript + Vite
├── lang/                           # ja.json / en.json(core・frontend共有。architecture/i18n.md)
└── migrations/                     # 0001_init.sql から番号順(data-model/tables.md 5節)
```

- **コア** (`scitl-core`): `principles.md` 5節が「1箇所に閉じる」ことを求める判断
  (状態遷移、ツール引数検証、ターンのオーケストレーション、サニタイズ、秘密情報アクセス)を
  すべて置く
- **GUI** (`scitl-tauri`): `commands/*.rs` はロジックを持たず、
  「デシリアライズ→coreを1つ呼ぶ→シリアライズ」のみ。この薄さ自体が
  「coreはUIなしで同じ検証経路を通って呼び出せる」ことの構造的な担保になる
- **CLI** (`scitl-cli`・`scitl-debug-cli`): `architecture/cli.md`
