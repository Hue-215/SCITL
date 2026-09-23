use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{now_iso8601, CoreError, Result};

#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: i64,
    pub title: Option<String>,
    pub description: Option<String>,
    pub deadline: Option<String>,
    pub archived_at: Option<String>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// `update_task`ツールが受け付ける引数。`status`は列挙のみを許す
/// (docs/spec/rebuild/tools.md「変更点の詳細」— 削除操作をここから漏らさない)。
/// タイトルは消せない(未設定に戻す操作を持たない)ので`Option`のまま。
#[derive(Debug, Default)]
pub struct TaskUpdate {
    pub title: Option<String>,
    pub description: FieldChange,
    pub deadline: FieldChange,
    pub status: Option<TaskStatus>,
}

/// 消せる項目の変更。「指定なし」と「消す」を別の値にする。`null`を「消す」の意味にすると、
/// 型に緩いモデルが変えないつもりの項目にも`null`を入れて値が消える
/// (docs/spec/rebuild/tools.md 2節)。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum FieldChange {
    #[default]
    Keep,
    Set(String),
    Clear,
}

impl FieldChange {
    fn apply(self, current: Option<String>) -> Option<String> {
        match self {
            Self::Keep => current,
            Self::Set(value) => Some(value),
            Self::Clear => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Archived,
    Unarchived,
}

impl TaskStatus {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "archived" => Ok(Self::Archived),
            "unarchived" => Ok(Self::Unarchived),
            other => Err(CoreError::InvalidArgument {
                name: "status".to_string(),
                reason: format!("unknown value: {other}"),
            }),
        }
    }
}

/// サイドバーのタスク一覧表示に必要な最小限の情報。
/// 本文(description)は一覧に出さないため含めない。
#[derive(Debug, Clone, Serialize)]
pub struct TaskSummary {
    pub id: i64,
    pub title: Option<String>,
    pub deadline: Option<String>,
    pub archived_at: Option<String>,
    pub steps_done: i64,
    pub steps_total: i64,
}

/// サイドバーの1行。`TaskSummary`に、表示側だけで使うフォールバックを添える。
/// `title`は未設定(null)のまま返し、書き換えない
/// (docs/spec/rebuild/data-model.md「title は TEXT NULL」)。
#[derive(Debug, Clone, Serialize)]
pub struct TaskListItem {
    #[serde(flatten)]
    pub summary: TaskSummary,
    /// `title`が未設定のときに代わりに表示する、最初のユーザー発言の切り詰め。
    /// ユーザー発言がまだ無ければ`None`(その場合の表示は画面側が決める)。
    pub fallback_label: Option<String>,
}

