# 疑似APIによる往復の確認(llm-relay)

プロバイダーのアダプタを足す・変えるときに、APIキーを使わずにターンの往復(ツール実行・保存まで)を
確かめるための仕組み。モデルの応答は、エージェント(または人)が「LLM役」として書く。道具は
`tools/llm-relay/` と `crates/scitl-core/examples/relay_session.rs` にある。

この文書は道具の現在の仕様であり、アダプタの設計方針ではない。方言対応の作業は #81 で扱う。

## 1. 構成

```
relay_session ─HTTP─▶ mock_llm.py ─box/req-NNNN.json─▶ LLM役
またはGUI              (疑似API) ◀─box/res-NNNN.json─  llm_relay.py next / reply
(本物のアダプタ)          │
                          └─ /v1/messages は anthropic_rules.py の検査を先に通す
```

| ファイル | 役割 |
|---|---|
| `tools/llm-relay/mock_llm.py` | `127.0.0.1:18080` で受ける疑似API。受けたリクエストを `box/` に書き、応答ファイルが置かれるまで待って返す(最大900秒) |
| `tools/llm-relay/anthropic_rules.py` | `/v1/messages` の検査と、Anthropic形式の応答の組み立て |
| `tools/llm-relay/llm_relay.py` | LLM役の窓口。`next` で次の未回答リクエストを表示し、`reply ID` で応答を書く |
| `tools/llm-relay/test_anthropic_rules.py` | 検査の自己テスト |
| `crates/scitl-core/examples/relay_session.rs` | GUIと同じ入口(`create_task`・`open_task_chat`・`run_turn`)を台本どおりに呼ぶドライバー |

ドライバーはCLIではなくexampleにしている。CLIは応答生成をしない(`scitl-cli` の方針)ため。

## 2. 中継の約束事

`box/` に置くファイル:

| ファイル | 書く側 | 中身 |
|---|---|---|
| `req-NNNN.json` | 疑似API | 受けたリクエスト本文。Anthropic形式には `_format: "anthropic"` と検査の警告 `_lint` を足す |
| `res-NNNN.json` | LLM役 | 応答(下記)。置かれた時点で疑似APIが返す |
| `rejected-NNNN.json` | 疑似API | 検査で断ったリクエストと理由。LLM役には渡さない |
| `done` | ドライバーを動かす側 | これがあると `llm_relay.py next` は `DONE` を返して終わる |

LLM役の応答は、方言に関係なく同じ形で書く。方言の形への変換は疑似APIが行う。

```json
{"content": "本文。ツールだけ呼ぶなら null",
 "tool_calls": [{"name": "add_steps", "arguments": {"descriptions": ["下書き"]}}],
 "reasoning_content": "思考(任意)",
 "stop_reason": "max_tokens"}
```

`stop_reason` は任意で、打ち切り等を試すときだけ書く(Anthropic形式のみ)。エラー応答を試すときは
次の形を書く。Anthropic形式では本物と同じエラーの形(`type`・`error`・`request_id`)と
`request-id` ヘッダーで返る。

```json
{"error": {"status": 429, "type": "rate_limit_error", "message": "..."}}
```

`llm_relay.py next` は、システムプロンプトとツール定義を前回の表示から変わっていなければ省く。

## 3. 受け口

### 3.1 OpenAI互換(`/v1/chat/completions`, `/v1/models`)

検査しない。互換を名乗るサーバーごとに挙動が違い、基準にする文書が無いため。非ストリーミングの
`chat.completion` の形で返し、`reasoning_content` は `message.reasoning_content` に載せる。
`/v1/models` は `relay-model` の1件を返す。

### 3.2 Anthropic形式(`/v1/messages`)

検査に通ったリクエストだけをLLM役へ渡す。応答は `Message` の形
(`id`・`type: "message"`・`role`・`content`・`model`・`stop_reason`・`stop_sequence`・`usage`)。
`usage` のトークン数は本文の長さから作った目安で、意味は無い。

