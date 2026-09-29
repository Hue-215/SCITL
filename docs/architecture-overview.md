# アーキテクチャの概観図

全体を読み通さずに構成を掴むための地図。方針と理由は`spec/rebuild/architecture.md`、
スキーマは`spec/rebuild/data-model.md`が正本であり、図と食い違う場合は常にそちらとコードを
正とする。図にはそれらに無い判断を書かない。

モジュールの分け方・プロセス境界・1ターンの処理の順番・テーブルの関係が変わったときに更新する。

## 1. 全体構成(プロセス境界とモジュール)

WebViewは表示に徹し、秘密情報・外部通信・データベースへの経路はすべて`scitl-core`を通る
(`architecture.md` 1節・5節・6節・7節)。

```mermaid
flowchart LR
  subgraph WV["WebView: frontend/ (React + TS)"]
    UI["画面<br/>App / Sidebar / ChatLog / Settings…"]
    MD["Markdown.tsx<br/>remarkInertHtmlで無害化"]
    API["api.ts<br/>invoke + Channel"]
    BIND["bindings/<br/>ts-rsで生成した型"]
  end

  subgraph TA["scitl-tauri: 薄いIPCシェル"]
    CMD["commands/*.rs<br/>デシリアライズ → coreを1つ呼ぶ → シリアライズ"]
    CONF["tauri.conf.json<br/>CSP: connect-src 'none'"]
  end

  CLI["scitl-cli<br/>JSON出力 / 応答生成はしない / preview"]

  subgraph CORE["scitl-core: UI非依存"]
    ORCH["orchestration/<br/>turn・operations・preview"]
    TOOLS["tools/<br/>引数検証・内部ツール"]
    LLM["llm/<br/>LlmAdapter trait・providers/"]
    MCP["mcp/<br/>stdio / streamable_http"]
    DB["db/<br/>同期repository"]
    ATT["attachments/"]
    SET["settings/ + config.rs"]
    SEC["secrets.rs<br/>資格情報の唯一の入口"]
    NET["net.rs<br/>hardened_client"]
    LINK["link.rs"]
    EXP["export/"]
  end

  SQLITE[("SQLite (WAL)<br/>migrations/")]
  FILES[("添付の実体")]
  TOML[("config.toml<br/>鍵の参照のみ")]
  KEYRING[("OS資格情報ストア")]
  LLMAPI(["LLM API<br/>クラウド / ローカル推論"])
  MCPS(["外部ツールサーバー"])
  OS(["OS: ブラウザ等"])

  UI --> MD
  UI --> API
  API -- invoke --> CMD
  CMD -. "TurnEvent (Channel)" .-> API
  CMD --> CORE
  CLI --> CORE

  ORCH --> LLM
  ORCH --> TOOLS
  ORCH -- spawn_blocking --> DB
  ORCH --> ATT
  TOOLS --> DB
  TOOLS -- 外部ツール --> MCP
  SET --> SEC
  SET --> TOML
  LLM --> NET
  MCP -- http --> NET
  MCP -- stdio --> MCPS
  NET --> LLMAPI
  NET --> MCPS
  SEC --> KEYRING
  DB --> SQLITE
  ATT --> FILES
  LINK -- 判定し直してから委譲 --> OS
```

## 2. 1ターンの流れ(チャット送信)

`orchestration::turn`の`run_turn`から`run_tool_rounds`まで。履歴は最初に1度だけ読み、
最新状態と間引きはラウンドごとにやり直す(`architecture.md` 3節、`tools.md` 4節)。

```mermaid
sequenceDiagram
  participant U as 画面
  participant C as commands::chat
  participant T as orchestration::turn
  participant D as db
  participant A as LlmAdapter
  participant X as tools / MCP

  U->>C: send_chat_message(タスク, 本文, 添付, Channel)
  C->>T: run_turn
  T->>T: in_flightでタスクごとに1本に絞る
  T->>D: ユーザー発言と添付を1トランザクションで保存
  T->>X: 外部ツールの一覧(有効なサーバーのみ、取得済みなら再利用)
  T->>D: 履歴を読む
  loop ラウンド(ツールの往復の上限 + 最後の1回)
    T->>D: 最新状態を読み、履歴を間引く
    T->>A: 発言列 + ツール(最後の1回はツール無し)
    A-->>T: イベント列(本文/思考デルタ・ツール呼び出し・終了理由)
    T-->>U: TurnEvent(途中経過)
    alt ツール呼び出しあり
      T->>X: 引数検証 → 実行
      X->>D: タスク / ステップを更新
      T->>D: 実行記録を保存
    else ツール呼び出しなし
      T->>D: 応答を保存して終了
    end
  end
  Note over T,D: 失敗はどの段でも種類を付けたエラー発言として保存する
  C-->>U: 完了 → list_chat_messagesで読み直し
```

## 3. データモデル

論理削除(`deleted_at`)と、発言が属するターン(`turn_id`・`attempt_no`)の意味は
`data-model.md`を参照。

```mermaid
erDiagram
  tasks ||--o{ task_steps : has
  tasks |o--o{ messages : has
  messages ||--o{ attachments : has

  tasks {
    int id
    text title
    text deadline
    text archived_at
    text deleted_at
  }
  task_steps {
    int id
    int task_id
    text description
    text done_at
    int order_index
  }
  messages {
    int id
    int task_id
    text role
    text kind
    text turn_id
    int attempt_no
    text deleted_at
  }
  attachments {
    int id
    int message_id
    text kind
    text content_text
    text file_hash
  }
```
