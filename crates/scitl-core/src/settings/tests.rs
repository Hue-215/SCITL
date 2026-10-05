use secrecy::{ExposeSecret, SecretString};

use super::*;
use crate::attachments::{AttachmentStore, Delivery};
use crate::config::{ApiFormat, Capability, ReasoningEffort};
use crate::llm;
use crate::mcp;
use crate::orchestration::discard_events;
use crate::tools::external;

// 鍵を渡さない操作だけを試す(資格情報ストアに触れない)。

/// 3つ目は設定ファイルを置いた一時ディレクトリ。落とすと消えるので、テストの間は持っておく。
fn temp_settings() -> (Settings, PathBuf, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    (Settings::load(path.clone()), path, dir)
}

fn ready_adapter(settings: &Settings) -> SharedAdapter {
    match settings.current().adapter {
        AdapterState::Ready(adapter) => adapter,
        AdapterState::NoProvider | AdapterState::KeyUnavailable(_) | AdapterState::Broken(_) => {
            panic!("adapter is not ready")
        }
    }
}

fn add_local_provider(settings: &Settings, name: &str) -> SettingsView {
    settings
        .add_provider(NewProvider {
            name: name.to_string(),
            api_format: ApiFormat::OpenAiCompat,
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: None,
        })
        .unwrap()
}

/// 秘密情報を持たないので、登録しても資格情報ストアに触れない。
fn http_endpoint() -> NewMcpEndpoint {
    NewMcpEndpoint::StreamableHttp {
        url: "http://127.0.0.1:8000/mcp".to_string(),
        headers: Vec::new(),
    }
}

/// 空白だけの鍵・ヘッダーに載せられない鍵は、資格情報ストアに触れる前に断る。
#[test]
fn add_provider_refuses_a_key_that_is_not_visible_ascii() {
    let (settings, _path, _dir) = temp_settings();
    for key in ["   ", "sk-test\n"] {
        let result = settings.add_provider(NewProvider {
            name: "remote".to_string(),
            api_format: ApiFormat::OpenAiCompat,
            base_url: "http://localhost:1234/v1".to_string(),
            api_key: Some(SecretString::from(key)),
        });
        assert!(
            matches!(result, Err(CoreError::ProviderConfig(_))),
            "{key:?}"
        );
    }
    assert!(settings.view().providers.is_empty());
}

fn make_key_unavailable(settings: &Settings) {
    settings.current.lock().unwrap().adapter = AdapterState::KeyUnavailable("locked".into());
}

/// 鍵を読めない間は送らずに失敗し、理由は設定画面のアクティブなプロバイダーに出す。
/// ターンの開始で読み直す(ここでは鍵を登録していないプロバイダーなので、読み直せば
/// 組み立てられる)。
#[tokio::test]
async fn an_unreadable_key_fails_the_turn_and_is_reloaded_at_the_next_turn() {
    let (settings, _path, _dir) = temp_settings();
    add_local_provider(&settings, "local");
    make_key_unavailable(&settings);

    assert!(matches!(
        settings.snapshot().adapter,
        Err(TurnFailure::KeyUnavailable)
    ));
    let view = settings.view();
    assert_eq!(view.providers[0].key_error.as_deref(), Some("locked"));
    assert_eq!(view.providers[0].error, None);

    assert!(settings.snapshot_for_turn().await.adapter.is_ok());
    assert_eq!(settings.view().providers[0].key_error, None);
}

/// タスクの追加のように、ターンを始めずに使えるかだけを確かめる入口でも読み直す。
#[tokio::test]
async fn checking_availability_reloads_an_unreadable_key() {
    let (settings, _path, _dir) = temp_settings();
    add_local_provider(&settings, "local");
    make_key_unavailable(&settings);

    assert!(settings.snapshot_reloading_key().await.adapter.is_ok());
}

/// 鍵を読めない間も、鍵と無関係な設定は変えられ、変えたときに読み直す。
#[test]
fn a_settings_change_reloads_an_unreadable_key() {
    let (settings, _path, _dir) = temp_settings();
    add_local_provider(&settings, "local");
    make_key_unavailable(&settings);

    settings.update_tools(Some(3), None).unwrap();
    ready_adapter(&settings);
}