思考が有効になる条件(4節のモデル表)のとき、応答の先頭に `thinking` ブロックを付ける。本文は
LLM役の `reasoning_content`(無ければ空)、`signature` は疑似APIが作る値。このブロックを覚えておき、
次のリクエストの直近のアシスタント発言で消されたり変えられたりしていれば400で断る(4節)。

ストリーミング(`stream: true`)は実装していないので400で断る。

## 4. Anthropic形式の検査

**400等を返すのは、公式ドキュメントに書かれている規則だけ。** 文書で確かめられなかったものは警告に
留め、リクエストは通す。エラーの文言は、文書に原文があるものは原文どおりにし、無いものは
`[mock wording]` を付けて独自の文言であることを示す。規則ごとの出典はコードに書いてある。

### 4.1 断るもの

| 対象 | 規則 | 状態コード | 出典 |
|---|---|---|---|
| ヘッダー | `Authorization: Bearer` か `x-api-key` が無い | 401 | overview, errors |
| ヘッダー | `anthropic-version` が無い、`content-type` が `application/json` でない | 400(状態コードは推定) | overview, versioning |
| 本文 | 32MBを超える | 413 | overview, errors |
| 本文 | `model`・`max_tokens`・`messages` が無い。`max_tokens` が0以上の整数でない | 400 | messages |
| 本文 | モデル表に無い `model` | 404(`model: <id>` で始まる) | errors |
| `system` | 文字列でも text ブロックの配列でもない。空の text | 400 | messages |
| `messages` | 10万件を超える。ロールが `user`・`assistant`・`system` 以外 | 400 | messages |
| ブロック | ロールに許されない種類。空の text | 400 | messages |
| 画像 | 形式が jpeg・png・gif・webp 以外。base64で10MBを超える。data URLのまま。枚数の上限を超える | 400 | vision |
| `tool_use` | `id` の形式、`name` の長さ(1〜200)、`input` がオブジェクトでない | 400 | messages |
| `tool_result` | `tool_use_id` の形式。`content` に text・image・document・search_result 以外 | 400 | messages, tools |
| ツールの往復 | `tool_use` の直後のターンに、対応する `tool_result` が無い | 400 | tools |
| ツールの往復 | `tool_result` より前に text 等がある | 400 | tools |
| `tools` | 名前が `^[a-zA-Z0-9_-]{1,128}$` に合わない。`input_schema.type` が `object` でない | 400 | messages |
| `tool_choice` | `any`・`tool` をOpus 5.5・Sonnet 5.5・Fable 5.1・Mythos 5.1に送る | 400 | errors |
| プリフィル | 最後がアシスタント発言で、モデルが4.6以降 | 400 | errors |
| 思考の設定 | `enabled` を4.7以降に。`adaptive` を4.5以前に。`disabled` を切れないモデルに。`between_tools` をSonnet 5.5以外に、または effort が `xhigh`・`max` のときに。ベータヘッダー無しの `block_binding` | 400 | errors |
| 思考の往復 | 疑似APIが出した思考ブロックが、直近のアシスタント発言で消えている・変わっている | 400 | errors |

続く同じロールの発言は、API本体と同じく1ターンにまとめてから検査する
(「Consecutive `user` or `assistant` turns in your request will be combined into a single turn.」
messages)。

### 4.2 警告に留めるもの

- 同じロールの発言が続く(API本体はまとめて受け付ける。SCITLの送り方として見たい)
- 最初の発言がユーザー発言でない(取得した公式ページに記述が無い)
- `messages` の中の `system` ロール(公式ページ同士で記述が食い違う。リファレンスの本文は
  「system ロールは無い」、他のページは会話途中の system 発言を載せている)
- 対応する `tool_use` の無い `tool_result`、重複したツール名(エラーの文言が文書に無い)
- `temperature`・`top_p`・`top_k`(新しいモデルで400になるとスキル同梱の資料にあるが、公式ページで
  確かめていない)
