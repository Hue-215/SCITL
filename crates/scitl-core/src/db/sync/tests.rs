use super::*;
use crate::db;

fn versions(conn: &Connection, table: &str) -> Vec<(String, i64)> {
    select(
        conn,
        &format!("SELECT sync_version, sync_changed FROM {table} ORDER BY id"),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[test]
fn marked_rows_get_this_devices_number_and_lose_the_mark() {
    let conn = db::open_in_memory().unwrap();
    let task_id = db::tasks::create_task(&conn).unwrap().id;
    db::task_steps::add_steps(&conn, task_id, &["買い出し".to_string()]).unwrap();
    db::memories::add(&conn, &["朝型".to_string()]).unwrap();
    assert_eq!(versions(&conn, "tasks"), [("{}".to_string(), 1)]);

    let snapshot = stamp_and_snapshot(&conn).unwrap();
    let device = &snapshot.device.device_id;
    for table in VERSIONED.iter().filter(|t| **t != "messages") {
        let rows = versions(&conn, table);
        assert_eq!(rows.len(), 1, "{table}");
        let (version, changed) = &rows[0];
        let version: serde_json::Value = serde_json::from_str(version).unwrap();
        assert!(version[device].as_i64().unwrap() > 1, "{table}");
        assert_eq!(*changed, 0, "{table}");
    }
    assert_eq!(
        snapshot.rows.tasks[0].version,
        versions(&conn, "tasks")[0].0
    );
    assert_eq!(snapshot.device, device_state(&conn).unwrap());

    // 印の無い行の版は変えない。
    let before = versions(&conn, "tasks");
    stamp_and_snapshot(&conn).unwrap();
    assert_eq!(versions(&conn, "tasks"), before);
}

#[test]
fn the_snapshot_includes_deleted_rows() {
    let conn = db::open_in_memory().unwrap();
    let task_id = db::tasks::create_task(&conn).unwrap().id;
    db::tasks::delete_task(&conn, task_id).unwrap();
    let rows = stamp_and_snapshot(&conn).unwrap().rows;
    assert_eq!(rows.tasks.len(), 1);
    assert!(rows.tasks[0].deleted_at.is_some());
}

#[test]
fn data_is_any_task_message_or_memory() {
    let conn = db::open_in_memory().unwrap();
    assert!(!has_data(&conn).unwrap());
    db::memories::add(&conn, &["朝型".to_string()]).unwrap();
    assert!(has_data(&conn).unwrap());
}

#[test]
fn the_device_id_can_be_renewed() {
    let conn = db::open_in_memory().unwrap();
    let before = device_state(&conn).unwrap();
    let renewed = renew_device_id(&conn).unwrap();
    assert_ne!(renewed, before.device_id);
    assert_eq!(device_state(&conn).unwrap().device_id, renewed);
}

#[test]
fn imported_rows_keep_their_uid_and_version_and_lose_the_mark() {
    let source = db::open_in_memory().unwrap();
    db::memories::add(&source, &["朝型".to_string()]).unwrap();
    let rows = stamp_and_snapshot(&source).unwrap().rows;

    let target = db::open_in_memory().unwrap();
    let counter = import(&target, |importer| {
        assert!(!importer.has_data()?);
        importer.insert_memory(&rows.memories[0])?;
        importer.finish(&"a".repeat(32), 42)
    })
    .unwrap();

    let copied = stamp_and_snapshot(&target).unwrap().rows;
    assert_eq!(copied.memories[0].uid, rows.memories[0].uid);
    assert_eq!(copied.memories[0].version, rows.memories[0].version);
    assert_eq!(versions(&target, "memories")[0].1, 0);
    assert_eq!(
        cursor(&target, &"a".repeat(32)).unwrap(),
        Some((counter, 42))
    );
}

#[test]
fn a_failed_import_leaves_nothing() {
    let source = db::open_in_memory().unwrap();
    db::memories::add(&source, &["朝型".to_string()]).unwrap();
    let rows = stamp_and_snapshot(&source).unwrap().rows;

    let target = db::open_in_memory().unwrap();
    let result: Result<()> = import(&target, |importer| {
        importer.insert_memory(&rows.memories[0])?;
        Err(crate::CoreError::Internal("stop".to_string()))
    });
    assert!(result.is_err());
    assert!(!has_data(&target).unwrap());
}

#[test]
fn rules_accept_what_the_app_stores_and_refuse_the_rest() {
    rules::title("買い物").unwrap();
    assert!(rules::title(" 買い物").is_err());
    assert!(rules::title("「買い物」").is_err());
    assert!(rules::title(&"あ".repeat(41)).is_err());
    rules::task_description("牛乳と卵\n2個ずつ").unwrap();
    assert!(rules::task_description(" ").is_err());
    rules::deadline("2026-02-28").unwrap();
    assert!(rules::deadline("2026-02-30").is_err());
    rules::step_description("買い出し").unwrap();
    assert!(rules::step_description("買い出し ").is_err());
    rules::memory_content("朝型").unwrap();
    assert!(rules::memory_content("朝\n型").is_err());
    rules::user_message_content("こんにちは").unwrap();
    rules::user_message_content("").unwrap();
    assert!(rules::user_message_content("こんにちは\n").is_err());
    rules::timestamp(&db::now_iso8601()).unwrap();
    for bad in [
        "2026-10-14T12:00:00",
        "2026-10-14 12:00:00Z",
        "2026-13-14T12:00:00Z",
        "2026-10-14T12:00:00.5Z",
    ] {
        assert!(rules::timestamp(bad).is_err(), "{bad}");
    }
}
