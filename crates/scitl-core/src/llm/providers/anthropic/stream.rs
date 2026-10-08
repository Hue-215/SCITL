//! Anthropic形式のストリーミングの応答(SSE)を読む。
//!
//! イベントは`message_start`、ブロックごとの`content_block_start`・`content_block_delta`・
//! `content_block_stop`、`message_delta`(終了理由)、`message_stop`の順に届く。生存確認の`ping`は
//! 読み飛ばし、途中の失敗は`error`で届く。

use reqwest::StatusCode;
use serde_json::{json, Value};

use crate::llm::{ErrorDetail, FinishReason, LlmError, Replay, ResponseEvent, SentSecrets};

use super::Thinking;

/// 組み立て中のブロック。
struct Block {
    index: u64,
    /// `content_block_start`のブロックに差分を足していったもの。
    value: Value,
    /// `tool_use`の引数(`input_json_delta`)の断片を連結したもの。応答を読み終えてから`input`に
    /// 読む(長さの上限で途中で切れたかは、終了理由が届くまで分からないため)。
    partial_json: Option<String>,
}

impl Block {
    fn kind(&self) -> Option<&str> {
        self.value.get("type").and_then(Value::as_str)
    }

    /// 連結した引数を`input`に読む。`input`の無い`tool_use`は空のオブジェクトにする(送り返す
    /// ときに欠けないように)。
    fn finish_input(&mut self, secrets: &SentSecrets) -> Result<(), LlmError> {
        if let Some(text) = self.partial_json.take() {
            self.value["input"] = super::super::streamed_arguments(&text, secrets)?;
        } else if self.kind() == Some("tool_use") && self.value.get("input").is_none() {
            self.value["input"] = json!({});
        }
        Ok(())
    }
}

/// 応答を読み、本文と思考の断片を届いた順に渡す。ツール呼び出しはブロックを組み立て終えてから、
/// 最後に`Done`の前に渡す。返り値は送り返す応答で、ストリーミングしないときと同じく、思考の
/// ブロックを含む応答ならすべてのブロックを並びごと持つ(ブロックは差分から組み立て直したもの)。
///
/// 失敗は`Err`で返す(`LlmAdapter::send`の約束事)。それまでに渡した断片は画面に流れているが、
/// 呼び出し側は保存しない。`refusal`で打ち切られた応答も同じ。
pub(super) async fn read(
    response: reqwest::Response,
    secrets: &SentSecrets,
    thinking: Thinking,
    on_event: &mut (dyn FnMut(ResponseEvent) + Send),
) -> Result<Replay, LlmError> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut stop_reason: Option<String> = None;
    let mut stop_details: Option<Value> = None;
    let stopped = super::super::sse::read_data(response, secrets, |data| {
        // 生存確認に空の`data`を送る中継がある。
        if data.trim().is_empty() {
            return Ok(false);
        }
        let event = super::super::parse_event(&data, secrets)?;
        match event.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let index = block_index(&event)?;
                let value = event
                    .get("content_block")
                    .filter(|b| b.is_object())
                    .cloned()
                    .ok_or_else(|| invalid("a content block is not an object"))?;
                // 開始のブロックは普通は空だが、中身を持って届いても落とさない。
                for (key, delta) in [("text", "text_delta"), ("thinking", "thinking_delta")] {
                    if let Some(text) = value.get(key).and_then(Value::as_str) {
                        emit_delta(delta, text, on_event);
                    }
                }
                if blocks.len() >= super::super::MAX_STREAMED_ELEMENTS {
                    return Err(super::super::too_many_elements());
                }
                blocks.push(Block {
                    index,
                    value,
                    partial_json: None,
                });
            }
            Some("content_block_delta") => {
                let index = block_index(&event)?;
                let block = blocks
                    .iter_mut()
                    .rfind(|b| b.index == index)
                    .ok_or_else(|| invalid("a delta arrived for a block that has not started"))?;
                let delta = event.get("delta").unwrap_or(&Value::Null);
                let text = |key: &str| delta.get(key).and_then(Value::as_str).unwrap_or_default();
                match delta.get("type").and_then(Value::as_str) {
                    Some(kind @ "text_delta") => {
                        super::super::append_text(&mut block.value, "text", text("text"));
                        emit_delta(kind, text("text"), on_event);
                    }
                    Some(kind @ "thinking_delta") => {
                        super::super::append_text(&mut block.value, "thinking", text("thinking"));
                        emit_delta(kind, text("thinking"), on_event);
                    }
                    // 署名は断片ではなく1つの値で届く。開始のブロックの値(空)は置き換える。
                    Some("signature_delta") => {
                        block.value["signature"] = json!(text("signature"));
                    }
                    Some("input_json_delta") => block
                        .partial_json
                        .get_or_insert_with(String::new)
                        .push_str(text("partial_json")),
                    Some("citations_delta") => {
                        if let Some(citation) = delta.get("citation") {
                            match block.value.get_mut("citations") {
                                Some(Value::Array(list)) => list.push(citation.clone()),
                                _ => block.value["citations"] = json!([citation]),
                            }
                        }
                    }
                    // 知らない差分は、送り返すブロックに入れようが無いので読み飛ばす。
                    _ => {}
                }
            }
            Some("message_delta") => {
                let delta = event.get("delta").unwrap_or(&Value::Null);
                if let Some(details) = delta.get("stop_details").filter(|d| !d.is_null()) {
                    stop_details = Some(details.clone());
                }
                // 終了理由が届いたら完了とし、残り(`message_stop`)は読まない。`message_stop`を
                // 送らずに接続を開けたままにする中継で、組み立て終えた応答をタイムアウトで捨てないため。
                if let Some(reason) = delta.get("stop_reason").and_then(Value::as_str) {
                    stop_reason = Some(reason.to_string());
                    return Ok(true);
                }
            }
            Some("message_stop") => return Ok(true),
            Some("error") => return Err(stream_error(&event, &data, secrets, thinking)),
            // 種類を付けずに`{"error": …}`だけを送る中継がある。
            None if event.get("error").is_some() => {
                return Err(stream_error(&event, &data, secrets, thinking))
            }
            // `message_start`(本文は空)・`content_block_stop`・`ping`・知らないイベント。
            _ => {}
        }
        Ok(false)
    })
    .await?;

    if !stopped {
        return Err(super::super::stream_cut_off());
    }
    if stop_reason.as_deref() == Some("refusal") {
        let details = stop_details.map(|d| d.to_string()).unwrap_or_default();
        return Err(LlmError::Refused(ErrorDetail::http(
            StatusCode::OK,
            &details,
            secrets,
        )));
    }

    let finish_reason = super::finish_reason(stop_reason.as_deref());
    let mut replay = false;
    for block in &mut blocks {
        block
            .finish_input(secrets)
            .map_err(|e| match finish_reason {
                FinishReason::Length => super::super::tool_call_cut_off(),
                _ => e,
            })?;
        match block.kind() {
            Some("thinking" | "redacted_thinking") => replay = true,
            Some("tool_use") => on_event(super::tool_call(&block.value)),
            _ => {}
        }
    }
    on_event(ResponseEvent::Done { finish_reason });

    Ok(if replay {
        Replay::new(
            blocks
                .iter()
                .map(|b| super::super::assembled_element(&b.value))
                .collect(),
        )
    } else {
        Replay::default()
    })
}

