#!/usr/bin/env python3
"""OpenAI互換の疑似APIサーバー。受けたリクエストを box/req-NNNN.json に書き、
LLM役(サブエージェント)が box/res-NNNN.json を書くまで待って、それを応答として返す。

応答ファイルの形(OpenAIの message を簡略化したもの):
  {"content": "本文" | null,
   "reasoning_content": "思考(任意)",
   "tool_calls": [{"name": "add_steps", "arguments": {...}}],
   "stop_reason": "max_tokens"(任意。打ち切り等を試すとき)}
"""
import json
import os
import re
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


def write_new(prefix, data):
    """box/<prefix>-NNNN.json を書いて NNNN を返す。番号は既存の最大+1 で、決めてから書き終えるまで
    ロックを持つ。件数から決めると、消したファイルの番号を使い回して残った応答と取り違える。
    res も見るのは、req を手で消しても残った応答と番号が重ならないようにするため。"""
    with lock:
        taken = [int(m.group(1)) for f in os.listdir(RELAY)
                 if (m := re.match(rf"(?:{prefix}|res)-(\d+)\.", f))]
        rid = f"{max(taken, default=0) + 1:04d}"
        path = os.path.join(RELAY, f"{prefix}-{rid}.json")
        with open(path + ".tmp", "w") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
        os.rename(path + ".tmp", path)
    return rid


def expire(rid):
    """答えずに終わった req を、llm_relay.py next が拾わない名前にする。"""
    path = os.path.join(RELAY, f"req-{rid}.json")
    try:
        os.rename(path, os.path.join(RELAY, f"req-{rid}.expired.json"))
    except FileNotFoundError:  # 手で消された等。拾わせないという目的は果たせている
        pass


def reset_box():
    """前回の実行の名残を片付ける。起動し直した時点で、前のリクエストを待っている相手はいない。
    done は次の next を即座に終わらせ、.last_shown_hash は新しいLLM役にシステムプロンプトと
    ツール定義を見せなくする。"""
    for name in ("done", ".last_shown_hash"):
        if os.path.exists(os.path.join(RELAY, name)):
            os.remove(os.path.join(RELAY, name))
    for f in os.listdir(RELAY):
        m = re.fullmatch(r"req-(\d+)\.json", f)
        if m and not os.path.exists(os.path.join(RELAY, f"res-{m.group(1)}.json")):
            expire(m.group(1))


def relay(body):
    """リクエストをLLM役へ渡し、応答ファイルを待つ。時間切れなら None。"""
    rid = write_new("req", body)
    res_path = os.path.join(RELAY, f"res-{rid}.json")
    deadline = time.time() + WAIT_SECS
    while not os.path.exists(res_path):
        if time.time() > deadline:
            expire(rid)
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

    def from_local_client(self):
        """ブラウザで開いたページからの投げ込みを断る。本文はシェルを持つLLM役にそのまま見せるため。
        ブラウザは別オリジンへのPOSTに Origin を付け、DNSリバインディングでは Host が別の名前になる。"""
        if self.headers.get("Origin") is None and \
                self.headers.get("Host") in (f"127.0.0.1:{PORT}", f"localhost:{PORT}"):
            return True
        sys.stderr.write(f"[mock] FORBIDDEN Host={self.headers.get('Host')} Origin={self.headers.get('Origin')}\n")
        self.send_json(403, {"error": {"message": "[mock wording] requests from browsers are not accepted"}})
        return False

    def do_GET(self):
        if not self.from_local_client():
            return
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
        if not self.from_local_client():
            return
        path = self.path.rstrip("/")
        if path.endswith("/messages"):
            handler = self.anthropic_messages
        elif path.endswith("/chat/completions"):
            handler = self.chat_completions
        else:
            return self.send_json(404, {"error": {"message": "not found"}})
        try:
            handler()
        except Exception as e:
            # LLM役の応答の書き損じ等。例外のまま切断すると、アダプタ側では原因の分からない
            # 通信エラーになる。OpenAI互換・Anthropic形式のどちらの読み方でも文面が取れる形で返す
            sys.stderr.write(f"[mock] ERROR {e!r}\n")
            self.send_json(500, {"type": "error",
                                 "error": {"type": "api_error", "message": f"[mock] {e!r}"}})

    def chat_completions(self):
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

    def anthropic_messages(self):
        """Anthropic形式。先に anthropic_rules の検査を通し、断るものはLLM役へ渡さない。"""
        request_id = rules.new_request_id()
        raw = self.rfile.read(int(self.headers.get("Content-Length") or 0))
        lint = []
        try:
            rules.check_headers(self.headers)
            body, thinking_on = rules.check_body(raw, lint)
            rules.check_thinking_round_trip(body["messages"])
        except rules.ApiError as err:
            write_new("rejected", {"status": err.status, "error": err.message, "lint": lint,
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
    # 片付けは待ち受けに成功してから。二重に起動したとき、後の方がポートを取れずに終わる前に
    # 動いている方のリクエストを片付けてしまわないように
    server = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    reset_box()
    print(f"[mock] listening on 127.0.0.1:{PORT}, relay dir {RELAY}", flush=True)
    server.serve_forever()