/// 思考に対応するモデルには強さを必ず送り、対応しないモデルには送らない。
#[test]
fn reasoning_effort_is_sent_only_to_models_that_think() {
    let (_, path, _dir) = temp_settings();
    let config_with = |model_lines: &str| {
        format!(
            r#"
active_provider_id = "p"

[[providers]]
id = "p"
name = "Local"
api_format = "open_ai_compat"
base_url = "http://localhost:1234/v1"

[[providers.models]]
name = "m"
{model_lines}
"#
        )
    };
    let effort_for = |model_lines: &str| {
        std::fs::write(&path, config_with(model_lines)).unwrap();
        let settings = Settings::load(path.clone());
        let generating = InFlightSet::new();
        // このテストは添付を使わないので、置き場所は作られない。
        let unused = path.with_file_name("attachments");
        let attachments = Attachments::new(AttachmentStore::new(
            unused.join("blobs"),
            unused.join("revealed"),
        ));
        settings
            .snapshot()
            .turn_context(&generating, &attachments, &discard_events)
            .reasoning_effort
    };

    let low = r#"reasoning_effort = "low""#;
    assert_eq!(effort_for(low), Some(ReasoningEffort::Low));
    let without_thinking = format!("{low}\n[providers.models.overrides]\nthinking = false");
    assert_eq!(effort_for(&without_thinking), None);
    // まだ選んでいないモデルでも、サーバーの既定には任せない。
    assert_eq!(effort_for(""), Some(ReasoningEffort::default()));
}

#[test]
fn chat_model_selection_switches_provider_and_model_together() {
    let (settings, path, _dir) = temp_settings();
    let a = add_local_provider(&settings, "A").providers[0].id.clone();
    let b = add_local_provider(&settings, "B").providers[1].id.clone();
    settings.add_models(&a, &["a1"]).unwrap();
    settings.add_models(&b, &["b1"]).unwrap();
    settings.add_models(&b, &["b2"]).unwrap();

    settings.select_chat_model(&b, "b2").unwrap();
    let reloaded = config::load(&path).unwrap();
    let (provider, model) = reloaded.active_model().unwrap();
    assert_eq!(
        (provider.id.as_str(), model.name.as_str()),
        (b.as_str(), "b2")
    );

    assert!(settings.select_chat_model(&a, "b1").is_err());
    assert_eq!(
        settings.current().config.active_provider_id.as_deref(),
        Some(b.as_str()),
        "失敗した選択は何も変えない"
    );
}

#[test]
fn chat_models_tell_the_deliveries_that_do_not_depend_on_the_model_before_one_is_selected() {
    let (settings, _, _dir) = temp_settings();
    add_local_provider(&settings, "A");
    let listed = view::chat_models(&settings.current().config, &settings.detected);
    assert!(listed.selected.is_none());
    assert_eq!(listed.attachments.text, Some(Delivery::Content));
    assert_eq!(listed.attachments.other, Some(Delivery::NameOnly));
    // 画像はモデルが画像を読めるかで変わる。
    assert_eq!(listed.attachments.image, None);
}