fn invalid(message: &'static str) -> LlmError {
    LlmError::InvalidResponse(ErrorDetail::internal(message))
}

fn block_index(event: &Value) -> Result<u64, LlmError> {
    event
        .get("index")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("a content block event has no index"))
}

/// 本文・思考の断片を渡す。空の断片は渡さない。
fn emit_delta(kind: &str, text: &str, on_event: &mut (dyn FnMut(ResponseEvent) + Send)) {
    if text.is_empty() {
        return;
    }
    let text = text.to_string();
    on_event(match kind {
        "thinking_delta" => ResponseEvent::ReasoningDelta { text },
        _ => ResponseEvent::TextDelta { text },
    });
}

/// 途中で届いた`error`を、同じ種類のエラー応答と同じに分類する。状態コードは200のままなので、
/// エラーの種類(`error.type`)から、エラー応答で返るときの状態コードに読み替える。
fn stream_error(event: &Value, data: &str, secrets: &SentSecrets, thinking: Thinking) -> LlmError {
    let status = match event.pointer("/error/type").and_then(Value::as_str) {
        Some("invalid_request_error") => StatusCode::BAD_REQUEST,
        Some("authentication_error") => StatusCode::UNAUTHORIZED,
        Some("permission_error") => StatusCode::FORBIDDEN,
        Some("not_found_error") => StatusCode::NOT_FOUND,
        Some("request_too_large") => StatusCode::PAYLOAD_TOO_LARGE,
        Some("rate_limit_error") => StatusCode::TOO_MANY_REQUESTS,
        Some("overloaded_error") => StatusCode::from_u16(529).expect("529 is a valid status code"),
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    super::http_error(status, data, secrets, thinking)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use secrecy::SecretString;

    use super::super::super::test_server::{now, spawn_event_stream};
    use super::super::{AnthropicAdapter, MessageResponse};
    use super::*;
    use crate::config::ReasoningEffort;
    use crate::error::CoreError;
    use crate::llm::{ChatMessage, LlmAdapter, PromptText, ToolOffer};

    async fn send_streamed(
        pieces: &[&'static str],
    ) -> (Result<Replay, CoreError>, Vec<ResponseEvent>, Value) {
        send_streamed_with(now(pieces), Duration::from_secs(30)).await
    }

    /// 断片ごとの待ち時間と、応答タイムアウトを決めて送る。
    async fn send_streamed_with(
        pieces: Vec<(Duration, &'static [u8])>,
        timeout: Duration,
    ) -> (Result<Replay, CoreError>, Vec<ResponseEvent>, Value) {
        let (base_url, handle) = spawn_event_stream(pieces);
        let adapter = AnthropicAdapter::new(
            base_url,
            crate::llm::providers::Credentials::key_only(SecretString::from("")),
            "claude-test",
            timeout,
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
        (result, events, handle.join().unwrap().body)
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

    const THINKING_TEXT_AND_TOOL_USE: &[&str] = &[
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"stop_reason\":null}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"\"}}\n\n",
        "event: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"pl\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"an\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"signature_delta\",\"signature\":\"sig\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"add\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"ing\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"add_steps\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"descriptions\\\": [\\\"dr\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"aft\\\"]}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":12}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];

    /// ストリーミングしないときに同じ応答として返るブロック。
    const SAME_REPLY_AS_JSON: &str = r#"{"type":"message","content":[
        {"type":"thinking","thinking":"plan","signature":"sig"},
        {"type":"text","text":"adding"},
        {"type":"tool_use","id":"toolu_1","name":"add_steps","input":{"descriptions":["draft"]}}
    ],"stop_reason":"tool_use"}"#;

    #[tokio::test]
    async fn asks_for_a_stream_and_passes_deltas_in_the_order_they_arrive() {
        let (result, events, body) = send_streamed(THINKING_TEXT_AND_TOOL_USE).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(body["stream"], true);
        assert_eq!(
            events,
            vec![
                reasoning("pl"),
                reasoning("an"),
                text("add"),
                text("ing"),
                ResponseEvent::ToolCall {
                    id: Some("toolu_1".to_string()),
                    name: "add_steps".to_string(),
                    arguments: json!({"descriptions": ["draft"]}).into(),
                },
                ResponseEvent::Done {
                    finish_reason: FinishReason::ToolCall
                },
            ]
        );
    }

    /// 差分から組み立て直したブロックは、ストリーミングしないときに受け取るブロックと(キーの順を
    /// 除いて)同じになる。
    #[tokio::test]
    async fn the_rebuilt_blocks_equal_the_blocks_received_without_streaming() {
        let (result, _, _) = send_streamed(THINKING_TEXT_AND_TOOL_USE).await;
        let replayed: Vec<Value> = result
            .unwrap()
            .elements()
            .iter()
            .map(|b| serde_json::from_str(b.get()).unwrap())
            .collect();
        let expected: MessageResponse = serde_json::from_str(SAME_REPLY_AS_JSON).unwrap();
        let expected: Vec<Value> = expected
            .content
            .iter()
            .map(|b| serde_json::from_str(b.get()).unwrap())
            .collect();
        assert_eq!(replayed, expected);
    }

    #[tokio::test]
    async fn returns_nothing_to_replay_without_thinking_and_reports_truncation() {
        let (result, events, _) = send_streamed(&[
            "data: {\"type\":\"message_start\",\"message\":{\"content\":[]}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"cut\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ])
        .await;
        assert_eq!(result.unwrap(), Replay::default());
        assert_eq!(
            events,
            vec![
                text("cut"),
                ResponseEvent::Done {
                    finish_reason: FinishReason::Length
                }
            ]
        );
    }

    /// 引数の断片が無い呼び出しは、空のオブジェクトの引数にする。
    #[tokio::test]
    async fn a_tool_use_without_input_fragments_has_an_empty_object() {
        let (result, events, _) = send_streamed(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"list_tasks\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ])
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            events[0],
            ResponseEvent::ToolCall {
                id: Some("toolu_1".to_string()),
                name: "list_tasks".to_string(),
                arguments: json!({}).into(),
            }
        );
    }

    /// 断られたと分かるのは最後なので、本文は流れたあとだが、返信にはしない。
    #[tokio::test]
    async fn a_refusal_at_the_end_is_an_error() {
        let (result, events, _) = send_streamed(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"type\":\"refusal\",\"category\":\"cyber\"}}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Refused(detail))) if detail.as_str().contains("cyber")),
            "{result:?}"
        );
        assert_eq!(events, vec![text("partial")]);
    }

    /// 途中の`error`は、エラーの種類から状態コードに読み替えて分類する。
    #[tokio::test]
    async fn an_error_event_is_classified_by_its_type() {
        let (result, _, _) = send_streamed(&[
            "data: {\"type\":\"message_start\",\"message\":{\"content\":[]}}\n\n",
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"slow down\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::RateLimit(_)))),
            "{result:?}"
        );

        let (result, _, _) = send_streamed(&[
            "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Http(detail))) if detail.as_str().contains("Overloaded")),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_stream_cut_off_before_the_stop_reason_is_a_connection_error() {
        let (result, events, _) = send_streamed(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"half\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Connection(_)))),
            "{result:?}"
        );
        assert_eq!(events, vec![text("half")]);
    }

    /// `message_stop`を省いても、終了理由が届いていれば終わった応答として読む。
    #[tokio::test]
    async fn the_stop_reason_without_message_stop_completes_the_reply() {
        let (result, events, _) = send_streamed(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"ok\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        ])
        .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            events,
            vec![
                text("ok"),
                ResponseEvent::Done {
                    finish_reason: FinishReason::Stop
                }
            ]
        );
    }

    #[tokio::test]
    async fn rejects_a_delta_for_a_block_that_has_not_started_and_broken_arguments() {
        let (result, _, _) = send_streamed(&[
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"x\"}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::InvalidResponse(_)))),
            "{result:?}"
        );

        let (result, _, _) = send_streamed(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"list_tasks\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"a\\\":\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::InvalidResponse(_)))),
            "{result:?}"
        );
    }

    /// 終了理由が届いたら、`message_stop`を待たずに完了とする(接続を開けたままにする中継で、
    /// 組み立て終えた応答をタイムアウトで捨てない)。
    #[tokio::test]
    async fn completes_at_the_stop_reason_even_if_the_connection_stays_open() {
        let mut pieces = now(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"ok\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        ]);
        pieces.push((
            Duration::from_secs(2),
            b"data: {\"type\":\"message_stop\"}\n\n",
        ));
        let (result, events, _) = send_streamed_with(pieces, Duration::from_millis(300)).await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(events.len(), 2, "{events:?}");
    }

    /// データの届かない時間が応答タイムアウトを超えたら、タイムアウトにする。
    #[tokio::test]
    async fn a_silent_stream_times_out() {
        let mut pieces = now(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        ]);
        pieces.push((
            Duration::from_secs(2),
            b"data: {\"type\":\"message_stop\"}\n\n",
        ));
        let (result, _, _) = send_streamed_with(pieces, Duration::from_millis(300)).await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Timeout(_)))),
            "{result:?}"
        );
    }

    /// 伏せた思考は開始のブロックのまま、引用は`citations`に足して組み立てる。
    #[tokio::test]
    async fn rebuilds_redacted_thinking_and_citations() {
        let (result, _, _) = send_streamed(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"redacted_thinking\",\"data\":\"opaque\"}}\n\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\",\"citations\":null}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"citations_delta\",\"citation\":{\"cited_text\":\"a\"}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"citations_delta\",\"citation\":{\"cited_text\":\"b\"}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"said\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n",
        ])
        .await;
        let replayed: Vec<Value> = result
            .unwrap()
            .elements()
            .iter()
            .map(|b| serde_json::from_str(b.get()).unwrap())
            .collect();
        assert_eq!(
            replayed,
            vec![
                json!({"type": "redacted_thinking", "data": "opaque"}),
                json!({"type": "text", "text": "said", "citations": [{"cited_text": "a"}, {"cited_text": "b"}]}),
            ]
        );
    }

    /// 長さの上限で引数の途中で切れた呼び出しは、打ち切りが原因と分かる失敗にする。
    #[tokio::test]
    async fn a_tool_call_cut_off_by_the_output_limit_says_so() {
        let (result, _, _) = send_streamed(&[
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"add_steps\",\"input\":{}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"descriptions\\\": [\\\"dr\"}}\n\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
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
            "data: {\"error\":{\"message\":\"upstream failed\",\"code\":502}}\n\n",
        ])
        .await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::Http(detail))) if detail.as_str().contains("upstream failed")),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn rejects_too_many_blocks() {
        let start: &'static str = "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n";
        let pieces = vec![start; super::super::super::MAX_STREAMED_ELEMENTS + 1];
        let (result, _, _) = send_streamed(&pieces).await;
        assert!(
            matches!(&result, Err(CoreError::Llm(LlmError::InvalidResponse(_)))),
            "{result:?}"
        );
    }
}
