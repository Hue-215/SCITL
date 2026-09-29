"""疑似APIの Anthropic Messages API (`POST /v1/messages`) の検査と応答の組み立て。

方針: 400等を返すのは、公式ドキュメントに書かれている規則だけ。文書で確かめられなかったもの・
SCITL側の方針として見たいものは警告(lint)に留め、リクエストは通す。規則ごとに出典を
[出典キー] で書く。エラーの文言は、文書が原文を示しているものは原文どおり、示していない
ものは "[mock wording]" を付けて疑似API独自の文言であることを示す。

出典 (2026-09-29 に取得):
  [overview]  https://platform.claude.com/docs/en/api/overview       必須ヘッダー、413、応答ヘッダー
  [version]   https://platform.claude.com/docs/en/api/versioning     anthropic-version
  [errors]    https://platform.claude.com/docs/en/api/errors         エラーの形、種類、モデル別の400
  [messages]  https://platform.claude.com/docs/en/api/messages       本文の必須項目、各ブロックの形
  [tools]     https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls
  [vision]    https://platform.claude.com/docs/en/build-with-claude/vision
  [skill]     claude-api スキル同梱のモデル表(2026-09-25時点のキャッシュ)。公式ページで直接
              確かめていないので、ここから来る判定は警告かモデルの一覧にだけ使う
"""
import json
import re
import uuid

# --- モデル表 --------------------------------------------------------------
# id -> (世代(major, minor), 省略時に思考が有効か)  [skill] の表から。
# 一覧に無いIDは 404 (`model: <id>` で始まる not_found_error) [errors]。
MODELS = {
    "claude-fable-5-1": ((5, 1), True),
    "claude-mythos-5-1": ((5, 1), True),
    "claude-fable-5": ((5, 0), True),
    "claude-opus-5-5": ((5, 5), True),
    "claude-opus-5": ((5, 0), True),
    "claude-opus-4-8": ((4, 8), False),
    "claude-opus-4-7": ((4, 7), False),
    "claude-opus-4-6": ((4, 6), False),
    "claude-sonnet-5-5": ((5, 5), True),
    "claude-sonnet-5": ((5, 0), True),
    "claude-sonnet-4-6": ((4, 6), False),
    "claude-haiku-4-5": ((4, 5), False),
}

# 思考を無効にできないモデル [errors] "Thinking cannot be disabled"
ALWAYS_THINKING = {"claude-fable-5-1", "claude-mythos-5-1", "claude-fable-5", "claude-opus-5-5"}
# 強制ツール使用を受け付けないモデル [errors] "Forced tool use not supported"
NO_FORCED_TOOL_CHOICE = {"claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1", "claude-mythos-5-1"}

ID_PATTERN = re.compile(r"^[a-zA-Z0-9_-]+$")          # tool_use.id / tool_result.tool_use_id [messages]
TOOL_NAME_PATTERN = re.compile(r"^[a-zA-Z0-9_-]{1,128}$")  # tools[].name [messages]
IMAGE_MEDIA_TYPES = {"image/jpeg", "image/png", "image/gif", "image/webp"}  # [vision]
USER_BLOCKS = {"text", "image", "document", "search_result", "tool_reference", "tool_result"}  # [messages]
ASSISTANT_BLOCKS = {"text", "tool_use", "server_tool_use", "thinking", "redacted_thinking"}  # [messages]
TOOL_RESULT_BLOCKS = {"text", "image", "document", "search_result"}  # [tools]
MAX_REQUEST_BYTES = 32 * 1024 * 1024        # [overview] [errors]
MAX_IMAGE_BASE64_BYTES = 10 * 1024 * 1024   # [vision] 直接のClaude API
MAX_MESSAGES = 100_000                      # [messages]


class ApiError(Exception):
    def __init__(self, status, kind, message):
        super().__init__(message)
        self.status, self.kind, self.message = status, kind, message


def bad(message):
    return ApiError(400, "invalid_request_error", message)


def error_body(err, request_id):
    """[errors] "Error shapes": 最上位の `error` に `type` と `message`、それと `request_id`。"""
    return {"type": "error", "error": {"type": err.kind, "message": err.message},
            "request_id": request_id}


def new_request_id():
    return "req_mock" + uuid.uuid4().hex[:20]


# --- 検査 ------------------------------------------------------------------