#[test]
fn chat_models_list_visible_models_and_the_selected_one_even_if_hidden() {
    let (settings, _, _dir) = temp_settings();
    let a = add_local_provider(&settings, "A").providers[0].id.clone();
    let b = add_local_provider(&settings, "B").providers[1].id.clone();
    settings.add_models(&a, &["qwen3:8b"]).unwrap();
    settings.add_models(&a, &["hidden"]).unwrap();
    settings.add_models(&b, &["qwen2.5:7b"]).unwrap();
    settings
        .set_model_capability(&b, "qwen2.5:7b", Capability::Thinking, false)
        .unwrap();
    settings.set_model_visible(&a, "hidden", false).unwrap();
    settings
        .set_reasoning_effort(&a, "qwen3:8b", ReasoningEffort::High)
        .unwrap();

    let chat_models =
        |settings: &Settings| view::chat_models(&settings.current().config, &settings.detected);
    let listed = chat_models(&settings);
    let names: Vec<_> = listed.choices.iter().map(|c| c.model.as_str()).collect();
    assert_eq!(names, ["qwen3:8b", "qwen2.5:7b"]);
    let selected = listed.selected.unwrap();
    assert_eq!(selected.choice.model, "qwen3:8b");
    assert!(selected.thinking);
    assert_eq!(selected.reasoning_effort, ReasoningEffort::High);

    settings.select_chat_model(&a, "hidden").unwrap();
    assert_eq!(
        chat_models(&settings).selected.unwrap().choice.model,
        "hidden"
    );

    settings.select_chat_model(&b, "qwen2.5:7b").unwrap();
    let listed = chat_models(&settings);
    let selected = listed.selected.unwrap();
    assert_eq!(selected.choice.provider_name, "B");
    assert!(!selected.thinking);
    // 既定では画像に対応しないので、画像の添付は名前だけになる。
    assert_eq!(listed.attachments.image, Some(Delivery::NameOnly));
    assert_eq!(listed.attachments.text, Some(Delivery::Content));
    assert_eq!(listed.attachments.other, Some(Delivery::NameOnly));

    settings
        .set_model_capability(&b, "qwen2.5:7b", Capability::Image, true)
        .unwrap();
    let listed = chat_models(&settings);
    assert_eq!(listed.attachments.image, Some(Delivery::Image));
    assert_eq!(listed.attachments.other, Some(Delivery::NameOnly));
}

#[test]
fn first_provider_and_model_become_active_and_are_saved() {
    let (settings, path, _dir) = temp_settings();
    let view = add_local_provider(&settings, "Local");
    let id = view.providers[0].id.clone();
    assert_eq!(view.active_provider_id.as_deref(), Some(id.as_str()));
    assert!(settings.snapshot().adapter.is_ok());

    let view = settings.add_models(&id, &[" m1 "]).unwrap();
    assert_eq!(view.providers[0].active_model.as_deref(), Some("m1"));
    let view = settings.add_models(&id, &["m2"]).unwrap();
    assert_eq!(
        view.providers[0].active_model.as_deref(),
        Some("m1"),
        "2つ目のモデルでアクティブは変わらない"
    );

    let reloaded = config::load(&path).unwrap();
    let names: Vec<_> = reloaded.providers[0]
        .models
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(names, ["m1", "m2"]);
}

#[test]
fn adding_several_models_registers_all_or_none() {
    let (settings, _, _dir) = temp_settings();
    let id = add_local_provider(&settings, "Local").providers[0]
        .id
        .clone();

    let view = settings.add_models(&id, &["b", "a"]).unwrap();
    let names: Vec<_> = view.providers[0]
        .models
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(names, ["b", "a"]);
    assert_eq!(
        view.providers[0].active_model.as_deref(),
        Some("b"),
        "モデルが無かったプロバイダーでは最初の1件がアクティブになる"
    );

    for rejected in [&["c", "a"][..], &["c", "c"], &["c", " "]] {
        assert!(settings.add_models(&id, rejected).is_err());
        let config = settings.current().config;
        assert!(
            config.providers[0].model("c").is_none(),
            "{rejected:?}: 登録できない名前が混ざれば1件も登録しない"
        );
    }
}

#[test]
fn deleting_active_provider_activates_first_remaining() {
    let (settings, _, _dir) = temp_settings();
    let first = add_local_provider(&settings, "A").providers[0].id.clone();
    let second = add_local_provider(&settings, "B").providers[1].id.clone();

    let view = settings.delete_provider(&first).unwrap();
    assert_eq!(view.active_provider_id.as_deref(), Some(second.as_str()));
    let view = settings.delete_provider(&second).unwrap();
    assert_eq!(view.active_provider_id, None);
    assert!(matches!(
        settings.snapshot().adapter,
        Err(TurnFailure::NoProvider)
    ));
}

