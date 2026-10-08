//! Gemini形式(Interactions API)のストリーミングの応答(SSE)を読む。
//!
//! イベントは`data`のJSONの`event_type`で見分ける。`interaction.created`のあと、出力のステップ
//! ごとに`step.start`(ステップの種類と最初の中身)・`step.delta`(差分)・`step.stop`が`index`付きで
//! 届き、`interaction.completed`(状態)と`[DONE]`で終わる。途中の失敗は`error`で届く。

use serde_json::Value;

use crate::llm::{ErrorDetail, FinishReason, LlmError, Replay, ResponseEvent, SentSecrets};

/// ストリーミングの応答の終わりの合図。
const STREAM_DONE: &str = "[DONE]";

/// 組み立て中のステップ。
struct Step {
    index: u64,
    /// `step.start`のステップに差分を足していったもの。
    value: Value,
    /// `function_call`の引数(`arguments_delta`)の断片を連結したもの。応答を読み終えてから
    /// `arguments`に読む(長さの上限で途中で切れたかは、状態が届くまで分からないため)。
    arguments: Option<String>,
}

impl Step {
    fn kind(&self) -> Option<&str> {
        self.value.get("type").and_then(Value::as_str)
    }

    /// 連結した引数を`arguments`に読む。`arguments`の無い`function_call`は空のオブジェクトにする
    /// (送り返すときに欠けないように。定義の上では必須)。
    fn finish_arguments(&mut self, secrets: &SentSecrets) -> Result<(), LlmError> {
        if let Some(text) = self.arguments.take() {
            self.value["arguments"] = super::super::streamed_arguments(&text, secrets)?;
        } else if self.kind() == Some("function_call") && self.value.get("arguments").is_none() {
            self.value["arguments"] = Value::Object(serde_json::Map::new());
        }
        Ok(())
    }
}

