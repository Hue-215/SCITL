#!/usr/bin/env python3
"""LLM役が使う窓口。

  llm_relay.py next [--timeout SECS]   次の未回答リクエストを待って表示する
  llm_relay.py reply ID < response.json 応答を書く(形は mock_llm.py の冒頭を参照)

ドライバーが終わると box/done ができ、next は DONE と表示して終わる。
システムプロンプトとツール定義は、前回表示から変わっていなければ省略する。
"""
import hashlib
import json
import os
import sys
import time

RELAY = os.environ.get("RELAY_DIR", os.path.join(os.path.dirname(__file__), "box"))
SEEN = os.path.join(RELAY, ".last_shown_hash")


def pending():
    reqs = sorted(f[4:8] for f in os.listdir(RELAY) if f.startswith("req-") and f.endswith(".json"))
    return [r for r in reqs if not os.path.exists(os.path.join(RELAY, f"res-{r}.json"))]


def normalize_anthropic(body):
    """Anthropic形式を、下の表示が読むOpenAI風の形に直す(表示のためだけ)。"""
    system = body.get("system")
    if isinstance(system, list):
        system = "\n".join(b["text"] for b in system)
    msgs = [{"role": "system", "content": system}] if system else []
    for m in body["messages"]:
        blocks = m["content"] if isinstance(m["content"], list) else [{"type": "text", "text": m["content"]}]
        texts, calls = [], []
        for b in blocks:
            t = b.get("type")
            if t == "text":
                texts.append(b["text"])
            elif t == "image":
                texts.append("<image>")
            elif t == "thinking":
                texts.append(f"<thinking>{b.get('thinking', '')}</thinking>")
            elif t == "tool_use":
                calls.append({"function": {"name": b["name"], "arguments": json.dumps(b["input"], ensure_ascii=False)}})
            elif t == "tool_result":
                c = b.get("content")
                if isinstance(c, list):
                    c = "\n".join(x.get("text", f"<{x['type']}>") for x in c)
                err = " is_error" if b.get("is_error") else ""
                texts.append(f"<tool_result id={b['tool_use_id']}{err}>{c or ''}</tool_result>")
        msgs.append({"role": m["role"], "content": "\n".join(texts), "tool_calls": calls})
    tools = [{"name": t["name"], "description": t.get("description", ""),
              "parameters": t.get("input_schema")} for t in body.get("tools", [])]
    return msgs, tools


def show(rid):
    with open(os.path.join(RELAY, f"req-{rid}.json")) as f:
        body = json.load(f)
    if body.get("_format") == "anthropic":
        msgs, tools = normalize_anthropic(body)
    else:
        msgs = body["messages"]
        tools = [{"name": t["function"]["name"], "description": t["function"]["description"],
                  "parameters": t["function"]["parameters"]} for t in body.get("tools", [])]
    system = [m for m in msgs if m["role"] == "system"]
    fixed = json.dumps([system, tools], ensure_ascii=False, sort_keys=True)
    h = hashlib.sha256(fixed.encode()).hexdigest()
    last = open(SEEN).read() if os.path.exists(SEEN) else ""
    print(f"=== REQUEST {rid} ({body.get('_format', 'openai')}, model={body.get('model')}) ===")
    if h != last:
        print("--- system prompt ---")
        for m in system:
            print(m["content"])
        print("--- tools ---")
        for t in tools:
            print(json.dumps(t, ensure_ascii=False))
        with open(SEEN, "w") as f:
            f.write(h)
    else:
        print("(system prompt and tools unchanged since last shown)")
    print("--- messages ---")
    for m in msgs:
        if m["role"] == "system":
            continue
        print(f"[{m['role']}]" + (f" tool_call_id={m['tool_call_id']}" if m.get("tool_call_id") else ""))
        c = m.get("content")
        if isinstance(c, list):
            c = "\n".join(p.get("text", f"<{p['type']}>") for p in c)
        if c:
            print(c)
        for tc in m.get("tool_calls", []):
            print(f"  -> tool_call {tc['function']['name']} {tc['function']['arguments']}")
    print(f"=== reply with: llm_relay.py reply {rid} ===")


def main():
    cmd = sys.argv[1]
    if cmd == "next":
        timeout = float(sys.argv[sys.argv.index("--timeout") + 1]) if "--timeout" in sys.argv else 300
        deadline = time.time() + timeout
        while True:
            p = pending()
            if p:
                return show(p[0])
            if os.path.exists(os.path.join(RELAY, "done")):
                print("DONE")
                return
            if time.time() > deadline:
                print("TIMEOUT (no request yet; run next again)")
                return
            time.sleep(0.5)
    elif cmd == "reply":
        rid = sys.argv[2]
        res = json.loads(sys.stdin.read())
        path = os.path.join(RELAY, f"res-{rid}.json")
        with open(path + ".tmp", "w") as f:
            json.dump(res, f, ensure_ascii=False, indent=2)
        os.rename(path + ".tmp", path)
        print(f"replied {rid}")


if __name__ == "__main__":
    main()
