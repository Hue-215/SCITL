use std::io::{Read, Write};
use std::net::TcpListener;

use secrecy::SecretString;

use super::*;
use crate::llm::{InlineImage, SentAt, ToolSchema};

const TEST_TIMEOUT: Duration = Duration::from_secs(30);

const MINIMAL_COMPLETION: &str =
    r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#;

/// 1回だけ接続を受け、`body`をコンプリーション応答として返す。受け取ったリクエストの
/// ヘッダー部を返す。
fn spawn_capturing(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        while !received.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(&buf[..n]);
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let text = String::from_utf8_lossy(&received).to_string();
        text.split("\r\n\r\n")
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    });
    (format!("http://{addr}/v1"), handle)
}

async fn send_with_key(api_key: &str) -> String {
    let (base_url, handle) = spawn_capturing(MINIMAL_COMPLETION);
    let adapter = OpenAiCompatAdapter::new(
        base_url,
        Credentials::key_only(SecretString::from(api_key)),
        "model",
        TEST_TIMEOUT,
    )
    .unwrap();
    adapter
        .send(None, &[], ToolOffer::NONE, None, &mut |_| {})
        .await
        .unwrap();
    handle.join().unwrap()
}

#[test]
fn request_preview_is_the_body_send_would_post_without_the_key() {
    let adapter = OpenAiCompatAdapter::new(
        "http://127.0.0.1:1/v1",
        Credentials::key_only(SecretString::from("sk-preview-secret")),
        "local-model",
        TEST_TIMEOUT,
    )
    .unwrap();
    let messages = [ChatMessage::user(PromptText::user_message("hi", None))];

    let preview = adapter
        .request_preview(&messages, ToolOffer::NONE, Some(ReasoningEffort::Low))
        .unwrap();

    let expected = request_body(
        "local-model",
        &messages,
        ToolOffer::NONE,
        Some(ReasoningEffort::Low),
    );
    assert_eq!(preview.body, serde_json::to_value(expected).unwrap());
    assert!(!serde_json::to_string(&preview)
        .unwrap()
        .contains("sk-preview-secret"));
}

/// 縮めるのは画像を置く位置だけ。`data:`で始まる本文を縮めると、モデルに渡る文を
/// プレビューから隠せる。
#[test]
fn request_preview_abbreviates_only_images() {
    let adapter = OpenAiCompatAdapter::new(
        "http://127.0.0.1:1/v1",
        Credentials::key_only(SecretString::from("")),
        "local-model",
        TEST_TIMEOUT,
    )
    .unwrap();
    let text = format!("data:text/plain,{}", "x".repeat(200));
    let messages = [
        ChatMessage::User {
            text: PromptText::user_message(&text, None),
            images: vec![png()],
        },
        ChatMessage::Assistant {
            content: Some(text.clone()),
            tool_calls: Vec::new(),
            replay: Default::default(),
        },
    ];

    let body = adapter
        .request_preview(&messages, ToolOffer::NONE, None)
        .unwrap()
        .body;

    let parts = &body["messages"][0]["content"];
    assert!(parts[0]["text"].as_str().unwrap().contains(&text));
    let image = parts[1]["image_url"]["url"].as_str().unwrap();
    assert!(image.starts_with("data:image/png;base64,… ("), "{image}");
    assert_eq!(body["messages"][1]["content"], text);
}

#[tokio::test]
async fn empty_api_key_sends_no_authorization_header() {
    let headers = send_with_key("").await;
    assert!(!headers.contains("authorization:"));
}

#[tokio::test]
async fn api_key_is_sent_as_bearer() {
    let headers = send_with_key("sk-test").await;
    assert!(headers.contains("authorization: bearer sk-test"));
}

/// ヘッダーに載せられない鍵は、送る前に方言と同じ文言で断る(鍵は文言に載せない)。
#[tokio::test]
async fn a_key_that_cannot_be_sent_in_a_header_is_refused_before_sending() {
    let adapter = OpenAiCompatAdapter::new(
        "http://127.0.0.1:1/v1",
        Credentials::key_only(SecretString::from("sk-a\nb")),
        "model",
        TEST_TIMEOUT,
    )
    .unwrap();
    let result = adapter
        .send(None, &[], ToolOffer::NONE, None, &mut |_| {})
        .await;
    assert!(
        matches!(&result, Err(CoreError::Llm(LlmError::InvalidRequest(detail)))
            if detail.as_str() == "the API key contains characters that cannot be sent in a header"),
        "{result:?}"
    );
}