/// 削除済み(deleted_at)を除く全タスクを作成日時昇順で返す。アーカイブ済みと未アーカイブの
/// 振り分けはフロントエンド側(archived_atの有無)で行う。
pub fn list_tasks(conn: &Connection) -> Result<Vec<TaskListItem>> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.deadline, t.archived_at,
                COUNT(s.id) FILTER (WHERE s.done_at IS NOT NULL) AS steps_done,
                COUNT(s.id) AS steps_total,
                (SELECT m.content FROM messages m
                  WHERE m.task_id = t.id
                    AND m.role = 'user'
                    AND m.kind = 'normal'
                    AND m.deleted_at IS NULL
                  ORDER BY m.created_at ASC, m.id ASC
                  LIMIT 1) AS first_user_message
         FROM tasks t
         LEFT JOIN task_steps s ON s.task_id = t.id AND s.deleted_at IS NULL
         WHERE t.deleted_at IS NULL
         GROUP BY t.id
         ORDER BY t.created_at ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            let first_user_message: Option<String> = row.get(6)?;
            Ok(TaskListItem {
                summary: TaskSummary {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    deadline: row.get(2)?,
                    archived_at: row.get(3)?,
                    steps_done: row.get(4)?,
                    steps_total: row.get(5)?,
                },
                fallback_label: first_user_message.as_deref().and_then(fallback_label),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 新規タスクの追加。タイトル・締切は未設定(null)で作り、聞き取りはチャットで行う
/// (principles.md 1節「チャットが操作の中心」)。
pub fn create_task(conn: &Connection) -> Result<Task> {
    let now = now_iso8601();
    conn.execute(
        "INSERT INTO tasks (title, created_at, updated_at) VALUES (NULL, ?1, ?1)",
        [&now],
    )?;
    get_task(conn, conn.last_insert_rowid())
}

pub fn get_task(conn: &Connection, task_id: i64) -> Result<Task> {
    conn.query_row(
        "SELECT id, title, description, deadline, archived_at, deleted_at, created_at, updated_at
         FROM tasks WHERE id = ?1 AND deleted_at IS NULL",
        [task_id],
        row_to_task,
    )
    .optional()?
    .ok_or(CoreError::TaskNotFound(task_id))
}

pub fn update_task(conn: &Connection, task_id: i64, update: TaskUpdate) -> Result<Task> {
    // 存在確認(未削除)を先に行い、TaskNotFoundを一貫して返す。
    let current = get_task(conn, task_id)?;

    if let FieldChange::Set(deadline) = &update.deadline {
        validate_deadline(deadline)?;
    }
    // 空の説明は「未設定」ではなく誤りとして返す。未設定はNULLで表すので(principles.md 2節)、
    // 空文字列を書くと未設定の表し方が2つになる。黙って消去に読み替えもしない(同3節)。
    if let FieldChange::Set(description) = &update.description {
        if description.trim().is_empty() {
            return Err(CoreError::InvalidArgument {
                name: "description".to_string(),
                reason: "must not be empty (to remove the description, use clear)".to_string(),
            });
        }
    }

    let now = now_iso8601();
    // サニタイズ後に空文字列になった場合は「タイトルの指定なし」として扱い、既存の値を保つ
    // (空文字列をtitleに書き込むと`title IS NULL`前提の判定が壊れるため)。
    let title = update
        .title
        .as_deref()
        .map(sanitize_title)
        .filter(|t| !t.is_empty())
        .or(current.title);
    // 既にアーカイブ済みなら元の日時を保つ(`task_steps::update_step`の`done_at`と同じ)。
    // 状態の列は現在の状態だけを表し、いつ何をしたかは会話ログのツール実行記録が持つ。
    let archived_at = match update.status {
        Some(TaskStatus::Archived) => current.archived_at.or_else(|| Some(now.clone())),
        Some(TaskStatus::Unarchived) => None,
        None => current.archived_at,
    };

    conn.execute(
        "UPDATE tasks SET title = ?1, description = ?2, deadline = ?3, archived_at = ?4,
                          updated_at = ?5
         WHERE id = ?6",
        rusqlite::params![
            title,
            update.description.apply(current.description),
            update.deadline.apply(current.deadline),
            archived_at,
            now,
            task_id,
        ],
    )?;

    get_task(conn, task_id)
}

/// 工程の変更をタスクの更新として記録する。工程はタスクの一部なので、`updated_at`は
/// 「タスクが最後に変わった日時」として工程の変更も含める。
pub(super) fn touch(conn: &Connection, task_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now_iso8601(), task_id],
    )?;
    Ok(())
}

/// タイトル文字列をタイトルとして書き込む前に無害化する(`docs/spec/principles.md` 4節
/// 「自由入力は地の文に混ぜる前にサニタイズする」)。今後すべてのタイトルがモデルの
/// `update_task`呼び出し由来になるため、書き込みの唯一の経路である`update_task`に集約する
/// (docs/spec/principles.md 5節)。制御文字(改行を含む)を空白に畳み込み、前後の空白・
/// 引用符を除き、連続空白を1つにまとめ、上限文字数で切り詰める。
const MAX_TITLE_CHARS: usize = 40;

