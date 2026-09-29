#!/usr/bin/env python3
"""OpenAI互換の疑似APIサーバー。受けたリクエストを relay/req-NNNN.json に書き、
LLM役(サブエージェント)が relay/res-NNNN.json を書くまで待って、それを応答として返す。

応答ファイルの形(OpenAIの message を簡略化したもの):
  {"content": "本文" | null,
   "reasoning_content": "思考(任意)",
   "tool_calls": [{"name": "add_steps", "arguments": {...}}],
   "stop_reason": "max_tokens"(任意。打ち切り等を試すとき)}
"""
import json
import os
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import anthropic_rules as rules  # noqa: E402

RELAY = os.environ.get("RELAY_DIR", os.path.join(os.path.dirname(__file__), "box"))
PORT = int(os.environ.get("RELAY_PORT", "18080"))
WAIT_SECS = 900
# 能力の自動検出に答える値。SCITLはループバックの接続先に llama.cpp の GET /props 等を問い合わせる。
VISION = os.environ.get("RELAY_VISION", "1") == "1"
TOOLS = os.environ.get("RELAY_TOOLS", "1") == "1"
N_CTX = int(os.environ.get("RELAY_N_CTX", "32768"))
# LLM役は方言に関係なく Anthropic の stop_reason の名前で書く。OpenAI互換ではこの名前にする。
FINISH_REASONS = {"end_turn": "stop", "tool_use": "tool_calls", "max_tokens": "length"}
os.makedirs(RELAY, exist_ok=True)
lock = threading.Lock()


def next_id(prefix="req-"):
    with lock:
        n = len([f for f in os.listdir(RELAY) if f.startswith(prefix)]) + 1
        return f"{n:04d}"


def record(prefix, data):
    """断ったリクエスト等を box/<prefix>-NNNN.json に残す(LLM役には渡さない)。"""
    rid = next_id(prefix + "-")
    with open(os.path.join(RELAY, f"{prefix}-{rid}.json"), "w") as f:
        json.dump(data, f, ensure_ascii=False, indent=2)


def relay(body):
    """リクエストをLLM役へ渡し、応答ファイルを待つ。時間切れなら None。"""
    rid = next_id()
    req_path = os.path.join(RELAY, f"req-{rid}.json")
    res_path = os.path.join(RELAY, f"res-{rid}.json")
    with open(req_path + ".tmp", "w") as f:
        json.dump(body, f, ensure_ascii=False, indent=2)
    os.rename(req_path + ".tmp", req_path)
    deadline = time.time() + WAIT_SECS
    while not os.path.exists(res_path):
        if time.time() > deadline:
            return None
        time.sleep(0.3)
    time.sleep(0.1)
    with open(res_path) as f:
        return json.load(f)


def completion(res):
    calls = [
        {
            "id": c.get("id") or "call_" + uuid.uuid4().hex[:8],
            "type": "function",
            "function": {
                "name": c["name"],
                "arguments": c["arguments"] if isinstance(c["arguments"], str)
                else json.dumps(c["arguments"], ensure_ascii=False),
            },
        }
        for c in res.get("tool_calls") or []
    ]
    message = {"role": "assistant", "content": res.get("content")}
    if calls:
        message["tool_calls"] = calls
    if res.get("reasoning_content"):
        message["reasoning_content"] = res["reasoning_content"]
    stop = res.get("stop_reason")
    finish = FINISH_REASONS.get(stop, stop) if stop else ("tool_calls" if calls else "stop")
    return {
        "id": "chatcmpl-" + uuid.uuid4().hex[:12],
        "object": "chat.completion",
        "created": int(time.time()),
        "model": "relay",
        "choices": [{"index": 0, "message": message,
                     "finish_reason": finish}],
    }


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("[mock] " + fmt % args + "\n")

    def send_json(self, status, body, headers=None):
        data = json.dumps(body, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        for k, v in (headers or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        path = self.path.rstrip("/")
        if path.endswith("/models"):
            return self.send_json(200, {"object": "list", "data": [{"id": "relay-model"}]})
        if path.endswith("/props"):  # llama.cpp の形。1モデルだけを載せたサーバーとして答える
            return self.send_json(200, {
                "default_generation_settings": {"n_ctx": N_CTX},
                "modalities": {"vision": VISION},
                "chat_template_caps": {"supports_tool_calls": TOOLS},
            })
        self.send_json(404, {"error": {"message": "not found"}})

    def do_POST(self):
        path = self.path.rstrip("/")
        if path.endswith("/messages"):
            return self.anthropic_messages()
        if not path.endswith("/chat/completions"):
            return self.send_json(404, {"error": {"message": "not found"}})
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        body["_auth_header_present"] = bool(self.headers.get("Authorization"))
        res = relay(body)
        if res is None:
            return self.send_json(504, {"error": {"message": "relay timed out"}})
        if "error" in res:
            # エラー応答を試すとき: {"error": {"status": 400, "message": ..., "code": ..., "param": ...}}
            # status 以外はOpenAIの error の形のまま返す
            err = dict(res["error"])
            return self.send_json(err.pop("status", 500), {"error": err})
        self.send_json(200, completion(res))

    def anthropic_messages(self, extra_headers=None):
        """Anthropic形式。先に anthropic_rules の検査を通し、断るものはLLM役へ渡さない。"""
        request_id = rules.new_request_id()
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        lint = []
        try:
            rules.check_headers(self.headers)
            body, thinking_on = rules.check_body(raw, lint)
            rules.check_thinking_round_trip(body["messages"])
        except rules.ApiError as err:
            record("rejected", {"status": err.status, "error": err.message, "lint": lint,
                                "body": raw.decode(errors="replace")})
            sys.stderr.write(f"[mock] REJECTED {err.status} {err.kind}: {err.message}\n")
            return self.send_json(err.status, rules.error_body(err, request_id),
                                  {"request-id": request_id})
        for w in lint:
            sys.stderr.write(f"[mock] lint: {w}\n")
        body["_format"] = "anthropic"
        body["_lint"] = lint
        res = relay(body)
        if res is None:
            err = rules.ApiError(504, "timeout_error", "relay timed out")
            return self.send_json(504, rules.error_body(err, request_id), {"request-id": request_id})
        if "error" in res:
            # LLM役が本物の形のエラーを返すとき: {"error": {"status": 529, "type": "overloaded_error", "message": ...}}
            e = res["error"]
            err = rules.ApiError(e.get("status", 500), e.get("type", "api_error"), e.get("message", ""))
            return self.send_json(err.status, rules.error_body(err, request_id), {"request-id": request_id})
        msg = rules.build_message(res, body["model"], thinking_on, len(raw))
        self.send_json(200, msg, {"request-id": request_id})


if __name__ == "__main__":
    print(f"[mock] listening on 127.0.0.1:{PORT}, relay dir {RELAY}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