#[tokio::test]
async fn malformed_tool_arguments_are_passed_up_instead_of_failing_the_send() {
    let (base_url, handle) = spawn_capturing(
        r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"update_task","arguments":"{\"title\": "}}]},"finish_reason":"tool_calls"}]}"#,
    );
    let adapter = OpenAiCompatAdapter::new(
        base_url,
        Credentials::key_only(SecretString::from("")),
        "model",
        TEST_TIMEOUT,
    )
    .unwrap();
    let mut events = Vec::new();
    adapter
        .send(None, &[], ToolOffer::NONE, None, &mut |e| events.push(e))
        .await
        .unwrap();
    handle.join().unwrap();

    assert!(events.iter().any(|e| matches!(
        e,
        ResponseEvent::ToolCall { name, arguments: ToolArguments::Malformed { raw, .. }, .. }
            if name == "update_task" && raw == "{\"title\": "
    )));
}

#[tokio::test]
async fn a_content_filter_stop_is_a_refusal_without_passing_the_partial_reply() {
    let (base_url, handle) = spawn_capturing(
        r#"{"choices":[{"message":{"content":"partial"},"finish_reason":"content_filter"}]}"#,
    );
    let adapter = OpenAiCompatAdapter::new(
        base_url,
        Credentials::key_only(SecretString::from("")),
        "model",
        TEST_TIMEOUT,
    )
    .unwrap();
    let mut events = Vec::new();
    let result = adapter
        .send(None, &[], ToolOffer::NONE, None, &mut |e| events.push(e))
        .await;
    handle.join().unwrap();

    assert!(
        matches!(result, Err(CoreError::Llm(LlmError::Refused(_)))),
        "{result:?}"
    );
    assert!(events.is_empty());
}

#[tokio::test]
async fn lists_models_under_the_base_url_sorted_without_duplicates() {
    let (base_url, handle) = spawn_capturing(
        r#"{"object":"list","data":[{"id":"gpt-b","object":"model"},{"id":"gpt-a"},{"id":"gpt-b"},{"id":" "}]}"#,
    );
    let names = list_models(
        &base_url,
        &Credentials::key_only(SecretString::from("sk-test")),
    )
    .await
    .unwrap();
    let headers = handle.join().unwrap();

    assert_eq!(names, ["gpt-a", "gpt-b"]);
    assert!(headers.starts_with("get /v1/models http/1.1"));
    assert!(headers.contains("authorization: bearer sk-test"));
}

fn with_headers(headers: &[(&str, &str)]) -> Credentials {
    Credentials::new(
        SecretString::from("sk-test"),
        headers
            .iter()
            .map(|(name, value)| (name.to_string(), SecretString::from(*value)))
            .collect(),
    )
    .unwrap()
}

#[tokio::test]
async fn sends_custom_headers_with_the_session_id_in_place_of_the_placeholder() {
    let (base_url, handle) = spawn_capturing(MINIMAL_COMPLETION);
    let credentials = with_headers(&[
        ("X-Opencode-Session", "{session_id}"),
        ("X-Title", "SCITL {other}"),
    ]);
    let adapter = OpenAiCompatAdapter::new(base_url, credentials, "model", TEST_TIMEOUT).unwrap();
    let session = SessionId::for_conversation("general").unwrap();
    adapter
        .send(Some(&session), &[], ToolOffer::NONE, None, &mut |_| {})
        .await
        .unwrap();
    let headers = handle.join().unwrap();

    assert!(
        headers.contains(&format!("x-opencode-session: {}\r\n", session.as_str())),
        "{headers}"
    );
    // 置き換えるのは`{session_id}`だけ。
    assert!(headers.contains("x-title: scitl {other}\r\n"), "{headers}");
    assert!(headers.contains("authorization: bearer sk-test"));
}

#[tokio::test]
async fn listing_models_leaves_out_only_the_headers_that_need_a_session() {
    let (base_url, handle) = spawn_capturing(r#"{"data":[]}"#);
    let credentials = with_headers(&[("X-Opencode-Session", "{session_id}"), ("X-Title", "SCITL")]);
    list_models(&base_url, &credentials).await.unwrap();
    let headers = handle.join().unwrap();

    assert!(!headers.contains("x-opencode-session"), "{headers}");
    assert!(headers.contains("x-title: scitl\r\n"), "{headers}");
}

#[tokio::test]
async fn an_echoed_custom_header_value_is_redacted_from_the_error() {
    let (base_url, handle) = super::super::test_server::spawn_server(vec![(
        400,
        r#"{"error":{"message":"bad gateway token gw-secret-5678 for session"}}"#,
    )]);
    let credentials = with_headers(&[("cf-aig-authorization", "gw-secret-5678")]);
    let adapter = OpenAiCompatAdapter::new(base_url, credentials, "model", TEST_TIMEOUT).unwrap();
    let result = adapter
        .send(None, &[], ToolOffer::NONE, None, &mut |_| {})
        .await;
    handle.join().unwrap();

    let error = result.unwrap_err().to_string();
    assert!(!error.contains("gw-secret-5678"), "{error}");
    assert!(error.contains("[redacted]"), "{error}");
}

#[tokio::test]
async fn listing_models_reports_a_rejected_key_as_an_auth_error() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _ = stream.read(&mut [0u8; 4096]);
        let _ = stream.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        );
    });
    let result = list_models(
        &format!("http://{addr}/v1"),
        &Credentials::key_only(SecretString::from("")),
    )
    .await;
    server.join().unwrap();

    assert!(matches!(result, Err(CoreError::Llm(LlmError::Auth(_)))));
}