def check_headers(headers):
    """[overview] Authentication の表。Authorization か x-api-key のどちらか、
    anthropic-version、content-type: application/json が必須。"""
    auth = headers.get("Authorization") or ""
    if not auth.startswith("Bearer ") and not headers.get("x-api-key"):
        # 鍵が無い/壊れている → 401 authentication_error [errors]
        raise ApiError(401, "authentication_error",
                       "[mock wording] missing Authorization: Bearer <key> or x-api-key header")
    if not headers.get("anthropic-version"):
        # 欠けたときの状態コードは文書に無い。400として扱う(要確認)。
        raise bad("[mock wording; status assumed] anthropic-version header is required")
    ctype = (headers.get("content-type") or "").split(";")[0].strip().lower()
    if ctype != "application/json":
        raise bad("[mock wording; status assumed] content-type must be application/json")


def check_body(raw, lint):
    if len(raw) > MAX_REQUEST_BYTES:
        raise ApiError(413, "request_too_large", "[mock wording] request exceeds 32 MB")
    try:
        body = json.loads(raw)
    except ValueError:
        raise bad("[mock wording] request body is not valid JSON")
    if not isinstance(body, dict):
        raise bad("[mock wording] request body must be a JSON object")

    for field in ("model", "max_tokens", "messages"):  # [messages] 必須
        if field not in body:
            raise bad(f"[mock wording] {field}: Field required")
    model = body["model"]
    if model not in MODELS:
        raise ApiError(404, "not_found_error", f"model: {model}")  # [errors] 404 の文言の頭
    mt = body["max_tokens"]
    if not isinstance(mt, int) or isinstance(mt, bool) or mt < 0:  # [messages] minimum 0
        raise bad("[mock wording] max_tokens: must be an integer >= 0")
    if body.get("stream"):
        raise bad("[mock limitation] this mock does not implement streaming")

    for key in ("temperature", "top_p", "top_k"):
        if key in body:
            lint.append(f"{key} is set: [skill] says it returns 400 on Opus 4.7+ / Fable; not verified on the official pages fetched")

    check_system(body.get("system"))
    tool_names = check_tools(body.get("tools"), lint)
    check_tool_choice(body.get("tool_choice"), model, tool_names)
    thinking_on = check_thinking(body, model)
    check_messages(body["messages"], model, lint)
    return body, thinking_on


def check_text_block(block, where):
    # TextBlockParam: text は minLength 1 [messages]
    if not isinstance(block.get("text"), str) or block["text"] == "":
        raise bad(f"[mock wording] {where}.text: text content blocks must be non-empty")


def check_system(system):
    """[messages] system は string か TextBlockParam の配列。"""
    if system is None or isinstance(system, str):
        return
    if not isinstance(system, list):
        raise bad("[mock wording] system: must be a string or an array of text blocks")
    for j, block in enumerate(system):
        if not isinstance(block, dict) or block.get("type") != "text":
            raise bad(f"[mock wording] system.{j}: only text blocks are allowed")
        check_text_block(block, f"system.{j}")


def check_tools(tools, lint):
    if tools is None:
        return set()
    if not isinstance(tools, list):
        raise bad("[mock wording] tools: must be an array")
    names = set()
    for k, tool in enumerate(tools):
        if not isinstance(tool, dict):
            raise bad(f"[mock wording] tools.{k}: must be an object")
        if tool.get("type") not in (None, "custom"):
            lint.append(f"tools.{k}: server/Anthropic-defined tool type {tool.get('type')!r} is not checked by the mock")
            continue
        name = tool.get("name")
        if not isinstance(name, str) or not TOOL_NAME_PATTERN.match(name):
            raise bad(f"[mock wording] tools.{k}.name: String should match pattern '^[a-zA-Z0-9_-]{{1,128}}$'")
        schema = tool.get("input_schema")
        if not isinstance(schema, dict) or schema.get("type") != "object":
            raise bad(f"[mock wording] tools.{k}.input_schema: must be a JSON schema with type \"object\"")
        if name in names:
            lint.append(f"tools.{k}: duplicate tool name {name!r} (behavior not documented)")
        names.add(name)
    return names


