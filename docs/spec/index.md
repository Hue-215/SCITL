# 仕様の索引

文書とコードが食い違う場合は、どちらが正しいかを確かめてから直す。

使っている技術: Tauri 2(Rust + WebView)、React + TypeScript + Vite、SQLite(`rusqlite`、WAL)、
`reqwest` + `rustls`、`keyring-core`、設定はTOML・言語ファイルはJSON(理由は`architecture/tech-stack.md`、
HTTPクライアントの設定は`architecture/network-secrets.md`)。

## 文書ごとの読む場面

「Opus」の列は、その文書の領域を変える差分がCLAUDE.md「Opusでレビューする条件」に当たるか。

| 文書 | 読む場面 | Opus |
|---|---|---|
| `principles.md` | 設計判断をするとき(該当する節だけ)。コード・コメント・コミットを書くとき(5節・8節) | 節による |
| `architecture/tech-stack.md` | 依存の追加・技術の置き換え、ライセンスの判断、依存の脆弱性 | 実行時依存の追加は当たる |
| `architecture/cli.md` | `scitl-cli`・`scitl-debug-cli`のコマンドを触る | 秘密情報の受け取り方を変えるなら当たる |
| `architecture/llm-adapter.md` | プロバイダーのアダプタ、モデルの能力、イベント列、失敗の種類 | 通信先・通信方式を変えるなら当たる |
| `architecture/prompt-shape.md` | システムプロンプト、ユーザー発言の囲み、日時・状態の伝え方、操作の記録、固定文言 | 囲み・予約タグの形を変えるなら当たる |
| `architecture/transcript.md` | 履歴の組み立て、送った形の保存、思考の送り返し、間引き | 保存の形を変えるなら当たる |
| `architecture/concurrency.md` | 多重起動の防止、同期と非同期の境界、応答生成の停止、途中経過の通知 | 2つ目の起動から届く引数を使うなら当たる |
| `architecture/network-secrets.md` | HTTPクライアント、平文http、秘密情報、資格情報ストア | 当たる |
| `architecture/webview-boundary.md` | 画面が持つもの・持たないもの、IPCコマンド、CSP・Tauriの権限、外部リンク | 「CSP / Tauri権限設定」の見出しの内容を変えるなら当たる。IPCコマンドの引数でパス・URL等を受け取るものを足す・広げるなら当たる。「画面が持つもの・持たないもの」の持たないものを緩めるなら当たる |
| `architecture/sanitize.md` | 外部から来た文字列・自由入力を、モデル・画面・端末・ファイルへ出す | 当たる |
| `architecture/i18n.md` | 画面の文言、言語ファイル、表示言語 | 当たらない |
| `architecture/attachments.md` | 添付の受け取り・正規化・置き場所・表示・モデルへの渡し方 | 受け取り方(外から受け取る入力)・囲みの外に置く規則・信頼できない入力としての扱いを変えるなら当たる |
| `architecture/export.md` | Markdownエクスポート | 書き出し先の決め方(外から受け取る入力)を変えるなら当たる |
| `data-model/tables.md` | 型と形式、`tasks`・`task_steps`・`attachments`・`memories`、索引、PRAGMAと排他、マイグレーション | 当たる |
| `data-model/messages.md` | `messages`、ターン境界、操作の記録、`turn_transcripts`。`architecture/transcript.md`と対で読む | 当たる |
| `tools.md` | LLMに公開するツールのスキーマ、引数検証、履歴への載せ方、外部(MCP)ツールの公開 | 公開する操作・権限を変えるなら当たる |
| `ui.md` | 画面を触るとき(必ず読む。`principles.md` 6節と、`architecture/webview-boundary.md`「画面が持つもの・持たないもの」も) | 描画だけの変更は当たらない。画面に持たせない処理を足すなら当たる(`architecture/webview-boundary.md`の行) |

## ファイルを跨ぐ不変条件

1つの文書だけを読んで変えると壊れる条件。変えるときは、挙げた文書をすべて読む。

- **前に送った部分を書き換えない**: 毎回変わるもの(日時・状態)をシステムプロンプトや直近の発言に
  添えない。`architecture/transcript.md`・`architecture/prompt-shape.md`・`architecture/llm-adapter.md`
  (Anthropic形式の思考ブロック)
- **ユーザー発言の囲みの中には、利用者が書いたものだけを置く**: 添付・操作の記録・通知は囲みの外に置く。
  `architecture/prompt-shape.md`・`architecture/attachments.md`・`architecture/transcript.md`(システムプロンプトの
  変更の通知)
- **保存するデータは書き換えず、無害化は出力先へ出す直前に出力先ごとに掛ける**: 送った形の保存は
  無害化済みのまま持つので、無害化の規則を変えたら保存の形の版を上げる。`architecture/sanitize.md`・
  `architecture/transcript.md`
- **画面は表示に徹する**: ファイル・OSの機能・入力の検証と規則・複数の処理をまとめる手順はRust側に
  置き、画面に写さない(入力欄で先に止めるための値はRust側から受け取って使う)。Rust側へ移す処理は、
  乗っ取った画面から呼ばれても困らない狭さにする。`principles.md` 4節「UI層は表示に徹する」・
  `architecture/webview-boundary.md`「画面が持つもの・持たないもの」・`ui.md`
- **画面からファイルのパスも中身も受け取らない**: 選ばせる・読む・書く・開くはRust側で行う。
  `architecture/attachments.md`「受け取り方」・`architecture/export.md`・`architecture/cli.md`(端末は例外)
