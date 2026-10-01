//! バイナリを実際に起動し、GUIと同じDBへの書き込みと端末への出力を確かめる。

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

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_scitl-cli"))
            .arg("--data-dir")
            .arg(self.path())
            .args(args)
            .output()
            .unwrap()
    }
}

fn stdout_json(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn operations_are_recorded_with_the_cli_source() {
    let data = DataDir::new();
    let task_id = {
        let conn = db::open(DataLayout::new(data.path()).database()).unwrap();
        db::tasks::create_task(&conn).unwrap().id
    };

    let renamed = data.run(&["task", "rename", &task_id.to_string(), "買い出し"]);
    assert!(renamed.status.success(), "{renamed:?}");
    assert!(renamed.stdout.is_empty());

    let messages = stdout_json(&data.run(&["chat", "show", "--task", &task_id.to_string()]));
    assert_eq!(messages[0]["source"], "cli");
    let detail = stdout_json(&data.run(&["task", "show", &task_id.to_string()]));
    assert_eq!(detail["task"]["title"], "買い出し");
}

#[test]
fn invisible_characters_reach_the_terminal_escaped_with_the_same_value() {
    let data = DataDir::new();
    let task_id = {
        let conn = db::open(DataLayout::new(data.path()).database()).unwrap();
        db::tasks::create_task(&conn).unwrap().id
    };
    let title = "ab\u{202E}\u{9B}c";

    data.run(&["task", "rename", &task_id.to_string(), title]);
    // 操作の記録は引数をそのまま残す(タイトルの列は整形後の値)。
    let output = data.run(&["chat", "show", "--task", &task_id.to_string()]);

    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert!(
        !stdout.contains('\u{202E}') && !stdout.contains('\u{9B}'),
        "{stdout}"
    );
    let record: serde_json::Value =
        serde_json::from_str(stdout_json(&output)[0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(record["arguments"]["title"], title);
}

#[test]
fn a_missing_data_directory_is_not_created() {
    let data = DataDir::new();
    let missing = data.path().join("missing");

    let output = Command::new(env!("CARGO_BIN_EXE_scitl-cli"))
        .arg("--data-dir")
        .arg(&missing)
        .args(["task", "list"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!missing.exists());
}

#[test]
fn a_failed_operation_exits_with_failure_and_writes_to_stderr() {
    let data = DataDir::new();

    let output = data.run(&["task", "show", "1"]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("task 1 not found"));
}

#[test]
fn argument_errors_reach_the_terminal_escaped() {
    let data = DataDir::new();

    let output = data.run(&["task", "show", "1\u{1B}[31m"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        !stderr.contains('\u{1B}') && stderr.contains("\\u001B"),
        "{stderr}"
    );
}

/// 応答生成・設定に触れるコマンドはscitl-debug-cliだけが持つ。
#[test]
fn commands_beyond_tasks_and_showing_chats_are_not_offered() {
    let data = DataDir::new();

    for args in [
        &["chat", "preview"][..],
        &["chat", "send", "x"],
        &["settings", "show"],
    ] {
        let output = data.run(args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
}