/// 思考の強さを指定したリクエストが400で返った。
fn bad_request(body: &str) -> LlmError {
    http_error(
        reqwest::StatusCode::BAD_REQUEST,
        body,
        &SentSecrets::default(),
        true,
    )
}

#[test]
fn recognizes_openai_context_exceeded() {
    let body = r#"{"error":{"message":"This model's maximum context length is 8192 tokens. However, your messages resulted in 9000 tokens.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#;
    assert!(matches!(bad_request(body), LlmError::ContextExceeded(_)));
}

#[test]
fn recognizes_llama_cpp_context_exceeded() {
    let body = r#"{"error":{"code":400,"message":"the request exceeds the available context size, try increasing it","type":"exceed_context_size_error","n_prompt_tokens":9000,"n_ctx":8192}}"#;
    assert!(matches!(bad_request(body), LlmError::ContextExceeded(_)));
}

#[test]
fn recognizes_vllm_context_exceeded_with_or_without_the_error_wrapper() {
    let message =
        "This model's maximum context length is 4096 tokens. However, you requested 5000 tokens.";
    let unwrapped = serde_json::json!({
        "object": "error", "message": message, "type": "BadRequestError", "param": null, "code": 400
    });
    let wrapped = serde_json::json!({
        "error": { "message": message, "type": "BadRequestError", "param": null, "code": 400 }
    });
    for body in [unwrapped, wrapped] {
        assert!(matches!(
            bad_request(&body.to_string()),
            LlmError::ContextExceeded(_)
        ));
    }
}

#[test]
fn recognizes_context_exceeded_in_an_error_given_as_a_bare_message() {
    for body in [
        r#"{"error":"The model is loaded with a Context Length of only 4096 tokens, which is not enough."}"#,
        r#"{"error":"the request exceeds the available context size"}"#,
    ] {
        assert!(matches!(bad_request(body), LlmError::ContextExceeded(_)));
    }
}

#[test]
fn recognizes_a_rejected_reasoning_effort() {
    for body in [
        r#"{"error":{"message":"Unsupported parameter: 'reasoning_effort' is not supported with this model.","type":"invalid_request_error","param":"reasoning_effort","code":"unsupported_parameter"}}"#,
        r#"{"error":{"message":"Unrecognized request argument supplied: reasoning_effort","type":"invalid_request_error","param":null,"code":null}}"#,
    ] {
        assert!(matches!(
            bad_request(body),
            LlmError::ReasoningEffortRejected(_)
        ));
    }
}

#[test]
fn tells_a_rejected_value_from_a_rejected_parameter() {
    for body in [
        r#"{"error":{"message":"Unsupported value: 'reasoning_effort' does not support 'none' with this model. Supported values are: 'low', 'medium', and 'high'.","type":"invalid_request_error","param":"reasoning_effort","code":"unsupported_value"}}"#,
        r#"{"error":{"message":"Invalid value for reasoning_effort: none","param":null,"code":null}}"#,
    ] {
        assert!(matches!(
            bad_request(body),
            LlmError::ReasoningEffortValueRejected(_)
        ));
    }
}

/// 強さを送っていない、または400系の入力の誤り以外なら、本文に引数名があっても
/// 状態コードによる分類に落ちる。
#[test]
fn reasoning_effort_in_the_body_alone_does_not_mean_it_was_rejected() {
    let body = r#"{"error":{"message":"Unsupported parameter: 'reasoning_effort' is not supported with this model.","param":"reasoning_effort","code":"unsupported_parameter"}}"#;
    assert!(matches!(
        http_error(
            reqwest::StatusCode::BAD_REQUEST,
            body,
            &SentSecrets::default(),
            false
        ),
        LlmError::Http(_)
    ));
    assert!(matches!(
        http_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            body,
            &SentSecrets::default(),
            true
        ),
        LlmError::RateLimit(_)
    ));
}