def check_tool_choice(choice, model, tool_names):
    if choice is None:
        return
    kind = choice.get("type") if isinstance(choice, dict) else None
    if kind not in ("auto", "any", "tool", "none"):  # [messages]
        raise bad("[mock wording] tool_choice.type: must be one of auto, any, tool, none")
    if kind in ("any", "tool") and model in NO_FORCED_TOOL_CHOICE:
        raise bad('tool_choice: type "tool" and "any" are not supported for this model.')  # [errors] 原文
    if kind == "tool" and not choice.get("name"):
        raise bad("[mock wording] tool_choice.name: required when type is \"tool\"")


def check_thinking(body, model):
    """[errors] の思考設定の400(文言は原文)。戻り値は、応答に思考ブロックを付けるか。"""
    gen, default_on = MODELS[model]
    thinking = body.get("thinking")
    effort = (body.get("output_config") or {}).get("effort")
    if thinking is None:
        return default_on
    kind = thinking.get("type") if isinstance(thinking, dict) else None
    if kind == "enabled" and gen >= (4, 7):
        raise bad('"thinking.type.enabled" is not supported for this model. Use "thinking.type.adaptive" and "output_config.effort" to control thinking behavior.')
    if kind == "adaptive" and gen <= (4, 5):
        raise bad("adaptive thinking is not supported on this model")
    if kind == "disabled":
        if model in ALWAYS_THINKING:
            raise bad('"thinking.type.disabled" is not supported for this model. Use "thinking.type.adaptive" and "output_config.effort" to control thinking behavior.')
        if model == "claude-sonnet-5-5":
            raise bad('"thinking.type.disabled" is not supported for this model. Use "thinking.type.between_tools" for the lowest thinking setting, or "thinking.type.adaptive" and "output_config.effort" to control thinking behavior.')
        return False
    if kind == "between_tools":
        if model != "claude-sonnet-5-5":
            raise bad('"thinking.type.between_tools" is not supported for this model.')
        if effort in ("xhigh", "max"):
            raise bad(f"output_config.effort '{effort}' is not supported when thinking is disabled on this model. Use effort 'high' or below, or enable thinking.")
        return False
    if isinstance(thinking, dict) and "block_binding" in thinking:
        raise bad("block_binding: Extra inputs are not permitted")  # ベータヘッダー無しの場合 [errors]
    return kind in ("adaptive", "enabled")


def blocks_of(message):
    """content の文字列は text ブロック1つの略記 [messages]。"""
    content = message.get("content")
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    return content


def check_messages(messages, model, lint):
    if not isinstance(messages, list):
        raise bad("[mock wording] messages: must be an array")
    if len(messages) > MAX_MESSAGES:
        raise bad("[mock wording] messages: at most 100,000 messages")
    if not messages:
        lint.append("messages is empty (behavior not documented on the pages fetched)")
        return

    image_count = 0
    for i, m in enumerate(messages):
        if not isinstance(m, dict):
            raise bad(f"[mock wording] messages.{i}: must be an object")
        role = m.get("role")
        if role == "system":
            # 公式ページ同士で食い違う: リファレンス本文は「system ロールは無い」、エラーページ等は
            # 会話途中の system メッセージを載せている。SCITLは使わない想定なので警告に留める。
            lint.append(f"messages.{i}: role 'system' in messages (official pages disagree; not rejected)")
            continue
        if role not in ("user", "assistant"):
            raise bad(f"[mock wording] messages.{i}.role: must be 'user' or 'assistant'")
        blocks = blocks_of(m)
        if not isinstance(blocks, list):
            raise bad(f"[mock wording] messages.{i}.content: must be a string or an array of content blocks")
        allowed = USER_BLOCKS if role == "user" else ASSISTANT_BLOCKS
        for j, b in enumerate(blocks):
            where = f"messages.{i}.content.{j}"
            kind = b.get("type") if isinstance(b, dict) else None
            if kind not in allowed:
                raise bad(f"[mock wording] {where}: block type {kind!r} is not allowed in a {role} message")
            if kind == "text":
                check_text_block(b, where)
            elif kind == "image":
                image_count += 1
                check_image(b, where)
            elif kind == "tool_use":
                if not isinstance(b.get("id"), str) or not ID_PATTERN.match(b["id"]):
                    raise bad(f"[mock wording] {where}.id: String should match pattern '^[a-zA-Z0-9_-]+$'")
                name = b.get("name")
                if not isinstance(name, str) or not 1 <= len(name) <= 200:
                    raise bad(f"[mock wording] {where}.name: length must be 1..200")
                if not isinstance(b.get("input"), dict):
                    raise bad(f"[mock wording] {where}.input: must be an object")
            elif kind == "tool_result":
                check_tool_result(b, where)
                for k, inner in enumerate(b.get("content") if isinstance(b.get("content"), list) else []):
                    if inner.get("type") == "image":
                        image_count += 1
                        check_image(inner, f"{where}.content.{k}")

    # [vision] 1リクエストの画像数の上限(200kコンテキストのモデルは100、それ以外は600)
    limit = 100 if model == "claude-haiku-4-5" else 600
    if image_count > limit:
        raise bad(f"[mock wording] too many images in one request: {image_count} > {limit}")
    if image_count > 20:
        lint.append(f"{image_count} images: the stricter per-image dimension limit for many-image requests applies [vision] (not checked by the mock)")

    check_turns(messages, model, lint)


