---
name: blackbox-test
description: scitl-debug-cliと疑似API(Sham_llm)でSCITLを外から操作して振る舞いを確かめるブラックボックステストの手順と、見つかった指摘を検証してGitHub Issueにするまでの扱い。ブラックボックステストを行うとき、テスト役・判定役のサブエージェントを立てるとき、LLM役(Sham_llmのnext/reply/inject_error)を務めるとき、scitl-debug-cliのコマンドや出力の読み方が要るとき、レビューやテストで出た指摘をIssueにするときに開く。
---

# scitl-debug-cli と Sham_llm によるブラックボックステストの手順

SCITLを外から操作して振る舞いを確かめるための、道具の使い方だけを書いた手順書。
使う道具は次の2つ。

- **scitl-debug-cli**: SCITLをGUI無しで操作するコマンド
- **Sham_llm**: LLMのAPIとして振る舞う疑似サーバー。応答の中身はあなたが「LLM役」として書く

## 0. 守ること

- **ブラックボックスとして扱う。** リポジトリのソースコード(`crates/`・`frontend/`等)と`docs/`は
  読まない。期待する振る舞いは、コマンドのヘルプ・出力・LLM役に届くリクエストから判断する
- **`--data-dir`を必ず付ける。** 省略すると、ユーザーが普段使っているGUIのデータを開いて書き換える。
  試験用のディレクトリを作って毎回それを指す
- 終わったら、登録したプロバイダーを消す(7節)。APIキーがOSの資格情報ストアに残るため

## 0.1 試験の範囲と体制

- **試験の範囲は、GUIからの操作と通常のCLI(デバッグ用でないもの)で起こり得る振る舞いに限る。**
  scitl-debug-cliは、その操作を外から再現するための道具として使う。scitl-debug-cliでしかできない操作
  (同じデータディレクトリへの同時実行、`config.toml`の手書き、データディレクトリのコピー等)でしか
  起こらない不具合は、検証も報告もしない
- **テスト役は、機能の領域(設定・チャット・タスク・添付・外部ツール等)ごとにOpusを1体ずつ立てる。**
  1体に広い範囲を任せると、会話が長くなって読み直すトークンが膨らむ
- **判定役(不具合候補を再確認する役)は、Sonnetを1体だけ立てて、候補を直列に渡す。**
  候補1件ごとに別の判定役を立てない
- Sham_llmの待ち行列は1つしかないため、LLM役を務めるテスト役は同時に1体だけにする
- **テスト役は、見つかった項目を最終報告で返す。** 「GUI・通常のCLIの操作で起こるもの」
  「GUIとCLIを同時に操作した場合に起こるもの」「未検証」に分ける。Issueにするのはメインセッション(8節)

## 1. 準備

### 1.1 scitl-debug-cli

リポジトリの最上位でビルドし、できたバイナリを使う。

```sh
cargo build -q -p scitl-debug-cli
B=$PWD/target/debug/scitl-debug-cli
D=<試験用の空ディレクトリ>; mkdir -p "$D"     # 無いディレクトリを指すとエラーになる
cli() { "$B" --data-dir "$D" "$@"; }
```

シェルの変数・関数はBashの呼び出しを跨いで残らない。コマンドごとに定義し直すか、毎回フルパスで書く。
OSの資格情報ストア(LinuxではSecret Service)が使える環境であること。

### 1.2 Sham_llm

`127.0.0.1:18080`で待ち受けていること。次で確かめる。

```sh
curl -s http://127.0.0.1:18080/v1/models    # {"object":"list","data":[{"id":"dummy-o",...}]} が返ればよい
```

返らなければ、Sham_llmのリポジトリ(`~/Documents/Sham_llm`)で`npm run dev`を実行して起動する
(Node.js 22以降)。

LLM役の窓口は、このプロジェクトに登録済みのMCPサーバー`sham-llm`のツールとして使える。

| ツール | 用途 |
|---|---|
| `mcp__sham-llm__next` | アプリから届いたリクエストを1件受け取る |
| `mcp__sham-llm__reply` | 受け取ったリクエストへ応答を返す |
| `mcp__sham-llm__status` | 待ち行列・注入待ちのエラーを見る |
| `mcp__sham-llm__inject_error` | 次のリクエストにエラーを返させる |

遅延ツールとして出ているときは、ToolSearchで`select:mcp__sham-llm__next,mcp__sham-llm__reply,mcp__sham-llm__status,mcp__sham-llm__inject_error`
を読み込んでから呼ぶ。

## 2. 疑似APIをプロバイダーとして登録する

疑似APIは方言ごとにダミーのモデルを1つだけ受ける。

| 方言 | `--api-format` | `--base-url` | モデル |
|---|---|---|---|
| OpenAI互換 | `open_ai_compat` | `http://127.0.0.1:18080/v1` | `dummy-o` |
| Anthropic形式 | `anthropic` | `http://127.0.0.1:18080` | `dummy-a` |
| Gemini形式 | `gemini` | `http://127.0.0.1:18080` | `dummy-g` |

`localhost`ではなく`127.0.0.1`と書く。APIキーは値ではなく**環境変数の名前**で渡す(空でない適当な値でよい)。

```sh
RELAY_KEY=dummy cli provider add --name sham --api-format open_ai_compat \
  --base-url http://127.0.0.1:18080/v1 --api-key-env RELAY_KEY
# 出力の providers[].id がプロバイダーのID(以下 <P>)
cli model add <P> dummy-o
cli model select <P> dummy-o
cli settings general --response-timeout-secs 900   # 既定の120秒ではLLM役が間に合わないことがある
```