/// 選択中のプロバイダーやその最後のモデルを消しても、モデルのあるプロバイダーが
/// 残っていればチャットを続けられる。
#[test]
fn selection_moves_to_a_provider_that_has_a_model() {
    let (settings, _, _dir) = temp_settings();
    let ids: Vec<String> = ["A", "B", "C"]
        .iter()
        .map(|name| {
            let view = add_local_provider(&settings, name);
            view.providers.last().unwrap().id.clone()
        })
        .collect();
    let active = || settings.current().config.active_provider_id.clone();
    assert_eq!(active().as_deref(), Some(ids[0].as_str()));

    // モデルの無いプロバイダーを選択中に、別のプロバイダーへモデルを登録する。
    settings.add_models(&ids[2], &["c1"]).unwrap();
    assert_eq!(active().as_deref(), Some(ids[2].as_str()));

    // 最後のモデルを消す。ほかにモデルが無ければ、選択は動かさない。
    settings.remove_model(&ids[2], "c1").unwrap();
    assert_eq!(active().as_deref(), Some(ids[2].as_str()));

    // 選択中のプロバイダーを消す。先頭のAにはモデルが無いので、Bへ移る。
    settings.add_models(&ids[1], &["b1"]).unwrap();
    settings.select_chat_model(&ids[1], "b1").unwrap();
    settings.add_models(&ids[2], &["c1"]).unwrap();
    settings.select_chat_model(&ids[2], "c1").unwrap();
    let view = settings.delete_provider(&ids[2]).unwrap();
    assert_eq!(view.active_provider_id.as_deref(), Some(ids[1].as_str()));

    // 選択中でないプロバイダーを消しても、選択は動かさない。
    let view = settings.delete_provider(&ids[0]).unwrap();
    assert_eq!(view.active_provider_id.as_deref(), Some(ids[1].as_str()));

    let view = settings.remove_model(&ids[1], "b1").unwrap();
    assert_eq!(view.active_provider_id.as_deref(), Some(ids[1].as_str()));
}

#[test]
fn removing_active_model_falls_back_to_first() {
    let (settings, _, _dir) = temp_settings();
    let id = add_local_provider(&settings, "A").providers[0].id.clone();
    settings.add_models(&id, &["m1"]).unwrap();
    settings.add_models(&id, &["m2"]).unwrap();
    settings.select_chat_model(&id, "m2").unwrap();

    let view = settings.remove_model(&id, "m2").unwrap();
    assert_eq!(view.providers[0].active_model.as_deref(), Some("m1"));
}

#[test]
fn rejects_limits_and_timeouts_out_of_range() {
    let (settings, _, _dir) = temp_settings();
    for secs in [0, input::MAX_TIMEOUT_SECS + 1, u64::MAX] {
        assert!(settings
            .update_general(general_update(None, Some(secs)))
            .is_err());
        assert!(settings.update_tools(None, Some(secs)).is_err());
    }
    for rounds in [0, input::MAX_ROUNDS_PER_TURN + 1] {
        assert!(settings.update_tools(Some(rounds), None).is_err());
    }
    settings
        .update_general(general_update(None, Some(input::MAX_TIMEOUT_SECS)))
        .unwrap();
    settings
        .update_tools(
            Some(input::MAX_ROUNDS_PER_TURN),
            Some(input::MAX_TIMEOUT_SECS),
        )
        .unwrap();
}

#[test]
fn provider_name_must_be_visible_and_unique_and_the_url_is_trimmed() {
    let (settings, _, _dir) = temp_settings();
    let add = |name: &str, base_url: &str| {
        settings.add_provider(NewProvider {
            name: name.to_string(),
            api_format: ApiFormat::OpenAiCompat,
            base_url: base_url.to_string(),
            api_key: None,
        })
    };
    let view = add(" Local ", " http://localhost:1234/v1 \n").unwrap();
    assert_eq!(view.providers[0].name, "Local");
    assert_eq!(view.providers[0].base_url, "http://localhost:1234/v1");

    let long = "a".repeat(input::PROVIDER_NAME_MAX_CHARS + 1);
    for refused in ["Local", "Local ", "\u{200B}", "a\u{202E}b", long.as_str()] {
        let err = add(refused, "http://localhost:1234/v1").unwrap_err();
        assert!(matches!(err, CoreError::InvalidSettings(_)), "{refused:?}");
    }
    assert_eq!(settings.view().providers.len(), 1);
}