def check_image(block, where):
    """[vision] base64 / url / file の3種。base64 は形式4種と 10MB (base64後) まで。"""
    src = block.get("source")
    kind = src.get("type") if isinstance(src, dict) else None
    if kind == "base64":
        if src.get("media_type") not in IMAGE_MEDIA_TYPES:
            raise bad(f"[mock wording] {where}.source.media_type: must be one of image/jpeg, image/png, image/gif, image/webp")
        data = src.get("data")
        if not isinstance(data, str) or not data:
            raise bad(f"[mock wording] {where}.source.data: base64 string required")
        if data.startswith("data:"):
            raise bad(f"[mock wording] {where}.source.data: send raw base64, not a data: URL")
        if len(data) > MAX_IMAGE_BASE64_BYTES:
            raise bad(f"[mock wording] {where}: image exceeds 10 MB (base64-encoded)")
    elif kind == "url":
        if not isinstance(src.get("url"), str):
            raise bad(f"[mock wording] {where}.source.url: string required")
    elif kind == "file":
        if not isinstance(src.get("file_id"), str):
            raise bad(f"[mock wording] {where}.source.file_id: string required")
    else:
        raise bad(f"[mock wording] {where}.source.type: must be base64, url or file")


def check_tool_result(block, where):
    """[messages] tool_use_id の形式、[tools] content は string か text/image/document/search_result の配列。"""
    tid = block.get("tool_use_id")
    if not isinstance(tid, str) or not ID_PATTERN.match(tid):
        raise bad(f"[mock wording] {where}.tool_use_id: String should match pattern '^[a-zA-Z0-9_-]+$'")
    content = block.get("content")
    if content is not None and not isinstance(content, str):
        if not isinstance(content, list):
            raise bad(f"[mock wording] {where}.content: must be a string or an array of blocks")
        for k, inner in enumerate(content):
            if not isinstance(inner, dict) or inner.get("type") not in TOOL_RESULT_BLOCKS:
                raise bad(f"[mock wording] {where}.content.{k}: only text, image, document, search_result blocks are allowed")
            if inner.get("type") == "text":
                check_text_block(inner, f"{where}.content.{k}")
    if "is_error" in block and not isinstance(block["is_error"], bool):
        raise bad(f"[mock wording] {where}.is_error: must be a boolean")


def merged_turns(messages):
    """[messages] 続く同じロールの発言は1ターンにまとめられる。まとめた後のターンと、
    そのターンが始まる元の添字を返す。system ロールは除く。"""
    turns = []
    for i, m in enumerate(messages):
        if m.get("role") == "system":
            continue
        blocks = blocks_of(m)
        if turns and turns[-1]["role"] == m["role"]:
            turns[-1]["blocks"].extend(blocks)
            turns[-1]["count"] += 1
        else:
            turns.append({"role": m["role"], "index": i, "blocks": list(blocks), "count": 1})
    return turns


