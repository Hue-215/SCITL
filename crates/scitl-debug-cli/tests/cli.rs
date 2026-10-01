//! バイナリを実際に起動し、GUIと同じデータディレクトリへの読み書きと端末への出力を確かめる。
//! 資格情報ストアの無い環境でも走るよう、秘密情報を保存する経路は通さない。

use std::path::Path;
use std::process::{Command, Output};

use scitl_core::db;
use scitl_core::paths::DataLayout;

struct DataDir(tempfile::TempDir);

impl DataDir {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn layout(&self) -> DataLayout {
        DataLayout::new(self.path())
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_scitl-debug-cli"))
            .arg("--data-dir")
            .arg(self.path())
            .args(args)
            .output()
            .unwrap()
    }

    fn create_task(&self) -> i64 {
        let conn = db::open(self.layout().database()).unwrap();
        db::tasks::create_task(&conn).unwrap().id
    }
}

fn stdout_json(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// 1行に1つずつ書かれたJSON。
fn stdout_lines(output: &Output) -> Vec<serde_json::Value> {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_commands_of_scitl_cli_work_the_same() {
    let data = DataDir::new();
    let task = data.create_task().to_string();

    let renamed = data.run(&["task", "rename", &task, "買い出し"]);
    assert!(renamed.status.success(), "{renamed:?}");

    let messages = stdout_json(&data.run(&["chat", "show", "--task", &task]));
    assert_eq!(messages[0]["source"], "cli");
    assert_eq!(
        stdout_json(&data.run(&["task", "show", &task]))["task"]["title"],
        "買い出し"
    );
}

#[test]
fn preview_without_a_provider_reports_why_and_saves_nothing() {
    let data = DataDir::new();
    let task = data.create_task().to_string();

    let output = data.run(&[
        "chat",
        "preview",
        "--task",
        &task,
        "--message",
        "こんにちは",
    ]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).is_empty());
    let messages = stdout_json(&data.run(&["chat", "show", "--task", &task]));
    assert_eq!(messages, serde_json::json!([]));
}

#[test]
fn a_failed_turn_is_saved_and_printed_as_the_last_message() {
    let data = DataDir::new();

    let lines = stdout_lines(&data.run(&["chat", "send", "こんにちは"]));

    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["type"], "last_message");
    assert_eq!(lines[0]["message"]["error_kind"], "no_provider");
    let messages = stdout_json(&data.run(&["chat", "show"]));
    assert_eq!(messages[0]["content"], "こんにちは");
    assert_eq!(messages[1]["id"], lines[0]["message"]["id"]);
}

#[test]
fn retrying_a_reply_makes_another_attempt_of_the_same_turn() {
    let data = DataDir::new();
    let failed = stdout_lines(&data.run(&["chat", "send", "こんにちは"]));
    let reply_id = failed[0]["message"]["id"].to_string();

    let retried = stdout_lines(&data.run(&["chat", "retry", &reply_id]));

    let (first, second) = (&failed[0]["message"], &retried[0]["message"]);
    assert_eq!(second["turn_id"], first["turn_id"]);
    assert_eq!(second["attempt_no"], 2);
}