/// 応答を読み、本文と思考の要約の断片を届いた順に渡す。ツール呼び出しはステップを組み立て
/// 終えてから、最後に`Done`の前に渡す。返り値は送り返す応答で、ストリーミングしないときと同じく、
/// 思考のステップを含む応答なら出力のステップ(思考・本文・呼び出し)を並びごと持つ(ステップは
/// 差分から組み立て直したもの)。
///
/// 失敗は`Err`で返す(`LlmAdapter::send`の約束事)。それまでに渡した断片は画面に流れているが、
/// 呼び出し側は保存しない。方針・安全上の判定で止められた応答も同じ。
pub(super) async fn read(
    response: reqwest::Response,
    secrets: &SentSecrets,
    on_event: &mut (dyn FnMut(ResponseEvent) + Send),
) -> Result<Replay, LlmError> {
    let mut steps: Vec<Step> = Vec::new();
    let mut status: Option<String> = None;
    super::super::sse::read_data(response, secrets, |data| {
        if data.trim() == STREAM_DONE {
            return Ok(true);
        }
        if data.trim().is_empty() {
            return Ok(false);
        }
        let event = super::super::parse_event(&data, secrets)?;
        match event.get("event_type").and_then(Value::as_str) {
            Some("step.start") => {
                let index = step_index(&event)?;
                let value = event
                    .get("step")
                    .filter(|s| s.is_object())
                    .cloned()
                    .ok_or_else(|| invalid("a step is not an object"))?;
                // 開始のステップが中身を持って届いても落とさない。
                match value.get("type").and_then(Value::as_str) {
                    Some("thought") => emit(reasoning(super::thought_summary(&value)), on_event),
                    Some("model_output") => emit(text(super::output_text(&value)), on_event),
                    _ => {}
                }
                if steps.len() >= super::super::MAX_STREAMED_ELEMENTS {
                    return Err(super::super::too_many_elements());
                }
                steps.push(Step {
                    index,
                    value,
                    arguments: None,
                });
            }
            Some("step.delta") => {
                let index = step_index(&event)?;
                let step = steps
                    .iter_mut()
                    .rfind(|s| s.index == index)
                    .ok_or_else(|| invalid("a delta arrived for a step that has not started"))?;
                let delta = event.get("delta").unwrap_or(&Value::Null);
                match delta.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        push_content(&mut step.value, "content", delta);
                        let part = delta
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        emit(text(part.to_string()), on_event);
                    }
                    Some("image" | "audio" | "video" | "document") => {
                        push_content(&mut step.value, "content", delta);
                    }
                    Some("thought_summary") => {
                        if let Some(content) = delta.get("content") {
                            push_content(&mut step.value, "summary", content);
                            let part = content.get("text").and_then(Value::as_str);
                            emit(reasoning(part.unwrap_or_default().to_string()), on_event);
                        }
                    }
                    // 署名は断片ではなく1つの値で届く。開始のステップが署名を持っていても置き換える
                    // (継ぎ足すと、同じ署名が2度届いたときに壊れた署名になる)。
                    Some("thought_signature") => {
                        if let Some(signature) = delta.get("signature").filter(|s| s.is_string()) {
                            step.value["signature"] = signature.clone();
                        }
                    }
                    Some("arguments_delta") => {
                        let part = delta.get("arguments").and_then(Value::as_str);
                        step.arguments
                            .get_or_insert_with(String::new)
                            .push_str(part.unwrap_or_default());
                    }
                    // 知らない差分(根拠の注記等)は、送り返すステップに入れようが無いので読み飛ばす。
                    _ => {}
                }
            }
            Some("interaction.completed") => {
                status = event
                    .pointer("/interaction/status")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                // 状態が届いたら完了とし、残り(`[DONE]`)は読まない。
                return Ok(status.is_some());
            }
            // 種類を付けずに`{"error": …}`だけを送る中継もある。
            Some("error") | None if event.get("error").is_some() => {
                let error = event.get("error").cloned().unwrap_or(Value::Null);
                return Err(super::failure(&[error], secrets));
            }
            // `interaction.created`・`interaction.status_update`・`step.stop`・知らないイベント。
            _ => {}
        }
        Ok(false)
    })
    .await?;

    // 状態の無いまま本文が終わった(`[DONE]`だけが届いた場合も含む)。
    let Some(status) = status else {
        return Err(super::super::stream_cut_off());
    };
    let finish_reason = super::finish_reason(&status, &[], secrets)?;

    let mut thought = false;
    let mut replayed = Vec::new();
    for step in &mut steps {
        step.finish_arguments(secrets)
            .map_err(|e| match finish_reason {
                FinishReason::Length => super::super::tool_call_cut_off(),
                _ => e,
            })?;
        if !super::is_replayed_step(&step.value) {
            continue;
        }
        match step.kind() {
            Some("thought") => thought = true,
            Some("function_call") => on_event(super::tool_call(&step.value)),
            _ => {}
        }
        replayed.push(super::super::assembled_element(&step.value));
    }
    on_event(ResponseEvent::Done { finish_reason });

    Ok(if thought {
        Replay::new(replayed)
    } else {
        Replay::default()
    })
}

fn invalid(message: &'static str) -> LlmError {
    LlmError::InvalidResponse(ErrorDetail::internal(message))
}

fn step_index(event: &Value) -> Result<u64, LlmError> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("a step event has no index"))
}

fn text(text: String) -> ResponseEvent {
    ResponseEvent::TextDelta { text }
}

fn reasoning(text: String) -> ResponseEvent {
    ResponseEvent::ReasoningDelta { text }
}

/// 本文・思考の断片を渡す。空の断片は渡さない。
fn emit(event: ResponseEvent, on_event: &mut (dyn FnMut(ResponseEvent) + Send)) {
    match &event {
        ResponseEvent::TextDelta { text } | ResponseEvent::ReasoningDelta { text }
            if text.is_empty() => {}
        _ => on_event(event),
    }
}