#[test]
fn model_name_must_be_visible() {
    let (settings, _, _dir) = temp_settings();
    let id = add_local_provider(&settings, "Local").providers[0]
        .id
        .clone();
    let long = "a".repeat(input::MODEL_NAME_MAX_CHARS + 1);
    for refused in ["\u{FEFF}", "m\u{1}", long.as_str()] {
        assert!(settings.add_models(&id, &[refused]).is_err(), "{refused:?}");
    }
    assert!(settings.view().providers[0].models.is_empty());
}

#[test]
fn mcp_endpoint_refuses_repeated_or_malformed_secret_names() {
    let (settings, _, _dir) = temp_settings();
    let pairs = |names: &[&str]| -> Vec<(String, SecretString)> {
        names
            .iter()
            .map(|n| (n.to_string(), SecretString::from("v")))
            .collect()
    };
    for headers in [&["X-Api-Key", "x-api-key"][..], &["A B"], &[""], &["Host"]] {
        let endpoint = NewMcpEndpoint::StreamableHttp {
            url: "https://example.com/mcp".to_string(),
            headers: pairs(headers),
        };
        assert!(
            settings.add_mcp_server("tools", endpoint).is_err(),
            "{headers:?}"
        );
    }
    assert!(settings.view().mcp_servers.is_empty());
}

fn general_update(
    system_prompt: Option<&str>,
    response_timeout_secs: Option<u64>,
) -> GeneralUpdate {
    GeneralUpdate {
        system_prompt: system_prompt.map(str::to_string),
        task_chat_system_prompt: None,
        task_opening_message: None,
        response_timeout_secs,
    }
}

#[test]
fn blank_prompts_and_prompts_equal_to_their_defaults_are_saved_as_unset() {
    let (settings, _, _dir) = temp_settings();
    let view = settings
        .update_general(GeneralUpdate {
            task_chat_system_prompt: Some(default_task_chat_prompt(Language::DEFAULT).to_string()),
            task_opening_message: Some(default_opening_message(Language::DEFAULT).to_string()),
            ..general_update(None, None)
        })
        .unwrap();
    assert!(view.general.task_chat_system_prompt.is_none());
    assert!(view.general.task_opening_message.is_none());

    let view = settings
        .update_general(GeneralUpdate {
            task_chat_system_prompt: Some("custom".to_string()),
            task_opening_message: Some(" \n".to_string()),
            ..general_update(Some("  "), None)
        })
        .unwrap();
    assert_eq!(
        view.general.task_chat_system_prompt.as_deref(),
        Some("custom")
    );
    assert!(view.general.task_opening_message.is_none());
    assert!(view.general.system_prompt.is_none());
}

#[test]
fn language_is_saved_apart_from_the_other_general_settings() {
    let (settings, path, _dir) = temp_settings();
    assert_eq!(settings.display_language(), Language::DEFAULT);

    settings.update_language(Language::En).unwrap();
    // プロンプト欄を保存しても、表示言語は変えない。
    let view = settings
        .update_general(general_update(Some("prompt"), None))
        .unwrap();
    assert_eq!(view.general.language, Language::En);
    assert_eq!(Settings::load(path).display_language(), Language::En);
}

/// 設定ファイルの知らない表示言語は画面へ伝え、選び直すと消える。
#[test]
fn unknown_language_is_reported_until_a_language_is_chosen() {
    let (_, path, _dir) = temp_settings();
    std::fs::write(&path, "[general]\nlanguage = \"j\u{202E}p\"\n").unwrap();
    let settings = Settings::load(path);

    let view = settings.view();
    assert_eq!(view.config_error, None);
    assert_eq!(view.general.language, Language::DEFAULT);
    assert_eq!(view.general.unknown_language.as_deref(), Some("jp"));

    let view = settings.update_language(Language::En).unwrap();
    assert_eq!(view.general.language, Language::En);
    assert_eq!(view.general.unknown_language, None);
}