fn sanitize_title(raw: &str) -> String {
    let squeezed = collapse_whitespace(raw);
    let trimmed =
        squeezed.trim_matches(|c: char| matches!(c, '"' | '\'' | '「' | '」' | '『' | '』'));
    // 引用符を剥がした内側にも空白が残りうるため、もう一度畳んでから切り詰める。
    collapse_whitespace(trimmed)
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// 制御文字(改行を含む)を空白に畳み、連続空白を1つにまとめ、前後の空白を落とす。
fn collapse_whitespace(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `deadline`として書き込める形(`YYYY-MM-DD`)かを検証する。タイトルと違い、外れた値を
/// 丸めずエラーとして返す(`docs/spec/principles.md` 3節「暗黙の型変換をしない」)。
/// 日時形式や自然文を受け付けると、辞書順=時系列順という前提と、タイムゾーンで締切が
/// 前後しない性質が壊れる(`docs/spec/rebuild/data-model.md` 1節)。
fn validate_deadline(raw: &str) -> Result<()> {
    let invalid = || CoreError::InvalidArgument {
        name: "deadline".to_string(),
        reason: "expected a date in YYYY-MM-DD format".to_string(),
    };

    let bytes = raw.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(invalid());
    }
    if bytes
        .iter()
        .enumerate()
        .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return Err(invalid());
    }

    let year: i32 = raw[0..4].parse().map_err(|_| invalid())?;
    let month: u32 = raw[5..7].parse().map_err(|_| invalid())?;
    let day: u32 = raw[8..10].parse().map_err(|_| invalid())?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return Err(invalid());
    }
    Ok(())
}

/// グレゴリオ暦の閏年規則。`db::now_iso8601`が使う日付計算とは向きが逆(あちらは
/// 通算日から日付を作る)ため、共有せずここに置く。
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// サイドバーでタイトルの代わりに出す文字列の上限文字数。切り詰めた場合は末尾に省略記号を
/// 付け、続きがあることを示す。
const MAX_FALLBACK_LABEL_CHARS: usize = 30;

/// 最初のユーザー発言から、一覧に出せる1行を作る。DBには書き戻さない表示専用の処理
/// (Issue #61)。空白しか無い発言では`None`を返す。
fn fallback_label(first_user_message: &str) -> Option<String> {
    let squeezed = collapse_whitespace(first_user_message);
    if squeezed.is_empty() {
        return None;
    }
    let mut chars = squeezed.chars();
    let head: String = chars.by_ref().take(MAX_FALLBACK_LABEL_CHARS).collect();
    Some(if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    })
}

/// 論理削除の書き込み側。配下の工程の`deleted_at`は書き換えない
/// (docs/spec/rebuild/data-model.md「論理削除の伝播について」)。ツールには非公開
/// (docs/spec/rebuild/tools.md 2節「意図的に非公開」)、画面・CLIからのみ呼ぶ。
pub fn delete_task(conn: &Connection, task_id: i64) -> Result<()> {
    get_task(conn, task_id)?;
    let now = now_iso8601();
    conn.execute(
        "UPDATE tasks SET deleted_at = ?1, updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now, task_id],
    )?;
    Ok(())
}