- 画像が20枚を超える(枚数が多いときの画像サイズの制限は検査しない)
- Anthropicが定義するツール(`type` が `custom` 以外)は検査しない

### 4.3 モデル表

`anthropic_rules.py` の `MODELS`。ID・世代・省略時に思考が有効か、を持つ。公式ページで直接
確かめたものではなく、claude-api スキル同梱の表(2026-09-25時点)から写した。モデル別の400の
判定(4.1節)はこの表の世代に依る。

### 4.4 出典

2026-09-29 に取得。

| キー | URL |
|---|---|
| overview | https://platform.claude.com/docs/en/api/overview |
| versioning | https://platform.claude.com/docs/en/api/versioning |
| errors | https://platform.claude.com/docs/en/api/errors |
| messages | https://platform.claude.com/docs/en/api/messages |
| tools | https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls |
| vision | https://platform.claude.com/docs/en/build-with-claude/vision |

## 5. 限界

- 検査は文書の読み取りなので、本物との差は残る。アダプタが固まったら本物のAPIで一度確かめる
- ストリーミング・サーバーツール・Files API・プロンプトキャッシュは扱わない
- Gemini形式は未実装。公式ドキュメント(`ai.google.dev`・`docs.cloud.google.com`)を読める環境で、
  どのAPI(Interactions API / `generateContent` / OpenAI互換の窓口)に対応するかを決めてから
  検査を書く(#81)
- LLM役は本物のモデルではない。言い回しやツールの選び方は本物と違いうるので、プロンプトの
  評価には使わない

## 6. 使い方

必要なもの: Python 3(標準ライブラリのみ)、Rustのビルド環境。Linuxでは `scitl-core` のビルドに
`libdbus-1-dev`(`pkg-config` が `dbus-1` を見つけられること)が要る。

```sh
cd tools/llm-relay
python3 -m unittest test_anthropic_rules     # 検査の自己テスト
python3 mock_llm.py > mock.log 2>&1 &        # 疑似API。box/ は tools/llm-relay/box
```

### 6.1 ドライバーで動かす

リポジトリの最上位で:

```sh
cargo run -p scitl-core --example relay_session -- \
  tools/llm-relay/data http://127.0.0.1:18080/v1 \
  @new "来週の金曜までに企画書を出したい" "下書きは今日終わったよ"
touch tools/llm-relay/box/done
```

STEPは `@new`(タスクを作って聞き取りを始める)・`@general`(総合チャットへ移る)・それ以外
(今の会話へのユーザー発言)。途中経過(本文・思考・ツールの実行)は標準出力に出る。DATA_DIRは
そのまま `scitl-cli --data-dir tools/llm-relay/data task show 1` 等で読める。

ドライバーが組み立てるのは今のところOpenAI互換のアダプタだけ。システムプロンプトは既定のもの、
外部ツール(MCP)は無し、モデルの能力は既定値(ツール・思考あり、画像なし)。

### 6.2 GUIから使う

設定の「APIプロバイダー」で、OpenAI互換・URL `http://127.0.0.1:18080/v1` のプロバイダーを登録する。
平文の `http` はIPリテラルのループバックに限って通るので、`localhost` ではなく `127.0.0.1` と書く。
鍵は空か適当な値でよい(OpenAI互換の受け口は認証を見ない)。モデル一覧には `relay-model` が出る。

ループバックのURLには、モデルの能力の自動検出が走る。疑似APIは能力の問い合わせに答えない(404)
ので、必要ならモデル表で能力を手動で設定する。

### 6.3 LLM役

```sh
python3 llm_relay.py next --timeout 600     # 次のリクエストを待って表示する
python3 llm_relay.py reply 0001 <<'JSON'
{"content": null, "tool_calls": [{"name": "add_steps", "arguments": {"descriptions": ["下書き"]}}]}
JSON
```

エージェントに任せるときは、この2つのコマンドだけを使い、アプリのソースを読まないよう指示する
(本物のモデルと同じく、リクエストに含まれる情報だけで答えさせるため)。