/// 本文で判定できなければ、状態コードによる分類に落ちる。
#[test]
fn other_error_bodies_fall_back_to_the_status_code() {
    assert!(matches!(
        bad_request(r#"{"error":{"message":"invalid model","code":"model_not_found"}}"#),
        LlmError::Http(_)
    ));
    assert!(matches!(bad_request("not json"), LlmError::Http(_)));
    assert!(matches!(
        http_error(
            reqwest::StatusCode::UNAUTHORIZED,
            "",
            &SentSecrets::default(),
            true
        ),
        LlmError::Auth(_)
    ));
}

#[test]
fn context_exceeded_keeps_the_sanitized_body_as_detail() {
    let body = r#"{"error":{"code":"context_length_exceeded","message":"sk-secret"}}"#;
    let LlmError::ContextExceeded(detail) = http_error(
        reqwest::StatusCode::BAD_REQUEST,
        body,
        &SentSecrets::new(["sk-secret"]),
        true,
    ) else {
        panic!("expected LlmError::ContextExceeded");
    };
    assert!(detail.as_str().starts_with("HTTP 400: "));
    assert!(!detail.as_str().contains("sk-secret"));
}

#[test]
fn serializes_system_user_and_assistant_text_as_openai_expects() {
    let system = serde_json::to_value(to_request_message(&ChatMessage::System(
        "be helpful".to_string(),
    )))
    .unwrap();
    assert_eq!(
        system,
        serde_json::json!({"role": "system", "content": "be helpful"})
    );

    let user = serde_json::to_value(to_request_message(&ChatMessage::user(
        PromptText::user_message("hi", Some(&utc("2026-09-22T04:12:00Z"))),
    )))
    .unwrap();
    assert_eq!(
        user,
        serde_json::json!({
            "role": "user",
            "content": "<scitl:user-message sent_at=\"2026-09-22T04:12:00+00:00\" weekday=\"Tuesday\">\nhi\n</scitl:user-message>",
        })
    );

    let assistant = serde_json::to_value(to_request_message(&ChatMessage::Assistant {
        content: Some("done".to_string()),
        tool_calls: Vec::new(),
        replay: Default::default(),
    }))
    .unwrap();
    assert_eq!(
        assistant,
        serde_json::json!({"role": "assistant", "content": "done"})
    );
}

#[test]
fn sends_reasoning_effort_only_when_given() {
    let body =
        |effort| serde_json::to_value(request_body("m", &[], ToolOffer::NONE, effort)).unwrap();
    assert!(body(None).get("reasoning_effort").is_none());
    assert_eq!(body(Some(ReasoningEffort::Off))["reasoning_effort"], "none");
    assert_eq!(
        body(Some(ReasoningEffort::High))["reasoning_effort"],
        "high"
    );
}

#[test]
fn sends_no_tools_when_they_cannot_be_called() {
    let schemas = [ToolSchema::internal(
        "search",
        "search",
        serde_json::json!({"type": "object"}),
    )];
    let body = |schemas: &[ToolSchema], callable| {
        serde_json::to_value(request_body(
            "m",
            &[],
            ToolOffer { schemas, callable },
            None,
        ))
        .unwrap()
    };

    assert_eq!(
        body(&schemas, true)["tools"][0]["function"]["name"],
        "search"
    );
    let forbidden = body(&schemas, false);
    assert!(forbidden.get("tools").is_none());
    assert!(forbidden.get("tool_choice").is_none());
}

fn user(text: &str, sent_at: &str) -> ChatMessage {
    ChatMessage::user(PromptText::user_message(text, Some(&utc(sent_at))))
}

fn utc(at: &str) -> SentAt {
    SentAt::in_zone(at, &chrono::Utc).unwrap()
}

fn assistant(text: &str) -> ChatMessage {
    ChatMessage::Assistant {
        content: Some(text.to_string()),
        tool_calls: Vec::new(),
        replay: Default::default(),
    }
}

fn tool_call(id: &str) -> ToolCallRequest {
    ToolCallRequest {
        id: Some(id.to_string()),
        name: "get_current_task_detail".to_string(),
        arguments: serde_json::json!({}).into(),
    }
}

fn request_json(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    to_request_messages(messages)
        .iter()
        .map(|m| serde_json::to_value(m).unwrap())
        .collect()
}

fn roles(sent: &[serde_json::Value]) -> Vec<&str> {
    sent.iter().map(|m| m["role"].as_str().unwrap()).collect()
}

#[test]
fn fills_in_a_user_message_when_the_conversation_starts_with_the_assistant() {
    let sent = request_json(&[
        ChatMessage::System("s".to_string()),
        assistant("a"),
        user("u", "2026-09-22T04:12:00Z"),
    ]);

    assert_eq!(roles(&sent), vec!["system", "user", "assistant", "user"]);
    // 補った発言も同じ囲みで送り、日時の属性だけを省く。
    assert_eq!(
        sent[1]["content"],
        PromptText::user_message(super::super::PLACEHOLDER_USER_TEXT, None).as_str()
    );
}

#[test]
fn merges_consecutive_messages_of_the_same_role() {
    let sent = request_json(&[
        ChatMessage::System("s".to_string()),
        user("u1", "2026-09-22T04:12:00Z"),
        user("u2", "2026-09-22T05:00:00Z"),
        assistant("a1"),
        assistant("a2"),
    ]);

    assert_eq!(roles(&sent), vec!["system", "user", "assistant"]);
    // 発言ごとの囲みと送信日時はそのまま残る。
    assert_eq!(
        sent[1]["content"],
        format!(
            "{}\n\n{}",
            PromptText::user_message("u1", Some(&utc("2026-09-22T04:12:00Z"))).as_str(),
            PromptText::user_message("u2", Some(&utc("2026-09-22T05:00:00Z"))).as_str(),
        )
    );
    assert_eq!(sent[2]["content"], "a1\n\na2");
}

fn png() -> InlineImage {
    InlineImage::from_bytes(b"\x89PNG\r\n\x1a\nbody").unwrap()
}

#[test]
fn sends_images_as_parts_after_the_text() {
    let sent = request_json(&[ChatMessage::User {
        text: PromptText::user_message("見て", None),
        images: vec![png()],
    }]);
    assert_eq!(
        sent[0],
        serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": PromptText::user_message("見て", None).as_str()},
                {"type": "image_url", "image_url": {"url": png().data_url()}},
            ],
        })
    );
    assert!(png().data_url().starts_with("data:image/png;base64,"));
}

