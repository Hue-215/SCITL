---
name: llm-relay
description: 疑似API(Sham_llm)とscitl-debug-cliで、APIキーを使わずにSCITLのターンの往復(ツール実行・保存まで)を動かす道具の使い方。LLM役(Sham_llmのnext/reply/inject_error)の務め方、scitl-debug-cliのコマンドと出力の読み方、試験用のドライバー、GUIを疑似APIにつなぐ方法、llama.cppでのプロンプトキャッシュの確かめ方を含む。プロバイダーのアダプタを足す・変えるとき、リクエストの組み立て(履歴・システムプロンプト・ツール定義・間引き)を変えたとき、APIキーを使わずにターンの往復を確かめるとき、ブラックボックステストでSCITLを操作するとき、scitl-debug-cliを使うときに開く。
---

# 疑似APIとscitl-debug-cliの使い方

- **scitl-debug-cli**: SCITLをGUI無しで操作するコマンド
- **Sham_llm**: LLMのAPIとして振る舞う疑似サーバー。応答の中身は、エージェント(または人)が
  「LLM役」として書く。本体は別リポジトリ[Hue-215/Sham_llm](https://github.com/Hue-215/Sham_llm)
  にあり、受け口・検査の範囲・LLM役の窓口・起動方法はそちらの文書を正とする。このリポジトリには
  含まれず、今は公開していない

LLM役は本物のモデルではない。言い回しやツールの選び方は本物と違いうるので、プロンプトの評価には
使わない。アダプタが固まったら本物のAPIで一度確かめる。

1〜3節は外から操作する道具の使い方で、ブラックボックステストのテスト役にはここだけを渡す。
4節以降はリポジトリの中身を使う確かめ方。

## 1. Sham_llmを用意する

`127.0.0.1:18080`で待ち受けていること。次で確かめる。

```sh
curl -s http://127.0.0.1:18080/v1/models    # {"object":"list","data":[{"id":"dummy-o",...}]} が返ればよい
```

返らなければ、手元に置いたSham_llmのリポジトリで`npm run dev`を実行して起動する
(Node.js 22以降)。

LLM役の窓口は、MCPサーバー`sham-llm`として`.mcp.json`(gitの管理外)に登録する。`next`は
リクエストが届くまで返らないので、タイムアウトを長くしておく。

```json
{ "mcpServers": { "sham-llm": { "type": "http", "url": "http://127.0.0.1:18080/mcp", "timeout": 3600000 } } }
```

登録すると、次のツールが使える。

| ツール | 用途 |
|---|---|
| `mcp__sham-llm__next` | アプリから届いたリクエストを1件受け取る |
| `mcp__sham-llm__reply` | 受け取ったリクエストへ応答を返す |
| `mcp__sham-llm__status` | 待ち行列・注入待ちのエラーを見る |
| `mcp__sham-llm__inject_error` | 次のリクエストにエラーを返させる |

遅延ツールとして出ているときは、ToolSearchで`select:mcp__sham-llm__next,mcp__sham-llm__reply,mcp__sham-llm__status,mcp__sham-llm__inject_error`
を読み込んでから呼ぶ。

疑似APIは方言ごとにダミーのモデルを1つだけ受ける。

| 方言 | `--api-format` | `--base-url` | モデル |
|---|---|---|---|
| OpenAI互換 | `open_ai_compat` | `http://127.0.0.1:18080/v1` | `dummy-o` |
| Anthropic形式 | `anthropic` | `http://127.0.0.1:18080` | `dummy-a` |
| Gemini形式 | `gemini` | `http://127.0.0.1:18080` | `dummy-g` |

鍵はどの方言も、空でない適当な値を入れる。

## 2. scitl-debug-cliで動かす

GUIと同じ設定の読み方(設定ファイル・資格情報ストア・能力の自動検出)まで含めて動かせる。

### 2.1 準備

リポジトリの最上位でビルドし、できたバイナリを使う。

```sh
cargo build -q -p scitl-debug-cli
B=$PWD/target/debug/scitl-debug-cli
D=<試験用の空ディレクトリ>; mkdir -p "$D"     # 無いディレクトリを指すとエラーになる
cli() { "$B" --data-dir "$D" "$@"; }
```

シェルの変数・関数はBashの呼び出しを跨いで残らない。コマンドごとに定義し直すか、毎回フルパスで書く。
OSの資格情報ストア(LinuxではSecret Service)が使える環境であること。

### 2.2 疑似APIをプロバイダーとして登録する

APIキーは値ではなく**環境変数の名前**で渡す(空でない適当な値でよい)。

```sh
RELAY_KEY=dummy cli provider add --name sham --api-format open_ai_compat \
  --base-url http://127.0.0.1:18080/v1 --api-key-env RELAY_KEY
# 出力の providers[].id がプロバイダーのID(以下 <P>)
cli model add <P> dummy-o
cli model select <P> dummy-o
cli settings general --response-timeout-secs 900   # 既定の120秒ではLLM役が間に合わないことがある
```

`settings general`と`settings tools`は、**指定しなかった項目を既定値に戻す**。値を残したい項目は毎回渡す。

### 2.3 応答生成の回し方

応答を生成するコマンド(`task create`・`chat send`・`chat retry`・`chat reply`)は、LLM役が`reply`するまで
返ってこない。あなたがLLM役も務めるので、次の順に進める。

1. 生成するコマンドを**バックグラウンドで**起動し、出力をファイルへ書かせる
   (Bashの`run_in_background`を使うか、`( cli chat send ... > out.jsonl 2>&1; echo "exit=$?" >> out.jsonl ) &`)
2. `mcp__sham-llm__next`でリクエストを受け取る
3. `mcp__sham-llm__reply`で応答を返す。ツール呼び出しを返した場合は、アプリがツールを実行して
   次のリクエストを送ってくるので、2に戻る
4. 本文だけの応答を返したらターンは終わる。出力ファイルを読む

1ターンの中でツール呼び出しを何往復まで許すかは`settings tools`で決まる。

LLM役を自分で務めずに、サブエージェントに任せてもよい。定義はこのスキルの`llm-role.md`にあり、
`.claude/agents/`か`~/.claude/agents/`へ写して使う(`next`と`reply`だけを持つ)。
場面ごとに決まった応答を返させたいときは、台本を渡して立てる。

### 2.4 コマンド

全体は`cli --help`、各コマンドは`cli <コマンド> --help`で見られる。

| コマンド | 内容 |
|---|---|
| `task list` / `task show <ID>` | タスクの一覧・詳細 |
| `task rename <ID> <タイトル>` / `archive` / `unarchive` / `delete <ID>` | タスクの操作(画面からの操作に相当) |
| `task create` | タスクを作り、最初の応答を生成する |
| `chat show [--task <ID>]` | 会話の発言の一覧。`--task`無しは総合チャット |
| `chat send [--task <ID>] [--attach <FILE>]... [本文]` | 発言を送り、応答を生成する。`--attach`は繰り返せる |
| `chat retry [--task <ID>] <発言ID>` | 応答(またはエラー発言)を作り直す |
| `chat reply [--task <ID>]` | 返信の無いまま終わった会話の応答を生成する。何も消さない |
| `chat preview [--task <ID>] [--message <本文>] [--external-tools]` | 次のターンで送るリクエストの本文を表示する。送信も保存もしない |
| `attachment list` / `attachment orphans [--delete]` | 添付の一覧、どの添付からも指されていないファイル |
| `export` | 全タスクと総合チャットをMarkdownでデータディレクトリの下へ書き出す |
| `settings show` / `general` / `tools` / `language <コード>` | 設定の表示・変更 |
| `provider add` / `delete <P>` / `models <P>` | プロバイダーの登録・削除・提供モデルの問い合わせ |
| `model add <P> <モデル>...` / `remove` / `select` | モデルの登録・削除・選択 |
| `mcp add-http` / `delete` / `enable` / `disable` / `enable-tool` / `disable-tool` / `fetch-tools` | 外部ツール(MCP)サーバーの登録と管理。秘密情報は`NAME=VAR`(VARは環境変数の名前)で渡す |

#### 出力と終了コード

- 出力はすべてJSON。表示系は字下げ付きのJSONを1つ、設定を変えるコマンドは変更後の設定全体を出す。
  タスク操作(`rename`等)は成功しても何も出さない
- 生成するコマンドは**1行に1つのJSON**を出す。`type`で見分ける
  - `task_created`: 作られたタスク(`task create`のみ、最初の行)
  - `response`: モデルの応答の途中経過。`event.type`が`reasoning_delta`・`text_delta`・`tool_call`・`done`
  - `tool_executed`: アプリがツールを実行した結果
  - `last_message`: 最後の行。会話の最後の発言
- **ターンの失敗は終了コード0**で、エラー発言として保存される。`last_message`の`message.role`が`"error"`になり、
  `error_kind`(`rate_limit`・`no_provider`等)と`error_detail`が入る
- コマンド自体の失敗(存在しないID等)は終了コード1で、標準エラーに`error: …`を出す
- 見えない文字(制御文字・書式文字)は`\uXXXX`の形で出る

### 2.5 例: タスクを作って1往復する

```sh
# 1. タスクを作る(バックグラウンド)
( cli task create > create.jsonl 2>&1; echo "exit=$?" >> create.jsonl ) &
```

2. `next` → 挨拶への応答を`reply`(`text`と`thinking`) → `create.jsonl`の`last_message`を確かめる

```sh
# 3. 発言を送る(バックグラウンド)
( cli chat send --task 1 "来週金曜までに企画書を出したい" > send.jsonl 2>&1; echo "exit=$?" >> send.jsonl ) &
```

4. `next` → `tool_calls: [{"name":"update_task","arguments":{"title":"企画書の提出","deadline":"2026-10-09"}}]`を`reply`
5. `next`(ツールの結果が届く) → 本文を`reply`
6. `send.jsonl`と`cli task show 1`で結果を確かめる

### 2.6 後始末

```sh
cli provider delete <P>     # APIキーも資格情報ストアから消える。MCPサーバーを登録したら mcp delete も
rm -rf "$D"
```

`next`で受け取ったまま`reply`していないリクエストが残っていないかを`mcp__sham-llm__status`で確かめる。

## 3. LLM役の書き方

### 3.1 `next`の表示

```
#8 新規
[system]
…
[tools]
{"name":"update_task", …}
[user]
…
[constraints] thinking=on effort=medium
```

- 1行目の`#番号 新規`は新しい会話、`#番号 続き`は同じ会話の続き(前回までに表示した部分と自分の応答は
  省かれる)。同じターンの中でも、ツールの結果を返すリクエストが`新規`として全体を表示し直すことがある
- `[system]`・`[tools]`・`[user]`・`[assistant]`は発言の区切り、`[tool_call 名前]`は自分が呼んだツール、
  `[tool_result 名前]`はその結果
- `[constraints]`は応答に課される条件
- `TIMEOUT`で始まる結果は「まだ届いていない」の意味。もう一度呼ぶ(`timeout_s`で待つ秒数を指定できる)

### 3.2 `reply`の引数

| 引数 | 内容 |
|---|---|
| `text` | 応答の本文 |
| `tool_calls` | `[{"name": "<[tools]にある名前>", "arguments": {...}}]` |
| `thinking` | `[constraints]`に`thinking=on`があるときに添える思考。1〜2文でよい |

結果が`OK`で始まれば受け付けられている。`OK`の後に行が続くときは、アプリへ届く前に変えられた点。
`差し戻し:`で始まったら、理由に従って直して`reply`し直す。

ツールの引数を不正にする(必須の引数を抜く・型を違える等)など、モデルが誤った場合の振る舞いも
`reply`の中身で作れる。

### 3.3 方言ごとの違い

- `dummy-o`: 思考はOpenAI互換サーバー(llama.cpp等)の形で返る
- `dummy-a`: Anthropicの現行モデルに合わせてある(どのモデルかはSham_llmの文書)。強制ツール使用・プリフィル・既定以外のサンプリング指定は400で断る
- `dummy-g`: 思考は常に有効

### 3.4 エラーを返させる

`mcp__sham-llm__inject_error`を、**生成するコマンドを起動する前に**呼ぶ。次の`count`件(既定1)の
リクエストに、LLM役を通さずエラーを返す。

| 引数 | 内容 |
|---|---|
| `status` | HTTPステータス(400〜599)。必須 |
| `count` | 何件に返すか |
| `dialect` | `openai`・`anthropic`・`gemini`・`any`(既定) |
| `message`・`type` | エラー本文の中身 |
| `retry_after_s` | `Retry-After`ヘッダーの秒数 |

注入待ちのエラーと待ち行列は`mcp__sham-llm__status`で見られる。

## 4. ドライバーで動かす

`crates/scitl-core/examples/relay_session.rs`は、GUIと同じ入口(`create_task`・`open_task_chat`・
`run_turn`)を台本どおりに呼ぶ。設定ファイルと資格情報ストアを使わずにアダプタとターンの文脈を
直に組み立てるので、Secret Serviceの無い環境でも動き、設定からは起こしにくい場面(実体の無い
外部ツールの定義を出し入れする、コンテキスト長を狭める)を作れる。設定の読み方まで含めて確かめる
ときは2節を使う。

リポジトリの最上位で:

```sh
cargo run -p scitl-core --example relay_session -- \
  target/relay-data http://127.0.0.1:18080/v1 \
  @new "来週の金曜までに企画書を出したい" "下書きは今日終わったよ"
```

- 方言は環境変数`RELAY_DIALECT`(`openai`(既定)・`anthropic`・`gemini`)で選ぶ。BASE_URLは1節の表の
  とおりで、Anthropic形式とGemini形式は`/v1`を付けない。この2つは思考の強さ「中」で呼ぶ
- STEPは次のとおり。途中経過(本文・思考・ツールの実行)は標準出力に出る
  - `@new`: タスクを作って聞き取りを始める
  - `@task <id>`: 既にあるタスクの会話へ移る
  - `@general`: 総合チャットへ移る
  - `@base <文>`: 以降のターンの基本のシステムプロンプトを差し替える(変更の通知を確かめるため)
  - `@tools on`/`@tools off`: 以降のターンで外部ツールを1つ有効・無効にする(ツール定義が変わる場面を
    確かめるため。サーバーの実体は無く、呼ばれたら失敗を返す)
  - それ以外: 今の会話へのユーザー発言
- `@base`と`@tools`は起動の間だけ効く。同じDATA_DIRで起動し直して会話を続けるときは毎回渡す
  (渡さないと既定に戻り、それも変更として扱われる)
- システムプロンプトは既定、外部ツールは`@tools on`にしない限り無し、モデルの能力は既定値(思考あり、
  画像なし)。能力の自動検出はしない
- 既定のコンテキスト長は4096で、システムプロンプトの変更の通知が1つ載るだけで間引きが起きる。
  間引き以外を確かめるときは環境変数`RELAY_CONTEXT_LENGTH`で広げる(間引きを確かめるときは、
  狭めて長い発言を重ねる)
- 結果はDATA_DIRをそのまま`scitl-cli --data-dir target/relay-data task show 1`等で読む
- Linuxでは`scitl-core`のビルドに`libdbus-1-dev`(`pkg-config`が`dbus-1`を見つけられること)が要る

## 5. GUIから使う

1. 普段使いのGUIを先に閉じる。GUIは1つしか起動せず、データディレクトリを変えて起動してもすぐに終わる
   (`docs/spec/architecture/concurrency.md`「多重起動の防止」)
2. 設定の「APIプロバイダー」で、1節の表の方言・URL・モデルで登録する
3. 設定「一般」の応答のタイムアウト(既定120秒)を延ばす。LLM役が考え込むと超える

能力の自動検出には疑似APIが答える(答える値はSham_llmの文書を参照)。検出結果はアプリの起動中、
モデルごとに覚えられる。

## 6. 本物のサーバーでプロンプトキャッシュを確かめる

リクエストの組み立てを変えたあと、前に送った部分が変わっていないか
(`docs/spec/architecture/transcript.md`「前に送った部分を書き換えない」)を、llama.cpp(llama-server)の
プロンプトキャッシュの当たり方で確かめる。ドライバーを本物のOpenAI互換サーバーに向け、モデル名を
環境変数`RELAY_MODEL`で渡す。

```sh
RELAY_MODEL=<サーバー側のモデル名> RELAY_CONTEXT_LENGTH=16384 \
  cargo run -p scitl-core --example relay_session -- \
  target/relay-data http://127.0.0.1:<ポート>/v1 "1つ目の発言"
```

- `RELAY_CONTEXT_LENGTH`はサーバーのコンテキスト長(llama-serverなら`GET /props`の
  `default_generation_settings.n_ctx`)以下にする
- 1ターンごとに起動し直す。その間のサーバーのログがそのターンのリクエストに対応する(総合チャットは
  そのまま続き、タスクの会話は`@task <id>`で続ける)

llama-serverはリクエストごとに次を出す。

- `prompt eval time = … / N tokens`: 計算し直したプロンプトのトークン数
- `eval time = … / G tokens`: 生成したトークン数
- `stop processing: n_tokens = T`: 終了時にスロットにあるトークン数

プロンプト全体は`T - G`、キャッシュから使えた分は`T - G - N`。前のリクエストのプロンプト全体と
同じかそれ以上なら、前のリクエストの末尾まで当たっている。前の応答が思考を含まなければ、応答の分まで
当たる(生成したトークン列と、送り返した発言のトークン列が一致するため)。思考を含むと、OpenAI互換では
思考を送り返さないので、応答の分は計算し直しになる。
