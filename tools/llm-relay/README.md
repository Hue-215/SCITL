# llm-relay

モデルの代わりにエージェント(または人)が応答を書く疑似APIと、それを相手に本物のアダプタと
ターンの処理を通すための道具。プロバイダーのアダプタを足すとき・変えるときに、APIキーを
使わずに往復を確かめる。

```
relay_session ──HTTP──▶ mock_llm.py ──box/req-NNNN.json──▶ LLM役
(本物のアダプタ)        (疑似API)  ◀──box/res-NNNN.json──  llm_relay.py next / reply
```

- `mock_llm.py` — `127.0.0.1:18080` で受ける。`/v1/chat/completions`(OpenAI互換)と
  `/v1/messages`(Anthropic形式)
- `anthropic_rules.py` — `/v1/messages` のリクエストの検査と応答の組み立て。検査に通らない
  リクエストは本物と同じ形のエラーで断り、LLM役へは渡さない(`box/rejected-NNNN.json` に残す)
- `llm_relay.py` — LLM役の窓口。`next` で次のリクエストを表示し、`reply ID` で応答を書く
- `crates/scitl-core/examples/relay_session.rs` — GUIと同じ入口(`create_task`・
  `open_task_chat`・`run_turn`)を台本どおりに呼ぶドライバー

## 検査の範囲

`anthropic_rules.py` がエラーを返すのは公式ドキュメントに書かれている規則だけで、規則ごとに
出典を書いてある。文書で確かめられなかったものは警告(`box/req-NNNN.json` の `_lint` と
`mock.log`)に留める。エラーの文言は、文書に原文があるものは原文、無いものは
`[mock wording]` を付けた独自の文言。

検査は文書の読み取りなので、本物との差は残る。アダプタが固まったら、本物のAPIで一度確かめる。
OpenAI互換の側は検査しない(互換を名乗るサーバーごとに挙動が違い、基準にする文書が無い)。

## 使い方

```sh
cd tools/llm-relay
python3 -m unittest test_anthropic_rules   # 検査の自己テスト
python3 mock_llm.py > mock.log 2>&1 &

# 別の端末で。STEPは @new(タスクを作って聞き取りを始める)・@general・それ以外(ユーザー発言)
cargo run -p scitl-core --example relay_session -- \
  tools/llm-relay/data http://127.0.0.1:18080/v1 @new "来週の金曜までに企画書を出したい"
```

LLM役は `python3 llm_relay.py next` で読み、次の形で答える。ドライバーが終わったら
`box/done` を作ると、`next` は `DONE` を返す。

```sh
python3 llm_relay.py reply 0001 <<'JSON'
{"content": "本文(ツールだけ呼ぶなら null)",
 "tool_calls": [{"name": "add_steps", "arguments": {"descriptions": ["下書き"]}}]}
JSON
```

エラー応答を試すときは `{"error": {"status": 429, "type": "rate_limit_error", "message": "..."}}`
を書く。結果は `scitl-cli --data-dir tools/llm-relay/data task show 1` 等で確かめる。
