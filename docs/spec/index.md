# 仕様の索引

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