`settings general`と`settings tools`は、**指定しなかった項目を既定値に戻す**。値を残したい項目は毎回渡す。

## 3. 応答生成の回し方

応答を生成するコマンド(`task create`・`chat send`・`chat retry`)は、LLM役が`reply`するまで
返ってこない。あなたがLLM役も務めるので、次の順に進める。

1. 生成するコマンドを**バックグラウンドで**起動し、出力をファイルへ書かせる
   (Bashの`run_in_background`を使うか、`( cli chat send ... > out.jsonl 2>&1; echo "exit=$?" >> out.jsonl ) &`)
2. `mcp__sham-llm__next`でリクエストを受け取る
3. `mcp__sham-llm__reply`で応答を返す。ツール呼び出しを返した場合は、アプリがツールを実行して
   次のリクエストを送ってくるので、2に戻る
4. 本文だけの応答を返したらターンは終わる。出力ファイルを読む

1ターンの中でツール呼び出しを何往復まで許すかは`settings tools`で決まる。

LLM役を自分で務めずに、サブエージェント`llm-role`に任せてもよい(`next`と`reply`だけを持つ)。
場面ごとに決まった応答を返させたいときは、台本を渡して立てる。

## 4. LLM役の書き方(Sham_llm)

### 4.1 `next`の表示

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

### 4.2 `reply`の引数

| 引数 | 内容 |
|---|---|
| `text` | 応答の本文 |
| `tool_calls` | `[{"name": "<[tools]にある名前>", "arguments": {...}}]` |
| `thinking` | `[constraints]`に`thinking=on`があるときに添える思考。1〜2文でよい |

結果が`OK`で始まれば受け付けられている。`OK`の後に行が続くときは、アプリへ届く前に変えられた点。
`差し戻し:`で始まったら、理由に従って直して`reply`し直す。

ツールの引数を不正にする(必須の引数を抜く・型を違える等)など、モデルが誤った場合の振る舞いも
`reply`の中身で作れる。

### 4.3 方言ごとの違い

- `dummy-o`: 思考はOpenAI互換サーバー(llama.cpp等)の形で返る
- `dummy-a`: Anthropicの現行モデルに合わせてある(どのモデルかはSham_llmの文書)。強制ツール使用・プリフィル・既定以外のサンプリング指定は400で断る
- `dummy-g`: 思考は常に有効

### 4.4 エラーを返させる

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

## 5. scitl-debug-cliのコマンド

全体は`cli --help`、各コマンドは`cli <コマンド> --help`で見られる。

| コマンド | 内容 |
|---|---|
| `task list` / `task show <ID>` | タスクの一覧・詳細 |
| `task rename <ID> <タイトル>` / `archive` / `unarchive` / `delete <ID>` | タスクの操作(画面からの操作に相当) |
| `task create` | タスクを作り、最初の応答を生成する |
| `chat show [--task <ID>]` | 会話の発言の一覧。`--task`無しは総合チャット |
| `chat send [--task <ID>] [--attach <FILE>]... [本文]` | 発言を送り、応答を生成する。`--attach`は繰り返せる |
| `chat retry [--task <ID>] <発言ID>` | 応答(またはエラー発言)を作り直す |
| `chat preview [--task <ID>] [--message <本文>] [--external-tools]` | 次のターンで送るリクエストの本文を表示する。送信も保存もしない |
| `attachment list` / `attachment orphans [--delete]` | 添付の一覧、どの添付からも指されていないファイル |
| `export` | 全タスクと総合チャットをMarkdownでデータディレクトリの下へ書き出す |
| `settings show` / `general` / `tools` / `language <コード>` | 設定の表示・変更 |
| `provider add` / `delete <P>` / `models <P>` | プロバイダーの登録・削除・提供モデルの問い合わせ |
| `model add <P> <モデル>...` / `remove` / `select` | モデルの登録・削除・選択 |
| `mcp add-stdio` / `add-http` / `delete` / `enable` / `disable` / `enable-tool` / `disable-tool` / `fetch-tools` | 外部ツール(MCP)サーバーの登録と管理。秘密情報は`NAME=VAR`(VARは環境変数の名前)で渡す |

### 5.1 出力と終了コード

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

## 6. 例: タスクを作って1往復する

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

## 7. 後始末

```sh
cli provider delete <P>     # APIキーも資格情報ストアから消える。MCPサーバーを登録したら mcp delete も
rm -rf "$D"
```

`next`で受け取ったまま`reply`していないリクエストが残っていないかを`mcp__sham-llm__status`で確かめる。

## 8. 指摘をIssueにする

テストやレビューで出た指摘は、ファイルに溜めずに次の手順で直接Issueにする(メインセッションが行う)。

1. 既にIssueのあるものは、新しく起票せずにそのIssueへ足す
2. 未検証の指摘は、コードを読むか、実機(本手順)で再現して検証する。再現しなかったもの・
   仕様どおりだったものは、Issueにせずに捨てる
3. 検証済みの指摘をIssueにする。細かい修正で、同じ箇所・同じ種類のものは1つのIssueにまとめる。
   Issueの本文に出所(どのテスト・レビューで見つかったか)を書く
4. テストを途中で止めた・道具の都合で試せなかった領域は、「ブラックボックステストでまだ試していない
   領域」のIssueに足す