#[test]
fn merging_keeps_images_of_either_message() {
    let sent = request_json(&[
        user("u1", "2026-09-22T04:12:00Z"),
        ChatMessage::User {
            text: PromptText::user_message("u2", None),
            images: vec![png()],
        },
    ]);
    assert_eq!(sent.len(), 1);
    let parts = sent[0]["content"].as_array().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(
        parts[0]["text"],
        format!(
            "{}\n\n{}",
            PromptText::user_message("u1", Some(&utc("2026-09-22T04:12:00Z"))).as_str(),
            PromptText::user_message("u2", None).as_str(),
        )
    );
    assert_eq!(parts[1]["type"], "image_url");
}

fn tool_result(id: &str, images: Vec<InlineImage>) -> ChatMessage {
    ChatMessage::Tool {
        tool_call_id: Some(id.to_string()),
        content: PromptText::json(&serde_json::json!({})),
        images,
    }
}

#[test]
fn sends_tool_result_images_in_one_user_message_after_the_tool_results() {
    let sent = request_json(&[
        ChatMessage::System("s".to_string()),
        user("u", "2026-09-22T04:12:00Z"),
        ChatMessage::Assistant {
            content: None,
            tool_calls: vec![
                tool_call("call_1"),
                tool_call("call_2"),
                tool_call("call_3"),
            ],
            replay: Default::default(),
        },
        tool_result("call_1", vec![png()]),
        tool_result("call_2", Vec::new()),
        tool_result("call_3", vec![png()]),
    ]);

    assert_eq!(
        roles(&sent),
        vec![
            "system",
            "user",
            "assistant",
            "tool",
            "tool",
            "tool",
            "user"
        ]
    );
    // toolロールには画像を載せない(拒むAPIがある)。
    for tool in &sent[3..6] {
        assert!(tool["content"].is_string());
    }
    let parts = sent[6]["content"].as_array().unwrap();
    assert_eq!(parts[0]["text"], TOOL_IMAGES_TEXT);
    assert_eq!(parts.len(), 3);
    assert!(parts[1..].iter().all(|p| p["type"] == "image_url"));
}

#[test]
fn sends_tool_result_images_before_the_next_round() {
    let sent = request_json(&[
        ChatMessage::System("s".to_string()),
        user("u", "2026-09-22T04:12:00Z"),
        ChatMessage::Assistant {
            content: None,
            tool_calls: vec![tool_call("call_1")],
            replay: Default::default(),
        },
        tool_result("call_1", vec![png()]),
        ChatMessage::Assistant {
            content: None,
            tool_calls: vec![tool_call("call_2")],
            replay: Default::default(),
        },
        tool_result("call_2", Vec::new()),
    ]);

    assert_eq!(
        roles(&sent),
        vec![
            "system",
            "user",
            "assistant",
            "tool",
            "user",
            "assistant",
            "tool"
        ]
    );
}