#[test]
fn failed_save_leaves_current_settings_unchanged() {
    let (settings, path, _dir) = temp_settings();
    // 保存先をディレクトリにして書き込みを失敗させる。
    std::fs::create_dir_all(&path).unwrap();
    assert!(settings
        .update_general(general_update(Some("prompt"), None))
        .is_err());
    assert!(settings.current().config.general.system_prompt.is_none());
}

/// フォームが送る`[名前, 値]`の組のまま、値を`SecretString`として受け取れる。
#[test]
fn new_mcp_endpoint_reads_secret_values_from_ipc_pairs() {
    let endpoint: NewMcpEndpoint = serde_json::from_value(serde_json::json!({
        "transport": "streamable_http",
        "url": "https://example.com/mcp",
        "headers": [["Authorization", "Bearer token"]],
    }))
    .unwrap();
    let NewMcpEndpoint::StreamableHttp { headers, .. } = endpoint;
    assert_eq!(headers[0].0, "Authorization");
    assert_eq!(headers[0].1.expose_secret(), "Bearer token");
}

/// 子プロセスとして起動する方式(stdio)の登録は、画面からもCLIからも受け付けない。
#[test]
fn new_mcp_endpoint_refuses_stdio() {
    let endpoint = serde_json::from_value::<NewMcpEndpoint>(serde_json::json!({
        "transport": "stdio",
        "command": "npx",
    }));
    assert!(endpoint.is_err());
}

#[test]
fn mcp_server_name_must_be_unique() {
    let (settings, _, _dir) = temp_settings();
    settings.add_mcp_server("tools", http_endpoint()).unwrap();
    let err = settings
        .add_mcp_server("tools", http_endpoint())
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidSettings(_)));
}

/// 試すのをやめたサーバーも、有効にし直せば次のターンでまた試す。無効にしただけでは数え直さない。
#[test]
fn re_enabling_an_mcp_server_tries_it_again() {
    let (settings, _, _dir) = temp_settings();
    let view = settings.add_mcp_server("tools", http_endpoint()).unwrap();
    let id = view.mcp_servers[0].id.clone();
    while !settings.mcp_tools.record_failure(&id) {}

    settings.set_mcp_server_enabled(&id, false).unwrap();
    assert!(settings.mcp_tools.gave_up(&id));
    settings.set_mcp_server_enabled(&id, true).unwrap();
    assert!(!settings.mcp_tools.gave_up(&id));
}

#[test]
fn tools_whose_names_cannot_be_exposed_cannot_be_enabled() {
    let (settings, _, _dir) = temp_settings();
    let view = settings.add_mcp_server("tools", http_endpoint()).unwrap();
    let id = view.mcp_servers[0].id.clone();

    let err = settings
        .set_mcp_tool_enabled(&id, "read\u{202E}file", true)
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidSettings(_)));

    let view = settings
        .set_mcp_tool_enabled(&id, "read_file", true)
        .unwrap();
    assert_eq!(view.mcp_servers[0].enabled_tools, vec!["read_file"]);
    // 一覧が未取得でも、有効化済みのツールは説明なしで一覧に出る。
    let server = &view.mcp_servers[0];
    assert!(!server.tools_fetched);
    assert_eq!(server.tools.len(), 1);
    assert_eq!(server.tools[0].label, "read_file");
    assert!(server.tools[0].description.is_none());
}