- **秘密情報に触れるのは`secrets.rs`だけ、HTTPは`net::hardened_client`だけを通る**:
  `architecture/network-secrets.md`・`architecture/cli.md`・`architecture/llm-adapter.md`

## 用語

- **総合チャット**・**タスクチャット**: 全体を見渡す会話と、個別のタスクに紐づく会話
- **ターン**・**試行**・**ラウンド**: ターンは、ユーザー発言(または再試行・応答の生成の指示)に応える
  応答生成の単位(`turn_id`)。試行は、同じターンを作り直したそれぞれ(`attempt_no`)。ラウンドは、
  1つの試行の中のモデル呼び出し1回で、ツールを呼ぶたびに次のラウンドへ進む(ラウンドとツールの実行の組を「往復」と呼ぶ)
- **方言**: プロバイダーのAPIの形式(OpenAI互換・Anthropic形式・Gemini形式)
- **囲み**・**形式の説明**: 囲みは、ユーザー発言の本文を包む`<scitl:user-message>`。形式の説明は、
  囲みや予約タグの読み方をモデルに伝える注記(`llm::user_message_format_note`)
- **実行記録**・**操作の記録**: 実行記録は、ツールの実行1回分の`messages`の行。そのうち`turn_id`を
  持たない、応答生成の外(画面・CLI)からの操作を記録したものが操作の記録
- **送った形**・**前が変わる**: 送った形は、返信のある試行ごとに保存した、モデルへ送った発言列
  (`turn_transcripts`)。前が変わるとは、次のリクエストの先頭が、前に送ったリクエストと一致しなくなること

## ワークスペース構成

```
SCITL/
├── Cargo.toml                      # workspace root
├── crates/
│   ├── scitl-core/                 # UI非依存のコアライブラリ
│   │   └── src/
│   │       ├── db/                 # tasks/steps/messages/attachments/memoriesのrepository
│   │       ├── attachments/        # 添付の種別の判定・実体の置き場所・送信前の添付(architecture/attachments.md)
│   │       ├── export/             # Markdownエクスポート(architecture/export.md)
│   │       ├── llm/                # 方言によらない型・アダプタ・能力の解決・プロンプトの形式(architecture/llm-adapter.md・prompt-shape.md)
│   │       ├── tools/              # registry(面別スキーマ生成), args検証, 各ツール
│   │       ├── orchestration/      # 応答生成と、会話をモデル・画面へ渡す形の組み立て(architecture/transcript.md)
│   │       ├── mcp/                # 外部ツールサーバーのクライアント(streamable_http)
│   │       ├── net.rs              # 全HTTP経路が通るクライアント設定(architecture/network-secrets.md)
│   │       ├── secrets.rs          # OS資格情報ストアへの唯一の入口
│   │       ├── config.rs           # 設定(TOML)。秘密情報は参照(`key_ref`)だけを持つ
│   │       ├── paths.rs            # データディレクトリの場所と中の並び(GUI・CLI共通)
│   │       ├── files.rs            # 書きかけのファイルを完成した名前で残さない書き込み
│   │       ├── settings/           # 設定・登録の操作(規則・検証・秘密情報の出し入れ・保存)
│   │       ├── error.rs            # コア全体の失敗の種類(CoreError)
│   │       ├── diagnostics.rs      # coreの唯一の診断の出口。標準エラー、Androidではlogcat(architecture/sanitize.md)
│   │       ├── blocking.rs         # 非同期層からブロッキング処理を呼ぶ入口(architecture/concurrency.md)
│   │       ├── in_flight.rs        # 同じ対象への処理を同時に1本に絞る(タスクごとの応答生成)
│   │       ├── link.rs             # 本文中のリンクを開く前の判定とOSへの委譲(architecture/webview-boundary.md)
│   │       ├── text.rs             # 出力先を知らない文字単位の部品(architecture/sanitize.md)
│   │       └── i18n.rs             # 言語ファイルを引く入口と表示言語の一覧(architecture/i18n.md)
│   ├── scitl-cli/                  # タスクの確認・操作と会話の表示。scitl-coreのみに依存
│   ├── scitl-debug-cli/            # scitl-cliのコマンドに、応答生成・設定・登録を足す
│   └── scitl-tauri/                # 薄いIPCシェル
│       ├── tauri.conf.json         # CSP・devCsp
│       ├── gen/android/            # Androidのプロジェクト(architecture/tech-stack.md「Androidのビルド」)
│       └── src/commands/
├── frontend/                       # React + TypeScript + Vite。表示だけを持つ
├── lang/                           # ja.json / en.json(core・frontend共有。architecture/i18n.md)
└── migrations/                     # 0001_init.sql から番号順(data-model/tables.md 5節)
```

- **コア** (`scitl-core`): `principles.md` 5節が「1箇所に閉じる」ことを求める判断
  (状態遷移、ツール引数検証、ターンのオーケストレーション、サニタイズ、秘密情報アクセス)を
  すべて置く
- **GUI** (`scitl-tauri`): コマンドはロジックを持たず、coreを1つ呼ぶだけ(`architecture/webview-boundary.md`)
- **画面** (`frontend/`): 表示に徹し、判定・規則・手順を持たない(入力欄で先に止める補助を除く。
  `architecture/webview-boundary.md`「画面が持つもの・持たないもの」)
- **CLI** (`scitl-cli`・`scitl-debug-cli`): `architecture/cli.md`