/// ステップの`key`の並び(`content`・`summary`)に中身を足す。本文の断片は、直前の本文に続けて
/// 1つの本文にする(ストリーミングしないときは、続く本文は1つの要素で返る)。
fn push_content(step: &mut Value, key: &str, part: &Value) {
    let is_text = |v: &Value| v.get("type").and_then(Value::as_str) == Some("text");
    if !step.get(key).is_some_and(Value::is_array) {
        step[key] = Value::Array(Vec::new());
    }
    let list = step[key].as_array_mut().expect("just made an array");
    match list.last_mut() {
        Some(last) if is_text(last) && is_text(part) => {
            let more = part.get("text").and_then(Value::as_str).unwrap_or_default();
            super::super::append_text(last, "text", more);
        }
        _ => list.push(part.clone()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use secrecy::SecretString;
    use serde_json::json;

    use super::super::super::test_server::{now, spawn_event_stream};
    use super::super::{GeminiAdapter, InteractionResponse};
    use super::*;
    use crate::config::ReasoningEffort;
    use crate::error::CoreError;
    use crate::llm::{ChatMessage, LlmAdapter, PromptText, ToolOffer};

    async fn send_streamed(
        pieces: &[&'static str],
    ) -> (Result<Replay, CoreError>, Vec<ResponseEvent>, Value) {
        let (base_url, handle) = spawn_event_stream(now(pieces));
        let adapter = GeminiAdapter::new(
            base_url,
            crate::llm::providers::Credentials::key_only(SecretString::from("")),
            "gemini-test",
            Duration::from_secs(30),
        )
        .unwrap();
        let mut events = Vec::new();
        let result = adapter
            .send(
                None,
                &[ChatMessage::user(PromptText::user_message("hi", None))],
                ToolOffer::NONE,
                Some(ReasoningEffort::High),
                &mut |e| events.push(e),
            )
            .await;
        let received = handle.join().unwrap();
        assert!(
            received.headers.contains("accept: text/event-stream"),
            "{}",
            received.headers
        );
        (result, events, received.body)
    }

    fn text(t: &str) -> ResponseEvent {
        ResponseEvent::TextDelta {
            text: t.to_string(),
        }
    }

    fn reasoning(t: &str) -> ResponseEvent {
        ResponseEvent::ReasoningDelta {
            text: t.to_string(),
        }
    }

    const THOUGHT_TEXT_AND_CALL: &[&str] = &[
        "data: {\"event_type\":\"interaction.created\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"in_progress\"}}\n\n",
        "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"thought\"}}\n\n",
        "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"thought_summary\",\"content\":{\"type\":\"text\",\"text\":\"pl\"}}}\n\n",
        "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"thought_summary\",\"content\":{\"type\":\"text\",\"text\":\"an\"}}}\n\n",
        "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"thought_signature\",\"signature\":\"sig\"}}\n\n",
        "data: {\"event_type\":\"step.stop\",\"index\":0}\n\n",
        "data: {\"event_type\":\"step.start\",\"index\":1,\"step\":{\"type\":\"model_output\"}}\n\n",
        "data: {\"event_type\":\"step.delta\",\"index\":1,\"delta\":{\"type\":\"text\",\"text\":\"add\"}}\n\n",
        "data: {\"event_type\":\"step.delta\",\"index\":1,\"delta\":{\"type\":\"text\",\"text\":\"ing\"}}\n\n",
        "data: {\"event_type\":\"step.stop\",\"index\":1}\n\n",
        "data: {\"event_type\":\"step.start\",\"index\":2,\"step\":{\"type\":\"function_call\",\"id\":\"call_1\",\"name\":\"add_steps\",\"arguments\":{}}}\n\n",
        "data: {\"event_type\":\"step.delta\",\"index\":2,\"delta\":{\"type\":\"arguments_delta\",\"arguments\":\"{\\\"descriptions\\\":[\\\"dr\"}}\n\n",
        "data: {\"event_type\":\"step.delta\",\"index\":2,\"delta\":{\"type\":\"arguments_delta\",\"arguments\":\"aft\\\"]}\"}}\n\n",
        "data: {\"event_type\":\"step.delta\",\"index\":2,\"delta\":{\"type\":\"thought_signature\",\"signature\":\"sig2\"}}\n\n",
        "data: {\"event_type\":\"step.stop\",\"index\":2}\n\n",
        "data: {\"event_type\":\"interaction.status_update\",\"interaction_id\":\"v1_x\",\"status\":\"requires_action\"}\n\n",
        "data: {\"event_type\":\"interaction.completed\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"requires_action\",\"usage\":{\"total_tokens\":3}}}\n\n",
        "data: [DONE]\n\n",
    ];

    /// ストリーミングしないときに同じ応答として返るステップ。
    const SAME_REPLY_AS_JSON: &str = r#"{"status":"requires_action","steps":[
        {"type":"thought","signature":"sig","summary":[{"type":"text","text":"plan"}]},
        {"type":"model_output","content":[{"type":"text","text":"adding"}]},
        {"type":"function_call","id":"call_1","name":"add_steps","arguments":{"descriptions":["draft"]},"signature":"sig2"}
    ]}"#;

    #[tokio::test]
    async fn asks_for_a_stream_and_passes_deltas_in_the_order_they_arrive() {
        let (result, events, body) = send_streamed(THOUGHT_TEXT_AND_CALL).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert_eq!(
            events,
            vec![
                reasoning("pl"),
                reasoning("an"),
                text("add"),
                text("ing"),
                ResponseEvent::ToolCall {
                    id: Some("call_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({"descriptions": ["draft"]}).into(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall
                },
            ]
        );
    }

    /// 差分から組み立て直したステップは、ストリーミングしないときに受け取るステップと(キーの順を
    /// 除いて)同じになる。
    #[tokio::test]
    async fn the_rebuilt_steps_equal_the_steps_received_without_streaming() {
        let (result, _, _) = send_streamed(THOUGHT_TEXT_AND_CALL).await;
        let replayed: Vec<Value> = result
            .unwrap()
            .elements()
            .iter()
            .map(|s| serde_json::from_str(s.get()).unwrap())
            .collect();
        let expected: InteractionResponse = serde_json::from_str(SAME_REPLY_AS_JSON).unwrap();
        let expected: Vec<Value> = expected
            .steps
            .iter()
            .map(|s| serde_json::from_str(s.get()).unwrap())
            .collect();
        assert_eq!(replayed, expected);
    }

    #[tokio::test]
    async fn returns_nothing_to_replay_without_a_thought_and_reports_truncation() {
        let (result, events, _) = send_streamed(&[
            "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"model_output\",\"content\":[{\"type\":\"text\",\"text\":\"c\"}]}}\n\n",
            "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"text\",\"text\":\"ut\"}}\n\n",
            "data: {\"event_type\":\"step.stop\",\"index\":0}\n\n",
            "data: {\"event_type\":\"interaction.completed\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"incomplete\"}}\n\n",
        ])
        .await;
        assert_eq!(result.unwrap(), Replay::default());
        assert_eq!(
            events,
            vec![
                text("c"),
                text("ut"),
                ResponseEvent::Done {
                    finish_reason: FinishReason::Length
                }
            ]
        );
    }

    /// 途中の`error`は、方針・安全上の判定で止めたものなら断られたものとし、本文は流れたあとだが
    /// 返信にはしない。
    #[tokio::test]
    async fn an_error_event_is_a_refusal_when_blocked_and_otherwise_a_provider_error() {
        let (result, events, _) = send_streamed(&[
            "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"model_output\"}}\n\n",
            "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"text\",\"text\":\"partial\"}}\n\n",
            "data: {\"event_type\":\"error\",\"error\":{\"code\":\"safety\",\"message\":\"blocked\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Refused(detail))) if detail.as_str().contains("safety")),
            "{result:?}"
        );
        assert_eq!(events, vec![text("partial")]);

        let (result, _, _) = send_streamed(&[
            "data: {\"event_type\":\"error\",\"error\":{\"code\":\"internal\",\"message\":\"oops\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Http(detail))) if detail.as_str().contains("oops")),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_status_without_an_error_event_is_a_provider_error() {
        let (result, _, _) = send_streamed(&[
            "data: {\"event_type\":\"interaction.completed\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"failed\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Http(_)))),
            "{result:?}"
        );
    }

    /// 状態の届かないまま終わった応答は、`[DONE]`が届いても途中で切れたものとする。
    #[tokio::test]
    async fn a_stream_without_the_final_status_is_a_connection_error() {
        for pieces in [
            &[
                "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"model_output\"}}\n\n",
                "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"text\",\"text\":\"half\"}}\n\n",
            ][..],
            &[
                "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"model_output\"}}\n\n",
                "data: [DONE]\n\n",
            ][..],
        ] {
            let (result, _, _) = send_streamed(pieces).await;
            assert!(
                matches!(&result, Err(CoreError::Llm(LlmError::Connection(_)))),
                "{result:?}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_a_delta_for_a_step_that_has_not_started_and_broken_arguments() {
        let (result, _, _) = send_streamed(&[
            "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"text\",\"text\":\"x\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::InvalidResponse(_)))),
            "{result:?}"
        );

        let (result, _, _) = send_streamed(&[
            "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"function_call\",\"id\":\"call_1\",\"name\":\"a\"}}\n\n",
            "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"arguments_delta\",\"arguments\":\"{\\\"x\\\":\"}}\n\n",
            "data: {\"event_type\":\"step.stop\",\"index\":0}\n\n",
            "data: {\"event_type\":\"interaction.completed\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"requires_action\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::InvalidResponse(_)))),
            "{result:?}"
        );
    }

    /// サーバー側のツール等のステップは、ストリーミングしないときと同じく送り返さない。
    #[tokio::test]
    async fn leaves_steps_other_than_the_output_out_of_the_replay() {
        let (result, _, _) = send_streamed(&[
            "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"thought\",\"signature\":\"sig\"}}\n\n",
            "data: {\"event_type\":\"step.start\",\"index\":1,\"step\":{\"type\":\"google_search_call\",\"id\":\"s1\"}}\n\n",
            "data: {\"event_type\":\"step.start\",\"index\":2,\"step\":{\"type\":\"model_output\",\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}\n\n",
            "data: {\"event_type\":\"interaction.completed\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"completed\"}}\n\n",
        ])
        .await;
        let types: Vec<String> = result
            .unwrap()
            .elements()
            .iter()
            .map(|s| serde_json::from_str::<Value>(s.get()).unwrap()["type"].to_string())
            .collect();
        assert_eq!(types, ["\"thought\"", "\"model_output\""]);
    }

    /// 署名は置き換える。開始のステップと差分の両方で届いても、2度つながない。
    #[tokio::test]
    async fn a_signature_sent_twice_is_kept_once() {
        let (result, _, _) = send_streamed(&[
            "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"thought\",\"signature\":\"sig\"}}\n\n",
            "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"thought_signature\",\"signature\":\"sig\"}}\n\n",
            "data: {\"event_type\":\"interaction.completed\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"completed\"}}\n\n",
        ])
        .await;
        let replay = result.unwrap();
        let step: Value = serde_json::from_str(replay.elements()[0].get()).unwrap();
        assert_eq!(step, json!({"type": "thought", "signature": "sig"}));
    }

    /// 引数の届かない呼び出しも、送り返すステップに空の`arguments`を持たせる。
    #[tokio::test]
    async fn a_call_without_arguments_is_replayed_with_an_empty_object() {
        let (result, events, _) = send_streamed(&[
            "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"thought\",\"signature\":\"sig\"}}\n\n",
            "data: {\"event_type\":\"step.start\",\"index\":1,\"step\":{\"type\":\"function_call\",\"id\":\"call_1\",\"name\":\"list_tasks\"}}\n\n",
            "data: {\"event_type\":\"step.stop\",\"index\":1}\n\n",
            "data: {\"event_type\":\"interaction.completed\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"requires_action\"}}\n\n",
        ])
        .await;
        let replay = result.unwrap();
        let call: Value = serde_json::from_str(replay.elements()[1].get()).unwrap();
        assert_eq!(
            call,
            json!({"type": "function_call", "id": "call_1", "name": "list_tasks", "arguments": {}})
        );
        assert_eq!(
            events[0],
            ResponseEvent::ToolCall {
                id: Some("call_1".to_string()),
                name: "list_tasks".to_string(),
                arguments: json!({}).into(),
            }
        );
    }

    /// 長さの上限で引数の途中で切れた呼び出しは、打ち切りが原因と分かる失敗にする。
    #[tokio::test]
    async fn a_tool_call_cut_off_by_the_output_limit_says_so() {
        let (result, _, _) = send_streamed(&[
            "data: {\"event_type\":\"step.start\",\"index\":0,\"step\":{\"type\":\"function_call\",\"id\":\"call_1\",\"name\":\"a\"}}\n\n",
            "data: {\"event_type\":\"step.delta\",\"index\":0,\"delta\":{\"type\":\"arguments_delta\",\"arguments\":\"{\\\"x\\\":\"}}\n\n",
            "data: {\"event_type\":\"interaction.completed\",\"interaction\":{\"id\":\"v1_x\",\"status\":\"incomplete\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::InvalidResponse(detail))) if detail.as_str().contains("output limit")),
            "{result:?}"
        );
    }

    /// 種類を付けずに`{"error": …}`だけを送る中継でも、エラーの中身を残す。
    #[tokio::test]
    async fn a_bare_error_object_is_an_error() {
        let (result, _, _) = send_streamed(&[
            "data: {\"error\":{\"code\":\"internal\",\"message\":\"upstream failed\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Http(detail))) if detail.as_str().contains("upstream failed")),
            "{result:?}"
        );
    }
}