#[test]
fn merges_assistant_text_into_a_following_tool_call() {
    let sent = request_json(&[
        ChatMessage::System("s".to_string()),
        user("u", "2026-09-22T04:12:00Z"),
        assistant("a"),
        ChatMessage::Assistant {
            content: None,
            tool_calls: vec![tool_call("call_1")],
            replay: Default::default(),
        },
        ChatMessage::Tool {
            tool_call_id: Some("call_1".to_string()),
            content: PromptText::json(&serde_json::json!({})),
            images: Vec::new(),
        },
    ]);

    assert_eq!(roles(&sent), vec!["system", "user", "assistant", "tool"]);
    assert_eq!(sent[2]["content"], "a");
    assert_eq!(sent[2]["tool_calls"][0]["id"], "call_1");
}

#[test]
fn leaves_tool_round_trips_as_they_are() {
    let round_trip = |id: &str| {
        [
            ChatMessage::Assistant {
                content: None,
                tool_calls: vec![tool_call(id)],
                replay: Default::default(),
            },
            ChatMessage::Tool {
                tool_call_id: Some(id.to_string()),
                content: PromptText::json(&serde_json::json!({})),
                images: Vec::new(),
            },
        ]
    };
    let mut messages = vec![
        ChatMessage::System("s".to_string()),
        user("u", "2026-09-22T04:12:00Z"),
    ];
    messages.extend(round_trip("call_1"));
    messages.extend(round_trip("call_2"));

    assert_eq!(
        roles(&request_json(&messages)),
        vec!["system", "user", "assistant", "tool", "assistant", "tool"]
    );
}

#[test]
fn serializes_assistant_tool_calls_with_json_encoded_arguments() {
    let assistant = serde_json::to_value(to_request_message(&ChatMessage::Assistant {
        content: None,
        tool_calls: vec![ToolCallRequest {
            id: Some("call_1".to_string()),
            name: "add_steps".to_string(),
            arguments: serde_json::json!({ "descriptions": ["買い出し"] }).into(),
        }],
        replay: Default::default(),
    }))
    .unwrap();

    assert_eq!(
        assistant,
        serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "add_steps",
                    "arguments": "{\"descriptions\":[\"買い出し\"]}"
                }
            }]
        })
    );
}

#[test]
fn echoes_malformed_tool_arguments_back_verbatim() {
    let assistant = serde_json::to_value(to_request_message(&ChatMessage::Assistant {
        content: None,
        tool_calls: vec![ToolCallRequest {
            id: Some("call_1".to_string()),
            name: "update_task".to_string(),
            arguments: ToolArguments::parse("{\"title\": ".to_string()),
        }],
        replay: Default::default(),
    }))
    .unwrap();

    assert_eq!(
        assistant["tool_calls"][0]["function"]["arguments"],
        "{\"title\": "
    );
}

#[test]
fn deserializes_reasoning_content_extension_when_present() {
    let parsed: ResponseMessage = serde_json::from_value(serde_json::json!({
        "content": "答え",
        "reasoning_content": "考え中…"
    }))
    .unwrap();
    assert_eq!(parsed.content.as_deref(), Some("答え"));
    assert_eq!(parsed.reasoning_content.as_deref(), Some("考え中…"));
}

#[test]
fn reasoning_content_defaults_to_none_when_absent() {
    let parsed: ResponseMessage = serde_json::from_value(serde_json::json!({
        "content": "答え"
    }))
    .unwrap();
    assert_eq!(parsed.reasoning_content, None);
}

#[test]
fn serializes_tool_response_and_omits_missing_tool_call_id() {
    let with_id = serde_json::to_value(to_request_message(&ChatMessage::Tool {
        tool_call_id: Some("call_1".to_string()),
        content: PromptText::json(&serde_json::json!({})),
        images: Vec::new(),
    }))
    .unwrap();
    assert_eq!(
        with_id,
        serde_json::json!({"role": "tool", "tool_call_id": "call_1", "content": "{}"})
    );

    // 呼び出しIDを払い出さないプロバイダー向け: 捏造せずフィールドごと省略する。
    let without_id = serde_json::to_value(to_request_message(&ChatMessage::Tool {
        tool_call_id: None,
        content: PromptText::json(&serde_json::json!({})),
        images: Vec::new(),
    }))
    .unwrap();
    assert_eq!(
        without_id,
        serde_json::json!({"role": "tool", "content": "{}"})
    );
}

const EVENT_STREAM_HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nConnection: close\r\n\r\n";