#[test]
fn tools_whose_schema_cannot_be_exposed_cannot_be_enabled() {
    let (settings, _, _dir) = temp_settings();
    let view = settings.add_mcp_server("tools", http_endpoint()).unwrap();
    let id = view.mcp_servers[0].id.clone();
    let tool = |name: &str, input_schema: serde_json::Value| mcp::McpToolInfo {
        name: name.to_string(),
        description: None,
        input_schema,
    };
    settings.mcp_tools.store(
        &id,
        vec![
            tool("list", serde_json::json!({ "type": "array" })),
            tool("read", serde_json::json!({ "type": "object" })),
        ],
    );

    let view = settings.view();
    let exposable: Vec<_> = view.mcp_servers[0]
        .tools
        .iter()
        .map(|t| (t.name.as_str(), t.exposable))
        .collect();
    assert_eq!(exposable, vec![("list", false), ("read", true)]);
    let err = settings
        .set_mcp_tool_enabled(&id, "list", true)
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidSettings(_)));
    settings.set_mcp_tool_enabled(&id, "read", true).unwrap();
}

#[test]
fn enabling_more_external_tools_than_the_limit_is_refused() {
    let (settings, _, _dir) = temp_settings();
    let view = settings.add_mcp_server("tools", http_endpoint()).unwrap();
    let id = view.mcp_servers[0].id.clone();
    for i in 0..external::MAX_EXTERNAL_TOOLS {
        settings
            .set_mcp_tool_enabled(&id, &format!("t{i}"), true)
            .unwrap();
    }

    let err = settings
        .set_mcp_tool_enabled(&id, "one_more", true)
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidSettings(_)));
    // 有効化済みのツールをもう一度有効にする・無効にするのは断らない。
    settings.set_mcp_tool_enabled(&id, "t0", true).unwrap();
    settings.set_mcp_tool_enabled(&id, "t0", false).unwrap();
    settings
        .set_mcp_tool_enabled(&id, "one_more", true)
        .unwrap();
}

/// 設定ファイルを読めなくても起動し、理由を画面とターンへ渡す。読めなかったファイルは
/// 上書きしない。
#[test]
fn unreadable_config_file_starts_empty_and_refuses_to_save() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "providers = [").unwrap();

    let settings = Settings::load(path.clone());

    assert!(settings.view().config_error.is_some());
    assert!(matches!(
        settings.snapshot().adapter,
        Err(TurnFailure::SettingsUnreadable)
    ));
    let err = settings
        .update_general(general_update(Some("prompt"), None))
        .unwrap_err();
    assert!(matches!(err, CoreError::InvalidSettings(_)));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "providers = [");
}

/// アクティブなプロバイダーを組み立てられなくても起動し、そのプロバイダーを使えない
/// ものとして扱う。無関係な変更は通し、削除すれば直る。
#[test]
fn broken_active_provider_does_not_block_startup_or_unrelated_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    // ホスト名宛のhttpは検証で弾かれる(net::validate_external_url)。
    std::fs::write(
        &path,
        r#"
active_provider_id = "broken"

[[providers]]
id = "broken"
name = "Broken"
api_format = "open_ai_compat"
base_url = "http://example.com/v1"

[[providers.models]]
name = "m"
"#,
    )
    .unwrap();

    let settings = Settings::load(path);
    let view = settings.view();
    assert!(view.config_error.is_none());
    assert!(view.providers[0].error.is_some());
    assert!(matches!(
        settings.snapshot().adapter,
        Err(TurnFailure::ProviderConfig)
    ));

    // アダプタの入力を変えない変更は通る。
    settings.update_tools(Some(3), None).unwrap();
    // 使えるプロバイダーを足しても、アクティブは壊れたままなので状態は変わらない。
    add_local_provider(&settings, "Local");
    assert!(settings.view().providers[0].error.is_some());

    let view = settings.delete_provider("broken").unwrap();
    assert!(view.providers.iter().all(|p| p.error.is_none()));
    assert!(settings.snapshot().adapter.is_ok());
}

