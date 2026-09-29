"""anthropic_rules の自己テスト。python3 -m unittest test_anthropic_rules"""
import json
import unittest

import anthropic_rules as r

HEADERS = {"x-api-key": "k", "anthropic-version": "2023-06-01", "content-type": "application/json"}


def body(**kw):
    b = {"model": "claude-opus-5-5", "max_tokens": 1024,
         "messages": [{"role": "user", "content": "hi"}]}
    b.update(kw)
    return json.dumps(b).encode()


def tool_turns(result_blocks):
    return [
        {"role": "user", "content": "add a step"},
        {"role": "assistant", "content": [
            {"type": "tool_use", "id": "toolu_1", "name": "add_steps", "input": {}}]},
        {"role": "user", "content": result_blocks},
    ]


class Rules(unittest.TestCase):
    def rejects(self, raw, status=400, contains=""):
        with self.assertRaises(r.ApiError) as cm:
            r.check_body(raw, [])
        self.assertEqual(cm.exception.status, status)
        self.assertIn(contains, cm.exception.message)

    def test_minimal_request_passes_and_thinks_on_opus_5_5(self):
        _, thinking_on = r.check_body(body(), [])
        self.assertTrue(thinking_on)

    def test_headers(self):
        r.check_headers(HEADERS)
        with self.assertRaises(r.ApiError) as cm:
            r.check_headers({k: v for k, v in HEADERS.items() if k != "x-api-key"})
        self.assertEqual(cm.exception.status, 401)
        bearer = dict(HEADERS, Authorization="Bearer k")
        del bearer["x-api-key"]
        r.check_headers(bearer)

    def test_required_fields(self):
        raw = json.loads(body())
        del raw["max_tokens"]
        self.rejects(json.dumps(raw).encode(), contains="max_tokens")

    def test_unknown_model_is_404(self):
        self.rejects(body(model="claude-sonnet-4.6"), status=404, contains="model: claude-sonnet-4.6")

    def test_prefill_rejected_on_4_6_and_later_but_not_haiku_4_5(self):
        msgs = [{"role": "user", "content": "hi"}, {"role": "assistant", "content": "Sure"}]
        self.rejects(body(messages=msgs), contains="does not support assistant message prefill")
        r.check_body(body(model="claude-haiku-4-5", messages=msgs), [])

    def test_consecutive_roles_are_merged_not_rejected(self):
        lint = []
        msgs = [{"role": "user", "content": "a"}, {"role": "user", "content": "b"}]
        r.check_body(body(messages=msgs), lint)
        self.assertTrue(any("consecutive" in w for w in lint))

    def test_tool_use_needs_result_in_next_turn(self):
        msgs = tool_turns([{"type": "text", "text": "no result"}])
        self.rejects(body(messages=msgs), contains="without `tool_result` blocks immediately after")

    def test_text_before_tool_result_rejected(self):
        msgs = tool_turns([{"type": "text", "text": "Here:"},
                           {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}])
        self.rejects(body(messages=msgs), contains="must come FIRST")

    def test_text_after_tool_result_passes(self):
        msgs = tool_turns([{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"},
                           {"type": "text", "text": "next?"}])
        r.check_body(body(messages=msgs), [])

    def test_split_user_messages_are_merged_before_the_order_check(self):
        msgs = tool_turns([{"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}])
        msgs.append({"role": "user", "content": "and then"})
        r.check_body(body(messages=msgs), [])

    def test_forced_tool_choice(self):
        self.rejects(body(tool_choice={"type": "any"}), contains='type "tool" and "any" are not supported')
        r.check_body(body(model="claude-opus-5", tool_choice={"type": "any"}), [])

    def test_thinking_settings(self):
        self.rejects(body(thinking={"type": "disabled"}), contains='"thinking.type.disabled" is not supported')
        self.rejects(body(thinking={"type": "enabled", "budget_tokens": 2000}), contains='"thinking.type.enabled"')
        self.rejects(body(model="claude-sonnet-5-5", thinking={"type": "disabled"}), contains="between_tools")
        self.rejects(body(thinking={"type": "between_tools"}), contains="between_tools")
        _, on = r.check_body(body(model="claude-opus-4-8", thinking={"type": "disabled"}), [])
        self.assertFalse(on)
        _, on = r.check_body(body(model="claude-opus-4-8"), [])
        self.assertFalse(on)

    def test_tool_name_pattern(self):
        tools = [{"name": "bad name", "input_schema": {"type": "object"}}]
        self.rejects(body(tools=tools), contains="tools.0.name")

    def test_image_rules(self):
        def with_image(src):
            return body(messages=[{"role": "user", "content": [
                {"type": "image", "source": src}, {"type": "text", "text": "what?"}]}])
        r.check_body(with_image({"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}), [])
        self.rejects(with_image({"type": "base64", "media_type": "image/bmp", "data": "AAAA"}), contains="media_type")
        self.rejects(with_image({"type": "base64", "media_type": "image/png",
                                 "data": "data:image/png;base64,AAAA"}), contains="data: URL")

    def test_empty_text_block_rejected(self):
        self.rejects(body(messages=[{"role": "user", "content": ""}]), contains="non-empty")

    def test_thinking_blocks_must_come_back_unchanged(self):
        msg = r.build_message({"tool_calls": [{"name": "add_steps", "arguments": {}}]},
                              "claude-opus-5-5", True, 100)
        thinking = [b for b in msg["content"] if b["type"] == "thinking"]
        call = next(b for b in msg["content"] if b["type"] == "tool_use")
        result = {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": call["id"], "content": "ok"}]}
        base = [{"role": "user", "content": "add a step"}]

        dropped = base + [{"role": "assistant", "content": [call]}, result]
        with self.assertRaises(r.ApiError) as cm:
            r.check_thinking_round_trip(dropped)
        self.assertIn("cannot be modified", cm.exception.message)

        kept = base + [{"role": "assistant", "content": thinking + [call]}, result]
        r.check_thinking_round_trip(kept)

    def test_response_shape(self):
        msg = r.build_message({"content": "done"}, "claude-opus-4-8", False, 100)
        self.assertEqual(msg["type"], "message")
        self.assertEqual(msg["stop_reason"], "end_turn")
        self.assertEqual(msg["content"], [{"type": "text", "text": "done"}])


if __name__ == "__main__":
    unittest.main()
