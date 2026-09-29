# llm-relay

モデルの代わりにエージェント(または人)が応答を書く疑似APIと、その窓口。仕様と使い方は
[`docs/llm-relay.md`](../../docs/llm-relay.md)。

```sh
python3 -m unittest test_anthropic_rules   # 検査の自己テスト
python3 mock_llm.py > mock.log 2>&1 &      # 127.0.0.1:18080
python3 llm_relay.py next                  # LLM役: 次のリクエストを表示
```
