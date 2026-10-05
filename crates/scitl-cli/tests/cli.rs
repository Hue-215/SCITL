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

/// 接続を閉じて終わるので、SQLiteがWALを本体へ書き戻して消す。
#[test]
fn a_command_leaves_no_wal_behind() {
    let data = DataDir::new();

    assert!(data.run(&["task", "list"]).status.success());

    let database = DataLayout::new(data.path()).database();
    assert!(database.exists());
    let mut wal = database.into_os_string();
    wal.push("-wal");
    assert!(!Path::new(&wal).exists());
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
fn a_file_given_as_the_data_directory_is_reported_as_not_a_directory() {
    let data = DataDir::new();
    let file = data.path().join("file");
    std::fs::write(&file, b"").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_scitl-cli"))
        .arg("--data-dir")
        .arg(&file)
        .args(["task", "list"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not a directory"));
}

/// 上位とサブコマンドの両方に書けると、どちらを使うかが黙って決まる。
#[test]
fn the_data_directory_is_accepted_only_before_the_command() {
    let data = DataDir::new();
    let other = DataDir::new();

    let output = data.run(&["task", "--data-dir", other.path().to_str().unwrap(), "list"]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--data-dir"));
}

#[cfg(unix)]
#[test]
fn a_read_only_data_directory_is_refused_with_the_reason() {
    use std::os::unix::fs::PermissionsExt;

    let data = DataDir::new();
    assert!(data.run(&["task", "list"]).status.success());
    let writable = std::fs::metadata(data.path()).unwrap().permissions();
    std::fs::set_permissions(data.path(), std::fs::Permissions::from_mode(0o500)).unwrap();

    let output = data.run(&["task", "list"]);
    std::fs::set_permissions(data.path(), writable).unwrap();

    // 権限を無視できる利用者(root)で走らせると、書き込めてしまう。
    if output.status.success() {
        return;
    }
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not writable"));
}

#[test]
fn showing_the_chat_of_a_missing_or_deleted_task_fails() {
    let data = DataDir::new();
    let task_id = {
        let conn = db::open(DataLayout::new(data.path()).database()).unwrap();
        db::tasks::create_task(&conn).unwrap().id
    };
    let task = task_id.to_string();
    assert_eq!(
        stdout_json(&data.run(&["chat", "show", "--task", &task])),
        serde_json::json!([])
    );
    assert!(data.run(&["task", "delete", &task]).status.success());

    for id in [task.as_str(), "999"] {
        let output = data.run(&["chat", "show", "--task", id]);

        assert!(!output.status.success(), "{id}");
        assert!(output.stdout.is_empty(), "{id}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(&format!("task {id} not found")),
            "{id}"
        );
    }
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
