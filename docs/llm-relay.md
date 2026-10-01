# 疑似APIによる往復の確認

プロバイダーのアダプタを足す・変えるときに、APIキーを使わずにターンの往復(ツール実行・保存まで)を
確かめるための手順。モデルの応答はエージェント(または人)が「LLM役」として書く。

疑似API本体は別リポジトリ [Hue-215/Sham_llm](https://github.com/Hue-215/Sham_llm) にある。
受け口・検査の範囲・LLM役の窓口・起動方法はそちらの文書を正とする。この文書はSCITL側の使い方だけを書く。

## 1. 前提

疑似APIが `127.0.0.1:18080` で待ち受けていること。疑似APIは方言ごとにダミーのモデルを1つだけ受ける。

| 方言 | モデル | ベースURL |
|---|---|---|
| OpenAI互換 | `dummy-o` | `http://127.0.0.1:18080/v1` |
| Anthropic形式 | `dummy-a` | `http://127.0.0.1:18080` |
| Gemini形式 | `dummy-g` | `http://127.0.0.1:18080` |

鍵はどの方言も、空でない適当な値を入れる。

## 2. ドライバーで動かす

`crates/scitl-core/examples/relay_session.rs` は、GUIと同じ入口(`create_task`・`open_task_chat`・
`run_turn`)を台本どおりに呼ぶ。応答生成は `scitl-debug-cli` でもできるが(6節)、ドライバーは
設定ファイルと資格情報ストアを使わずに、アダプタとターンの文脈を直に組み立てる。Secret Serviceの
無い環境でも動き、設定からは起こしにくい場面(実体の無い外部ツールの定義を出し入れする、
コンテキスト長を狭める)を作れる。

リポジトリの最上位で:

```sh
cargo run -p scitl-core --example relay_session -- \
  target/relay-data http://127.0.0.1:18080/v1 \
  @new "来週の金曜までに企画書を出したい" "下書きは今日終わったよ"
```

方言は環境変数 `RELAY_DIALECT`(`openai`(既定)・`anthropic`・`gemini`)で選ぶ。BASE_URLは1節の表の
とおりで、Anthropic形式とGemini形式は `/v1` を付けない。この2つは思考の強さ「中」で呼ぶ。

STEPは `@new`(タスクを作って聞き取りを始める)・`@task <id>`(既にあるタスクの会話へ移る)・
`@general`(総合チャットへ移る)・
`@base <文>`(以降のターンの基本のシステムプロンプトを差し替える。変更の通知を確かめるため)・
`@tools on`/`@tools off`(以降のターンで外部ツールを1つ有効・無効にする。ツール定義が変わる場面を
確かめるため。サーバーの実体は無く、呼ばれたら失敗を返す)・
それ以外(今の会話へのユーザー発言)。途中経過(本文・思考・ツールの実行)は標準出力に出る。DATA_DIRは
そのまま `scitl-cli --data-dir target/relay-data task show 1` 等で読める。

`@base`と`@tools`は起動の間だけ効く。同じDATA_DIRで起動し直して会話を続けるときは、毎回渡す
(渡さないと既定に戻り、それも変更として扱われる)。

システムプロンプトは既定のもの、外部ツール(MCP)は`@tools on`にしない限り無し、モデルの能力は既定値(思考あり、
画像なし)。能力の自動検出はしない。既定のコンテキスト長は小さく(4096)、システムプロンプトの変更の
通知が1つ載るだけで間引きが起きるので、間引き以外を確かめるときは環境変数 `RELAY_CONTEXT_LENGTH`
で広げる(逆に間引きを確かめるときは、狭めて長い発言を重ねる)。

Linuxでは `scitl-core` のビルドに `libdbus-1-dev`(`pkg-config` が `dbus-1` を見つけられること)が要る。

## 3. GUIから使う

GUIは1つしか起動しない。普段使いのGUIが動いていると、データディレクトリを変えて起動しても
すぐに終わる(`spec/rebuild/architecture.md`「多重起動の防止」)ので、先に閉じておく。

設定の「APIプロバイダー」で、1節の表の方言・URL・モデルで登録する。平文の `http` はIPリテラルの
ループバックに限って通るので、`localhost` ではなく `127.0.0.1` と書く。

設定「一般」の応答のタイムアウトは既定で120秒。LLM役が考え込むと超えるので、延ばしておく。

能力の自動検出には疑似APIが答える(答える値はSham_llmの文書を参照)。検出結果はアプリの起動中、
モデルごとに覚えられる。

## 4. 限界

LLM役は本物のモデルではない。言い回しやツールの選び方は本物と違いうるので、プロンプトの評価には
使わない。アダプタが固まったら本物のAPIで一度確かめる。

## 5. 本物のサーバーを相手にする(プロンプトキャッシュの確認)

ドライバーは本物のOpenAI互換サーバーにも向けられる。モデル名を環境変数 `RELAY_MODEL` で渡す。
リクエストの組み立てを変えたあと、前に送った部分が変わっていないか(`spec/rebuild/architecture.md`
3節「前に送った部分を変えない」)を、llama.cpp(llama-server)のプロンプトキャッシュの当たり方で
確かめるのに使う。

```sh
RELAY_MODEL=<サーバー側のモデル名> RELAY_CONTEXT_LENGTH=16384 \
  cargo run -p scitl-core --example relay_session -- \
  target/relay-data http://127.0.0.1:<ポート>/v1 "1つ目の発言"
```

1ターンごとに起動し直すと、その間のサーバーのログがそのターンのリクエストに対応する(総合チャットは
そのまま続き、タスクの会話は `@task <id>` で続ける)。llama-serverはリクエストごとに次を出す。

- `prompt eval time = … / N tokens`: 計算し直したプロンプトのトークン数
- `eval time = … / G tokens`: 生成したトークン数
- `stop processing: n_tokens = T`: 終了時にスロットにあるトークン数

プロンプト全体は `T - G`、キャッシュから使えた分は `T - G - N`。前のリクエストのプロンプト全体と
同じかそれ以上なら、前のリクエストの末尾まで当たっている。前の応答が思考を含まなければ、応答の分まで
当たる(生成したトークン列と、送り返した発言のトークン列が一致するため)。思考を含むと、OpenAI互換では
思考を送り返さないので、応答の分は計算し直しになる。

`RELAY_CONTEXT_LENGTH` はサーバーのコンテキスト長(llama-serverなら `GET /props` の
`default_generation_settings.n_ctx`)以下にする。

## 6. scitl-debug-cliで動かす

GUIと同じ設定の読み方(設定ファイル・資格情報ストア・能力の自動検出)まで含めて確かめるときは、
`scitl-debug-cli` で登録して送る。鍵は引数ではなく環境変数の名前で渡す。資格情報ストア(Linuxでは
Secret Service)が要る。

```sh
D=target/debug-data; mkdir -p $D
cli() { cargo run -q -p scitl-debug-cli -- --data-dir $D "$@"; }

RELAY_KEY=dummy cli provider add --name sham --api-format open_ai_compat \
  --base-url http://127.0.0.1:18080/v1 --api-key-env RELAY_KEY   # 出力の providers[].id を控える
cli model add <プロバイダーのID> dummy-o
cli model select <プロバイダーのID> dummy-o
cli settings general --response-timeout-secs 900                 # LLM役は遅いので延ばす
cli task create                                                   # タスクを作り、聞き取りを始める
cli chat send --task 1 --attach memo.txt "これを見て"
```

途中経過は1行に1つのJSONで標準出力に出て、最後の行が会話の最後の発言になる。失敗したターンは
エラー発言として保存され、その行に `error_kind` が入る。確かめ終えたら `cli provider delete` で
鍵ごと消す。