#[test]
fn adapter_is_rebuilt_only_when_its_inputs_change() {
    let (settings, _, _dir) = temp_settings();
    let id = add_local_provider(&settings, "A").providers[0].id.clone();
    let before = ready_adapter(&settings);

    settings.add_mcp_server("tools", http_endpoint()).unwrap();
    assert!(Arc::ptr_eq(&before, &ready_adapter(&settings)));

    settings.add_models(&id, &["m1"]).unwrap();
    let after_model = ready_adapter(&settings);
    assert!(!Arc::ptr_eq(&before, &after_model));

    // 表示と能力はアダプタの入力ではない。
    settings.set_model_visible(&id, "m1", false).unwrap();
    settings
        .set_model_capability(&id, "m1", Capability::Image, true)
        .unwrap();
    settings
        .set_model_context_length(&id, "m1", Some(4096))
        .unwrap();
    assert!(Arc::ptr_eq(&after_model, &ready_adapter(&settings)));
}

#[test]
fn capability_overrides_are_kept_only_while_they_differ_from_the_default() {
    let (settings, path, _dir) = temp_settings();
    let id = add_local_provider(&settings, "Local").providers[0]
        .id
        .clone();
    settings.add_models(&id, &["m"]).unwrap();
    let fallback = llm::DEFAULT_CAPABILITIES;

    let view = settings
        .set_model_capability(&id, "m", Capability::Image, !fallback.image)
        .unwrap();
    let model = &view.providers[0].models[0];
    assert_eq!(model.capabilities.image, !fallback.image);
    assert!(model.overridden);
    assert!(!model.lacks_tools, "検出していないモデルは警告しない");

    // 初期値と同じ値に戻したら、手動設定は残らない。
    let view = settings
        .set_model_capability(&id, "m", Capability::Image, fallback.image)
        .unwrap();
    assert!(!view.providers[0].models[0].overridden);

    settings
        .set_model_capability(&id, "m", Capability::Thinking, !fallback.thinking)
        .unwrap();
    let view = settings
        .set_model_context_length(&id, "m", Some(8192))
        .unwrap();
    let model = &view.providers[0].models[0];
    assert_eq!(model.capabilities.context_length, 8192);
    assert!(settings
        .set_model_context_length(&id, "m", Some(0))
        .is_err());

    let saved = &config::load(&path).unwrap().providers[0].models[0];
    assert_eq!(saved.overrides.thinking, Some(!fallback.thinking));
    assert_eq!(saved.overrides.context_length, Some(8192));

    settings.set_model_visible(&id, "m", false).unwrap();
    let view = settings.reset_model_capabilities(&id, "m").unwrap();
    let model = &view.providers[0].models[0];
    assert!(!model.overridden);
    assert_eq!(model.capabilities, fallback);
    assert!(!model.visible, "表示/非表示は能力ではないので戻さない");

    assert!(settings.set_model_visible(&id, "missing", true).is_err());
}

/// 自動検出の結果は、手動設定を外す基準と、ターンに渡す能力の両方に効く。
#[test]
fn detected_capabilities_are_the_layer_below_manual_settings() {
    let (settings, _path, _dir) = temp_settings();
    let id = add_local_provider(&settings, "Local").providers[0]
        .id
        .clone();
    settings.add_models(&id, &["m"]).unwrap();
    settings.detected.store(
        &id,
        "m",
        llm::DetectedCapabilities {
            image: Some(true),
            tools: Some(false),
            context_length: Some(16_384),
            ..Default::default()
        },
    );

    let view = settings.view();
    assert!(view.providers[0].can_detect_capabilities);
    let model = &view.providers[0].models[0];
    assert!(model.capabilities.image);
    assert_eq!(model.default_context_length, 16_384);
    assert!(!model.overridden);
    assert!(model.lacks_tools);
    assert!(settings.snapshot().capabilities.image);

    // 検出した値と同じにしたら手動設定は残らず、既定値と同じでも違えば残る。
    let view = settings
        .set_model_context_length(&id, "m", Some(16_384))
        .unwrap();
    assert!(!view.providers[0].models[0].overridden);
    let view = settings
        .set_model_capability(&id, "m", Capability::Image, false)
        .unwrap();
    assert!(view.providers[0].models[0].overridden);
    assert!(!settings.snapshot().capabilities.image);

    // モデルを消したら結果も捨てる。
    settings.remove_model(&id, "m").unwrap();
    assert!(settings.detected.get(&id, "m").is_none());
}