def check_turns(messages, model, lint):
    turns = merged_turns(messages)
    for t in turns:
        if t["count"] > 1:
            lint.append(f"messages.{t['index']}: {t['count']} consecutive {t['role']} messages (the API combines them into one turn [messages])")
    if turns and turns[0]["role"] != "user":
        lint.append("the first message is not a user message (not stated on the official pages fetched)")

    # 最後がアシスタント発言(プリフィル)は 4.6 以降で 400 [errors] 原文
    gen, _ = MODELS[model]
    if turns and turns[-1]["role"] == "assistant" and gen >= (4, 6):
        raise bad("This model does not support assistant message prefill. The conversation must end with a user message.")

    # [tools] tool_use の直後のターンに、対応する tool_result を先頭に並べる
    for n, t in enumerate(turns):
        if t["role"] != "assistant":
            continue
        ids = [b["id"] for b in t["blocks"] if b.get("type") == "tool_use"]
        if not ids:
            continue
        nxt = turns[n + 1] if n + 1 < len(turns) else None
        results = [b for b in (nxt["blocks"] if nxt else []) if b.get("type") == "tool_result"]
        missing = [i for i in ids if i not in {r["tool_use_id"] for r in results}]
        if missing:
            where = nxt["index"] if nxt else t["index"]
            # 文書が示す文言は "tool_use ids were found without tool_result blocks immediately after" の部分だけ
            raise bad(f"messages.{where}: `tool_use` ids were found without `tool_result` blocks immediately after: {', '.join(missing)} [rest is mock wording]")
        seen_other = False
        for j, b in enumerate(nxt["blocks"]):
            if b.get("type") != "tool_result":
                seen_other = True
            elif seen_other:
                raise bad(f"[mock wording] messages.{nxt['index']}: tool_result blocks must come FIRST in the content array; text must come after all tool results")
        extra = [r["tool_use_id"] for r in results if r["tool_use_id"] not in ids]
        if extra:
            lint.append(f"messages.{nxt['index']}: tool_result for unknown tool_use ids {extra} (error message not documented; not rejected)")
    for n, t in enumerate(turns):
        if t["role"] == "user" and any(b.get("type") == "tool_result" for b in t["blocks"]):
            prev = turns[n - 1] if n > 0 else None
            if not prev or not any(b.get("type") == "tool_use" for b in prev["blocks"]):
                lint.append(f"messages.{t['index']}: tool_result without a preceding tool_use turn (not rejected)")


# --- 思考ブロックの往復 -----------------------------------------------------
# [errors] "Thinking blocks cannot be modified": ツール使用中は、アシスタントの番の thinking /
# redacted_thinking を受け取ったとおりに返す(本文が空のものも含めて)。疑似APIは自分が出した
# ブロックを覚えておき、直近のアシスタント発言で消されたり変えられたりしていないかを見る。
ISSUED_THINKING = {}  # tool_use id -> その応答の thinking ブロック列


def check_thinking_round_trip(messages):
    turns = merged_turns(messages)
    last = next((t for t in reversed(turns) if t["role"] == "assistant"), None)
    if not last:
        return
    issued = None
    for b in last["blocks"]:
        if b.get("type") == "tool_use" and b.get("id") in ISSUED_THINKING:
            issued = ISSUED_THINKING[b["id"]]
            break
    if not issued:
        return
    sent = [b for b in last["blocks"] if b.get("type") in ("thinking", "redacted_thinking")]
    for j, want in enumerate(issued):
        got = sent[j] if j < len(sent) else None
        if got != want:
            raise bad(f"messages.{last['index']}.content.{j}: `thinking` or `redacted_thinking` blocks in the latest assistant message cannot be modified. These blocks must remain as they were in the original response.")


# --- 応答 -------------------------------------------------------------------

def build_message(res, model, thinking_on, body_len):
    """[messages] の Message の形。LLM役の応答 {content, tool_calls, reasoning_content,
    stop_reason?} から組み立てる。"""
    content = []
    thinking = []
    if thinking_on:
        # 表示の既定は "omitted" で本文は空 [skill]。LLM役が思考を書いたときだけ中身を入れる。
        thinking = [{"type": "thinking", "thinking": res.get("reasoning_content") or "",
                     "signature": "mocksig_" + uuid.uuid4().hex}]
        content.extend(thinking)
    if res.get("content"):
        content.append({"type": "text", "text": res["content"]})
    calls = []
    for c in res.get("tool_calls") or []:
        args = c["arguments"]
        if isinstance(args, str):
            args = json.loads(args)
        call = {"type": "tool_use", "id": "toolu_mock" + uuid.uuid4().hex[:16],
                "name": c["name"], "input": args}
        calls.append(call)
        content.append(call)
    for call in calls:
        ISSUED_THINKING[call["id"]] = thinking
    stop = res.get("stop_reason") or ("tool_use" if calls else "end_turn")
    return {
        "id": "msg_mock" + uuid.uuid4().hex[:20],
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop,
        "stop_sequence": None,
        "usage": {"input_tokens": body_len // 4, "output_tokens": max(1, len(json.dumps(content)) // 4)},
    }