/// 1回だけ接続を受け、SSEの応答を`pieces`の順に、それぞれ前に`delay`だけ待ってから書いて
/// 閉じる。受け取ったリクエストの本文を返す。
fn spawn_streaming(
    pieces: Vec<(Duration, &'static [u8])>,
) -> (String, std::thread::JoinHandle<serde_json::Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut raw = Vec::new();
        let mut buf = [0u8; 8192];
        let header_end = loop {
            let n = stream.read(&mut buf).unwrap();
            raw.extend_from_slice(&buf[..n]);
            if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let headers = String::from_utf8_lossy(&raw[..header_end]).to_ascii_lowercase();
        let length = headers
            .lines()
            .find_map(|l| l.strip_prefix("content-length: "))
            .map_or(0, |v| v.trim().parse::<usize>().unwrap());
        while raw.len() < header_end + length {
            let n = stream.read(&mut buf).unwrap();
            raw.extend_from_slice(&buf[..n]);
        }
        let body = serde_json::from_slice(&raw[header_end..header_end + length]).unwrap();
        let _ = stream.write_all(EVENT_STREAM_HEAD.as_bytes());
        for (delay, piece) in pieces {
            std::thread::sleep(delay);
            if stream.write_all(piece).is_err() {
                break;
            }
        }
        body
    });
    (format!("http://{addr}/v1"), handle)
}