fn row_to_task(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        deadline: row.get(3)?,
        archived_at: row.get(4)?,
        deleted_at: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn seed_task(conn: &Connection) -> i64 {
        let now = now_iso8601();
        conn.execute(
            "INSERT INTO tasks (title, created_at, updated_at) VALUES (NULL, ?1, ?1)",
            [&now],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn update_task_sanitizes_title_control_chars_quotes_and_truncates() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("\"買い物リストの作成\n\n\"".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.title.as_deref(), Some("買い物リストの作成"));

        let long = "あ".repeat(MAX_TITLE_CHARS + 10);
        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some(long),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.title.unwrap().chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn update_task_strips_japanese_bracket_quotes_from_title() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("「買い物リストの作成」".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.title.as_deref(), Some("買い物リストの作成"));

        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("『買い物リストの作成』".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.title.as_deref(), Some("買い物リストの作成"));
    }

    #[test]
    fn update_task_ignores_title_that_is_blank_after_sanitizing() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("買い物".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("   \n\"\"   ".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.title.as_deref(), Some("買い物"));
    }

    #[test]
    fn update_task_rejects_deadline_that_is_not_a_plain_date() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        // 日時形式・自然文・区切りや桁の違い・空文字は、丸めずエラーにする。
        for bad in [
            "2026-10-01T00:00:00Z",
            "来週の金曜",
            "2026/10/01",
            "2026-1-1",
            "",
            "２０２６-10-01",
        ] {
            let err = update_task(
                &conn,
                id,
                TaskUpdate {
                    deadline: FieldChange::Set(bad.to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(
                matches!(&err, CoreError::InvalidArgument { name, .. } if name == "deadline"),
                "expected InvalidArgument for {bad:?}, got {err:?}"
            );
        }

        // 弾いた値は書き込まれない。
        assert!(get_task(&conn, id).unwrap().deadline.is_none());
    }

    #[test]
    fn update_task_rejects_dates_that_do_not_exist() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        for bad in [
            "2026-02-30",
            "2027-02-29",
            "2026-13-01",
            "2026-00-10",
            "2026-01-00",
            "2026-04-31",
        ] {
            let err = update_task(
                &conn,
                id,
                TaskUpdate {
                    deadline: FieldChange::Set(bad.to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err();
            // 種別まで見るのは、将来ここがDB層の別のエラーにすり替わっても気付くため。
            assert!(
                matches!(&err, CoreError::InvalidArgument { name, .. } if name == "deadline"),
                "expected InvalidArgument for {bad:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn update_task_accepts_plain_dates_including_leap_day() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        for good in ["2026-10-01", "2028-02-29", "2000-02-29", "2026-12-31"] {
            let updated = update_task(
                &conn,
                id,
                TaskUpdate {
                    deadline: FieldChange::Set(good.to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(updated.deadline.as_deref(), Some(good));
        }
    }

    #[test]
    fn update_task_sets_only_given_fields() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("買い物".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(updated.title.as_deref(), Some("買い物"));
        assert!(updated.deadline.is_none());
        assert!(updated.archived_at.is_none());
    }

    #[test]
    fn update_task_archives_and_unarchives() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        let archived = update_task(
            &conn,
            id,
            TaskUpdate {
                status: Some(TaskStatus::Archived),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(archived.archived_at.is_some());

        let unarchived = update_task(
            &conn,
            id,
            TaskUpdate {
                status: Some(TaskStatus::Unarchived),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(unarchived.archived_at.is_none());
    }

    #[test]
    fn archiving_again_keeps_the_original_archived_at() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        let archive = || TaskUpdate {
            status: Some(TaskStatus::Archived),
            ..Default::default()
        };
        conn.execute(
            "UPDATE tasks SET archived_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
            [id],
        )
        .unwrap();

        let updated = update_task(&conn, id, archive()).unwrap();
        assert_eq!(updated.archived_at.as_deref(), Some("2020-01-01T00:00:00Z"));
    }

    #[test]
    fn clear_removes_deadline_and_description_and_keep_leaves_them() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        update_task(
            &conn,
            id,
            TaskUpdate {
                description: FieldChange::Set("牛乳と卵".to_string()),
                deadline: FieldChange::Set("2026-10-01".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let kept = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("買い物".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(kept.description.as_deref(), Some("牛乳と卵"));
        assert_eq!(kept.deadline.as_deref(), Some("2026-10-01"));

        let cleared = update_task(
            &conn,
            id,
            TaskUpdate {
                description: FieldChange::Clear,
                deadline: FieldChange::Clear,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(cleared.description.is_none());
        assert!(cleared.deadline.is_none());
        assert_eq!(cleared.title.as_deref(), Some("買い物"));
    }

    #[test]
    fn empty_description_is_rejected_not_written() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        for blank in ["", "  \n "] {
            let err = update_task(
                &conn,
                id,
                TaskUpdate {
                    description: FieldChange::Set(blank.to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(
                matches!(&err, CoreError::InvalidArgument { name, .. } if name == "description")
            );
        }
        assert!(get_task(&conn, id).unwrap().description.is_none());
    }

    #[test]
    fn update_task_missing_returns_not_found() {
        let conn = db::open_in_memory().unwrap();
        let err = update_task(&conn, 999, TaskUpdate::default()).unwrap_err();
        assert!(matches!(err, CoreError::TaskNotFound(999)));
    }

    #[test]
    fn status_parse_rejects_unknown_value() {
        assert!(TaskStatus::parse("deleted").is_err());
    }

    #[test]
    fn create_task_starts_with_null_fields() {
        let conn = db::open_in_memory().unwrap();
        let task = create_task(&conn).unwrap();
        assert!(task.title.is_none());
        assert!(task.deadline.is_none());
        assert!(task.archived_at.is_none());
    }

    #[test]
    fn list_tasks_reports_step_counts_and_excludes_deleted_steps() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        let now = now_iso8601();
        conn.execute(
            "INSERT INTO task_steps (task_id, description, done_at, order_index, created_at)
             VALUES (?1, 'done', ?2, 0, ?2)",
            rusqlite::params![id, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_steps (task_id, description, order_index, created_at)
             VALUES (?1, 'pending', 1, ?2)",
            rusqlite::params![id, now],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO task_steps (task_id, description, deleted_at, order_index, created_at)
             VALUES (?1, 'deleted', ?2, 2, ?2)",
            rusqlite::params![id, now],
        )
        .unwrap();

        let summaries = list_tasks(&conn).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].summary.steps_done, 1);
        assert_eq!(summaries[0].summary.steps_total, 2);
    }

    #[test]
    fn list_tasks_excludes_deleted_tasks() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        conn.execute(
            "UPDATE tasks SET deleted_at = ?1 WHERE id = ?2",
            rusqlite::params![now_iso8601(), id],
        )
        .unwrap();

        assert!(list_tasks(&conn).unwrap().is_empty());
    }

    fn seed_user_message(conn: &Connection, task_id: i64, content: &str) {
        conn.execute(
            "INSERT INTO messages (task_id, role, content, kind, created_at)
             VALUES (?1, 'user', ?2, 'normal', ?3)",
            rusqlite::params![task_id, content, now_iso8601()],
        )
        .unwrap();
    }

    #[test]
    fn list_tasks_falls_back_to_first_user_message_without_writing_title() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        seed_user_message(&conn, id, "  来週の  発表資料を\n作りたい  ");
        seed_user_message(&conn, id, "あとで締切も決める");

        let items = list_tasks(&conn).unwrap();
        assert_eq!(
            items[0].fallback_label.as_deref(),
            Some("来週の 発表資料を 作りたい")
        );
        // 表示側だけの処理であり、`title`は未設定のまま(data-model.md)。
        assert!(items[0].summary.title.is_none());
        assert!(get_task(&conn, id).unwrap().title.is_none());
    }

    #[test]
    fn list_tasks_truncates_long_fallback_label_with_ellipsis() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        seed_user_message(&conn, id, &"あ".repeat(MAX_FALLBACK_LABEL_CHARS + 5));

        let label = list_tasks(&conn).unwrap()[0]
            .fallback_label
            .clone()
            .unwrap();
        assert_eq!(label.chars().count(), MAX_FALLBACK_LABEL_CHARS + 1);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn list_tasks_has_no_fallback_label_without_user_message() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        conn.execute(
            "INSERT INTO messages (task_id, role, content, kind, created_at)
             VALUES (?1, 'assistant', 'どんなタスクですか?', 'normal', ?2)",
            rusqlite::params![id, now_iso8601()],
        )
        .unwrap();
        seed_user_message(&conn, id, "   ");

        assert!(list_tasks(&conn).unwrap()[0].fallback_label.is_none());
    }

    #[test]
    fn list_tasks_skips_deleted_first_user_message() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        seed_user_message(&conn, id, "書き間違えた発言");
        conn.execute(
            "UPDATE messages SET deleted_at = ?1 WHERE task_id = ?2",
            rusqlite::params![now_iso8601(), id],
        )
        .unwrap();
        seed_user_message(&conn, id, "書き直した発言");

        assert_eq!(
            list_tasks(&conn).unwrap()[0].fallback_label.as_deref(),
            Some("書き直した発言")
        );
    }

    #[test]
    fn delete_task_marks_deleted_but_keeps_steps_untouched() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        let now = now_iso8601();
        conn.execute(
            "INSERT INTO task_steps (task_id, description, order_index, created_at)
             VALUES (?1, 'buy', 0, ?2)",
            rusqlite::params![id, now],
        )
        .unwrap();

        delete_task(&conn, id).unwrap();

        assert!(matches!(
            get_task(&conn, id).unwrap_err(),
            CoreError::TaskNotFound(_)
        ));
        let deleted_at: Option<String> = conn
            .query_row(
                "SELECT deleted_at FROM task_steps WHERE task_id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(deleted_at.is_none());
    }

    #[test]
    fn delete_task_missing_returns_not_found() {
        let conn = db::open_in_memory().unwrap();
        assert!(matches!(
            delete_task(&conn, 999).unwrap_err(),
            CoreError::TaskNotFound(999)
        ));
    }
}