#[test]
fn a_task_is_not_created_while_the_chat_is_unavailable() {
    let data = DataDir::new();

    let output = data.run(&["task", "create"]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no_provider"));
    assert_eq!(
        stdout_json(&data.run(&["task", "list"])),
        serde_json::json!([])
    );
}

#[test]
fn sent_files_are_listed_and_only_unreferenced_stored_files_are_removed() {
    let data = DataDir::new();
    let memo = data.path().join("memo.txt");
    std::fs::write(&memo, "本文").unwrap();
    // UTF-8として読めないので「その他」になり、実体が置き場所に書かれる。
    let binary = data.path().join("blob.bin");
    std::fs::write(&binary, [0xFF, 0x00, 0xFE]).unwrap();

    let sent = data.run(&[
        "chat",
        "send",
        "--attach",
        memo.to_str().unwrap(),
        "--attach",
        binary.to_str().unwrap(),
    ]);
    assert!(sent.status.success(), "{sent:?}");

    let listed = stdout_json(&data.run(&["attachment", "list"]));
    assert_eq!(listed[0]["original_name"], "memo.txt");
    assert_eq!(listed[0]["file_hash"], serde_json::Value::Null);
    assert_eq!(listed[1]["original_name"], "blob.bin");
    let kept = data
        .layout()
        .attachments()
        .join(listed[1]["file_hash"].as_str().unwrap());
    assert!(kept.exists());

    let stray_hash = "b".repeat(64);
    let stray = data.layout().attachments().join(&stray_hash);
    std::fs::write(&stray, b"stray").unwrap();

    let orphans = stdout_json(&data.run(&["attachment", "orphans"]));
    assert_eq!(
        orphans,
        serde_json::json!([{"hash": stray_hash, "size_bytes": 5}])
    );
    assert!(stray.exists(), "--delete無しでは消さない");
    assert_eq!(
        stdout_json(&data.run(&["attachment", "orphans", "--delete"])),
        orphans
    );
    assert!(!stray.exists());
    assert!(kept.exists());
}

#[test]
fn a_file_that_cannot_be_read_sends_nothing() {
    let data = DataDir::new();
    let missing = data.path().join("missing.txt");

    let output = data.run(&[
        "chat",
        "send",
        "--attach",
        missing.to_str().unwrap(),
        "見て",
    ]);

    assert!(!output.status.success());
    assert_eq!(
        stdout_json(&data.run(&["chat", "show"])),
        serde_json::json!([])
    );
}

#[test]
fn export_writes_a_folder_under_the_data_directory() {
    let data = DataDir::new();
    data.create_task();

    let summary = stdout_json(&data.run(&["export"]));

    assert_eq!(summary["tasks"], 1);
    let folder = data
        .layout()
        .export()
        .join(summary["folder"].as_str().unwrap());
    assert!(folder.join("general-chat.md").exists());
}

#[test]
fn providers_and_models_are_registered_into_the_config_file() {
    let data = DataDir::new();

    let added = stdout_json(&data.run(&[
        "provider",
        "add",
        "--name",
        "local",
        "--api-format",
        "open_ai_compat",
        "--base-url",
        "http://127.0.0.1:18080/v1",
    ]));
    let provider = added["providers"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(added["providers"][0]["has_api_key"], false);

    stdout_json(&data.run(&["model", "add", &provider, "model-a", "model-b"]));
    stdout_json(&data.run(&["model", "select", &provider, "model-b"]));
    stdout_json(&data.run(&["settings", "general", "--response-timeout-secs", "300"]));

    // 起動のたびに設定ファイルから読み直すので、ここで見えるのは保存されたもの。
    let shown = stdout_json(&data.run(&["settings", "show"]));
    assert_eq!(shown["active_provider_id"], provider.as_str());
    assert_eq!(shown["providers"][0]["active_model"], "model-b");
    assert_eq!(shown["providers"][0]["models"].as_array().unwrap().len(), 2);
    assert_eq!(shown["general"]["response_timeout_secs"], 300);

    let removed = stdout_json(&data.run(&["provider", "delete", &provider]));
    assert_eq!(removed["providers"], serde_json::json!([]));
}

#[test]
fn values_are_spelled_as_in_the_config_file_and_others_are_refused() {
    let data = DataDir::new();

    let output = data.run(&["settings", "language", "fr"]);
    assert!(!output.status.success());

    let changed = stdout_json(&data.run(&["settings", "language", "en"]));
    assert_eq!(changed["general"]["language"], "en");
}

#[test]
fn mcp_servers_are_registered_changed_and_removed() {
    let data = DataDir::new();

    let added = stdout_json(&data.run(&["mcp", "add-stdio", "files", "npx", "-y", "server"]));
    let server = &added["mcp_servers"][0];
    assert_eq!(server["endpoint"]["command"], "npx");
    assert_eq!(
        server["endpoint"]["args"],
        serde_json::json!(["-y", "server"])
    );
    let id = server["id"].as_str().unwrap().to_string();

    let disabled = stdout_json(&data.run(&["mcp", "disable", &id]));
    assert_eq!(disabled["mcp_servers"][0]["enabled"], false);
    let offered = stdout_json(&data.run(&["mcp", "enable-tool", &id, "read"]));
    assert_eq!(
        offered["mcp_servers"][0]["enabled_tools"],
        serde_json::json!(["read"])
    );

    let removed = stdout_json(&data.run(&["mcp", "delete", &id]));
    assert_eq!(removed["mcp_servers"], serde_json::json!([]));
}

#[test]
fn a_secret_whose_variable_is_unset_registers_nothing() {
    let data = DataDir::new();

    let output = Command::new(env!("CARGO_BIN_EXE_scitl-debug-cli"))
        .arg("--data-dir")
        .arg(data.path())
        .args([
            "mcp",
            "add-stdio",
            "--env",
            "TOKEN=SCITL_TEST_UNSET",
            "files",
            "npx",
        ])
        .env_remove("SCITL_TEST_UNSET")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SCITL_TEST_UNSET"));
    let shown = stdout_json(&data.run(&["settings", "show"]));
    assert_eq!(shown["mcp_servers"], serde_json::json!([]));
}

/// 値そのものを名前の位置に書いても、端末へ書き戻さない。
#[test]
fn a_secret_written_in_place_of_a_variable_name_is_not_echoed() {
    let data = DataDir::new();

    for args in [
        &[
            "mcp",
            "add-http",
            "--header",
            "Authorization: Bearer s3cr3t-value",
            "web",
            "https://example.com/mcp",
        ][..],
        &[
            "mcp",
            "add-stdio",
            "--env",
            "TOKEN=s3cr3t-value",
            "files",
            "npx",
        ],
        &[
            "provider",
            "add",
            "--name",
            "p",
            "--api-format",
            "open_ai_compat",
            "--base-url",
            "http://127.0.0.1:1/v1",
            "--api-key-env",
            "s3cr3t-value",
        ],
    ] {
        let output = data.run(args);
        assert!(!output.status.success(), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("s3cr3t"), "{stderr}");
    }
    let shown = stdout_json(&data.run(&["settings", "show"]));
    assert_eq!(shown["mcp_servers"], serde_json::json!([]));
    assert_eq!(shown["providers"], serde_json::json!([]));
}

#[test]
fn an_empty_secret_is_refused() {
    let data = DataDir::new();

    let output = Command::new(env!("CARGO_BIN_EXE_scitl-debug-cli"))
        .arg("--data-dir")
        .arg(data.path())
        .args([
            "provider",
            "add",
            "--name",
            "p",
            "--api-format",
            "open_ai_compat",
        ])
        .args([
            "--base-url",
            "http://127.0.0.1:1/v1",
            "--api-key-env",
            "SCITL_TEST_EMPTY",
        ])
        .env("SCITL_TEST_EMPTY", "")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let shown = stdout_json(&data.run(&["settings", "show"]));
    assert_eq!(shown["providers"], serde_json::json!([]));
}