/// 間を空けずに書く断片。
fn now(pieces: &[&'static str]) -> Vec<(Duration, &'static [u8])> {
    pieces
        .iter()
        .map(|p| (Duration::ZERO, p.as_bytes()))
        .collect()
}

/// 間を空けて書く断片。
fn after(step: Duration, pieces: &[&'static str]) -> Vec<(Duration, &'static [u8])> {
    pieces.iter().map(|p| (step, p.as_bytes())).collect()
}

async fn send_streamed(
    pieces: Vec<(Duration, &'static [u8])>,
    timeout: Duration,
) -> (
    Result<Replay, CoreError>,
    Vec<ResponseEvent>,
    serde_json::Value,
) {
    let (base_url, handle) = spawn_streaming(pieces);
    let adapter = OpenAiCompatAdapter::new(
        base_url,
        Credentials::key_only(SecretString::from("")),
        "model",
        timeout,
    )
    .unwrap();
    let mut events = Vec::new();
    let result = adapter
        .send(None, &[], ToolOffer::NONE, None, &mut |e| events.push(e))
        .await;
    (result, events, handle.join().unwrap())
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

fn done(finish_reason: FinishReason) -> ResponseEvent {
    ResponseEvent::Done { finish_reason }
}

#[tokio::test]
async fn asks_for_a_stream_and_passes_deltas_in_the_order_they_arrive() {
    let (result, events, body) = send_streamed(
        [
            now(&[
                ": keep-alive\n\n",
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"考え\"}}]}\n\n",
                // 行の途中で切れても、繋いで読む。
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"中\"}}]}\n\ndata: {\"choi",
            ]),
            // 「え」の途中で切れても、繋いで読む。
            vec![
                (
                    Duration::from_millis(20),
                    &b"ces\":[{\"delta\":{\"content\":\"\xE7\xAD\x94\xE3\x81"[..],
                ),
                (Duration::from_millis(20), &b"\x88\"}}]}\n\n"[..]),
            ],
            now(&[
                "data: {\"choices\":[{\"delta\":{\"content\":\"です\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                // 使用量だけのイベントは読み飛ばす。
                "data: {\"choices\":[],\"usage\":{\"total_tokens\":3}}\n\n",
                "data: [DONE]\n\n",
            ]),
        ]
        .concat(),
        TEST_TIMEOUT,
    )
    .await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(body["stream"], true);
    assert_eq!(
        events,
        vec![
            reasoning("考え"),
            reasoning("中"),
            text("答え"),
            text("です"),
            done(FinishReason::Stop),
        ]
    );
}

#[tokio::test]
async fn assembles_tool_call_fragments_by_index() {
    let (result, events, _) = send_streamed(
        now(&[
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"list_tasks\",\"arguments\":\"\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"function\":{\"name\":\"update_task\",\"arguments\":\"{\\\"ti\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"tle\\\": \\\"a\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ]),
        TEST_TIMEOUT,
    )
    .await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        events,
        vec![
            ResponseEvent::ToolCall {
                id: Some("call_a".to_string()),
                name: "list_tasks".to_string(),
                arguments: serde_json::json!({}).into(),
            },
            ResponseEvent::ToolCall {
                id: Some("call_b".to_string()),
                name: "update_task".to_string(),
                arguments: serde_json::json!({ "title": "a" }).into(),
            },
            done(FinishReason::ToolCall),
        ]
    );
}

#[tokio::test]
async fn assembles_tool_call_fragments_without_an_index() {
    let (result, events, _) = send_streamed(
        now(&[
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"name\":\"list_tasks\",\"arguments\":\"{\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_b\",\"function\":{\"name\":\"list_steps\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ]),
        TEST_TIMEOUT,
    )
    .await;

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        events,
        vec![
            ResponseEvent::ToolCall {
                id: None,
                name: "list_tasks".to_string(),
                arguments: serde_json::json!({}).into(),
            },
            ResponseEvent::ToolCall {
                id: Some("call_b".to_string()),
                name: "list_steps".to_string(),
                arguments: serde_json::json!({}).into(),
            },
            done(FinishReason::ToolCall),
        ]
    );
}

#[tokio::test]
async fn a_tool_call_without_a_name_is_an_invalid_response() {
    let (result, _, _) = send_streamed(
        now(&[
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ]),
        TEST_TIMEOUT,
    )
    .await;
    assert!(
        matches!(result, Err(CoreError::Llm(LlmError::InvalidResponse(_)))),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_finish_reason_completes_the_stream_without_the_done_marker() {
    let (result, events, _) = send_streamed(
        now(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"length\"}]}",
        ]),
        TEST_TIMEOUT,
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(events, vec![text("ok"), done(FinishReason::Length)]);
}

#[tokio::test]
async fn a_stream_cut_before_the_end_is_an_invalid_response() {
    let (result, events, _) = send_streamed(
        now(&["data: {\"choices\":[{\"delta\":{\"content\":\"途中\"}}]}\n\n"]),
        TEST_TIMEOUT,
    )
    .await;
    assert!(
        matches!(result, Err(CoreError::Llm(LlmError::InvalidResponse(_)))),
        "{result:?}"
    );
    // 流れた断片は画面に出たままになるが、`Err`なので呼び出し側は保存しない。
    assert_eq!(events, vec![text("途中")]);
}

#[tokio::test]
async fn a_stream_with_no_reply_is_an_empty_response() {
    let (result, events, _) = send_streamed(now(&["data: [DONE]\n\n"]), TEST_TIMEOUT).await;
    assert!(
        matches!(result, Err(CoreError::Llm(LlmError::EmptyResponse))),
        "{result:?}"
    );
    assert!(events.is_empty());
}

#[tokio::test]
async fn an_error_in_the_stream_is_classified_from_its_body() {
    let (result, _, _) = send_streamed(
        now(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
            "data: {\"error\":{\"message\":\"the request exceeds the available context size\",\"type\":\"exceed_context_size_error\"}}\n\n",
        ]),
        TEST_TIMEOUT,
    )
    .await;
    assert!(
        matches!(result, Err(CoreError::Llm(LlmError::ContextExceeded(_)))),
        "{result:?}"
    );

    let (result, _, _) = send_streamed(
        now(&["data: {\"error\":{\"message\":\"upstream failed\"}}\n\n"]),
        TEST_TIMEOUT,
    )
    .await;
    assert!(
        matches!(result, Err(CoreError::Llm(LlmError::Http(_)))),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_content_filter_stop_in_the_stream_is_a_refusal() {
    let (result, events, _) = send_streamed(
        now(&[
            "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"content_filter\"}]}\n\n",
            "data: [DONE]\n\n",
        ]),
        TEST_TIMEOUT,
    )
    .await;
    assert!(
        matches!(result, Err(CoreError::Llm(LlmError::Refused(_)))),
        "{result:?}"
    );
    // 断られたと分かるのは最後なので、本文は流れたあと。`Done`は渡さない。
    assert_eq!(events, vec![text("partial")]);
}

#[tokio::test]
async fn a_stream_that_stops_arriving_times_out() {
    let (result, events, _) = send_streamed(
        [
            now(&["data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n"]),
            after(Duration::from_millis(1500), &["data: [DONE]\n\n"]),
        ]
        .concat(),
        Duration::from_millis(300),
    )
    .await;
    assert!(
        matches!(result, Err(CoreError::Llm(LlmError::Timeout(_)))),
        "{result:?}"
    );
    assert_eq!(events, vec![text("a")]);
}

#[tokio::test]
async fn a_stream_that_keeps_arriving_outlasts_the_timeout() {
    let step = Duration::from_millis(150);
    let (result, events, _) = send_streamed(
        after(
            step,
            &[
                "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"c\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            ],
        ),
        Duration::from_millis(400),
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        events,
        vec![text("a"), text("b"), text("c"), done(FinishReason::Stop)]
    );
}
