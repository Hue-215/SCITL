use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{check_max_chars, now_iso8601};
use crate::error::{CoreError, Result};
use crate::text::{collapse_whitespace, ellipsize, visible_line};

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
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

/// `update_task`ツールが受け付ける引数。`status`は列挙のみを許す(削除をここから漏らさない)。
/// タイトルは消せない(未設定に戻す操作を持たない)ので`Option`のまま。
#[derive(Debug, Default)]
pub struct TaskUpdate {
    pub title: Option<String>,
    pub description: FieldChange,
    pub deadline: FieldChange,
    pub status: Option<TaskStatus>,
}

/// 消せる項目の変更。「指定なし」と「消す」を別の値にする。`null`を「消す」の意味にすると、
/// 型に緩いモデルが変えないつもりの項目にも`null`を入れて値が消える。
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
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct TaskSummary {
    pub id: i64,
    pub title: Option<String>,
    pub deadline: Option<String>,
    pub archived_at: Option<String>,
    pub steps_done: i64,
    pub steps_total: i64,
}

/// サイドバーの1行。`TaskSummary`に、表示側だけで使うフォールバックを添える。
/// `title`は未設定(null)のまま返し、書き換えない。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct TaskListItem {
    #[serde(flatten)]
    pub summary: TaskSummary,
    /// `title`が未設定のときに代わりに表示する、最初のユーザー発言の切り詰め。
    /// `title`があるか、ユーザー発言がまだ無ければ`None`(その場合の表示は画面側が決める)。
    pub fallback_label: Option<String>,
}

/// 画面のヘッダー向けのタスク詳細。`TaskListItem`と同じ工程の数とフォールバックを添える。
/// `Task`そのものに足さないのは、`Task`がモデルへ渡す`task_detail`にも乗り、混ぜると
/// モデルがタイトル設定済みと誤解するため。
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct TaskDetailView {
    #[serde(flatten)]
    pub task: Task,
    pub steps_done: i64,
    pub steps_total: i64,
    /// `TaskListItem::fallback_label`と同じ。
    pub fallback_label: Option<String>,
}

/// タスク`t`の工程の完了数と総数を引く相関サブクエリ。一覧とヘッダーで数え方を
/// 食い違わせないため、ここだけに書く。
const STEPS_DONE: &str = "(SELECT COUNT(*) FROM task_steps s
                  WHERE s.task_id = t.id AND s.deleted_at IS NULL AND s.done_at IS NOT NULL)";
const STEPS_TOTAL: &str = "(SELECT COUNT(*) FROM task_steps s
                  WHERE s.task_id = t.id AND s.deleted_at IS NULL)";

/// 削除済み(deleted_at)を除く全タスクを作成日時昇順で返す。アーカイブ済みと未アーカイブの
/// 振り分けはフロントエンド側(archived_atの有無)で行う。
pub fn list_tasks(conn: &Connection) -> Result<Vec<TaskListItem>> {
    let mut labels = fallback_labels(conn, None)?;
    Ok(list_summaries(conn)?
        .into_iter()
        .map(|summary| TaskListItem {
            fallback_label: labels.remove(&summary.id),
            summary,
        })
        .collect())
}

/// [`list_tasks`]と同じ範囲・順の、代わりの呼び名を添えない形。呼び名を使わない呼び出し側
/// (モデルへ渡すタスク一覧)が、組み立てて捨てる分の問い合わせをしないために使う。
pub fn list_summaries(conn: &Connection) -> Result<Vec<TaskSummary>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT t.id, t.title, t.deadline, t.archived_at, {STEPS_DONE}, {STEPS_TOTAL}
         FROM tasks t
         WHERE t.deleted_at IS NULL
         ORDER BY t.created_at ASC"
    ))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(TaskSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                deadline: row.get(2)?,
                archived_at: row.get(3)?,
                steps_done: row.get(4)?,
                steps_total: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 新規タスクの追加。タイトル・締切は未設定(null)で作り、聞き取りはチャットで行う。
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
        &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1 AND deleted_at IS NULL"),
        [task_id],
        row_to_task,
    )
    .optional()?
    .ok_or(CoreError::TaskNotFound(task_id))
}

/// 削除済みを除く全タスクの行を作成日時昇順で返す。[`list_tasks`]と同じ範囲・順で、
/// 一覧に出さない列(説明・作成/更新日時)まで要る呼び出し側(エクスポート)が使う。
pub fn list_all(conn: &Connection) -> Result<Vec<Task>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLUMNS} FROM tasks WHERE deleted_at IS NULL ORDER BY created_at ASC"
    ))?;
    let rows = stmt
        .query_map([], row_to_task)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// 画面のヘッダー向け。存在しない・削除済みなら`get_task`と同じく`TaskNotFound`。
pub fn get_task_detail_view(conn: &Connection, task_id: i64) -> Result<TaskDetailView> {
    let task = get_task(conn, task_id)?;
    let (steps_done, steps_total): (i64, i64) = conn.query_row(
        &format!("SELECT {STEPS_DONE}, {STEPS_TOTAL} FROM tasks t WHERE t.id = ?1"),
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok(TaskDetailView {
        task,
        steps_done,
        steps_total,
        fallback_label: fallback_labels(conn, Some(task_id))?.remove(&task_id),
    })
}

pub fn update_task(conn: &Connection, task_id: i64, update: TaskUpdate) -> Result<Task> {
    super::in_transaction(conn, |conn| {
        // 存在確認(未削除)を先に行い、TaskNotFoundを一貫して返す。
        let current = get_task(conn, task_id)?;

        if let FieldChange::Set(deadline) = &update.deadline {
            validate_deadline(deadline)?;
        }
        // 空の説明は「未設定」ではなく誤りとして返す。未設定はNULLで表すので、
        // 空文字列を書くと未設定の表し方が2つになる。黙って消去に読み替えもしない。
        if let FieldChange::Set(description) = &update.description {
            if description.trim().is_empty() {
                return Err(CoreError::InvalidArgument {
                    name: "description".to_string(),
                    reason: "must not be empty (to remove the description, use clear)".to_string(),
                });
            }
            check_max_chars("description", description, MAX_DESCRIPTION_CHARS)?;
        }

        let now = now_iso8601();
        let title = update
            .title
            .as_deref()
            .map(normalize_title)
            .transpose()?
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
    })
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

pub(crate) const MAX_TITLE_CHARS: usize = 40;

/// 説明の上限文字数。説明は画面のヘッダーに出るので、そこで無理なく読める長さにする。
pub(crate) const MAX_DESCRIPTION_CHARS: usize = 200;

/// タイトル文字列を、1行のタイトルとして書き込める形に正規化する。描かれない文字(`text::is_invisible_format`)を除き、
/// 制御文字(改行を含む)を空白に畳み込み、前後の空白・引用符を除き、連続空白を1つにまとめる。
/// 描かれない文字は、出力先ごとではなく保存する時点で除く。タイトルは短い表示用の値で、
/// 残しておく理由が無い(ゼロ幅接合子も除くので、結合した絵文字は分かれる)。
///
/// 空になる値と[`MAX_TITLE_CHARS`]を超える値は、黙って捨てたり切ったりせずに断る。タイトルは
/// 未設定に戻す操作を持たず、空文字列を書くと`title IS NULL`前提の判定も壊れる。タイトルの
/// 書き込みはすべて`update_task`を通し、この正規化を通す。
pub(crate) fn normalize_title(raw: &str) -> Result<String> {
    let squeezed = visible_line(raw);
    let trimmed =
        squeezed.trim_matches(|c: char| matches!(c, '"' | '\'' | '「' | '」' | '『' | '』'));
    // 引用符を剥がした内側にも空白が残りうるため、もう一度畳む。
    let title = collapse_whitespace(trimmed);
    if title.is_empty() {
        return Err(CoreError::InvalidArgument {
            name: "title".to_string(),
            reason: "must not be empty (the title cannot be removed)".to_string(),
        });
    }
    check_max_chars("title", &title, MAX_TITLE_CHARS)?;
    Ok(title)
}

/// `deadline`として書き込める形(`YYYY-MM-DD`)かを検証する。タイトルと違い、外れた値を
/// 丸めずエラーとして返す。日時形式や自然文を受け付けると、辞書順=時系列順という前提と、
/// タイムゾーンで締切が前後しない性質が壊れる。
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

    // 形は上で確かめたので、ここでは実在する日付か(月の範囲・月末・閏日)だけを見る。chronoの
    // 書式パースは1桁の月日も受け付けるため、形の確認には使わない。
    let year: i32 = raw[0..4].parse().map_err(|_| invalid())?;
    let month: u32 = raw[5..7].parse().map_err(|_| invalid())?;
    let day: u32 = raw[8..10].parse().map_err(|_| invalid())?;
    chrono::NaiveDate::from_ymd_opt(year, month, day)
        .map(drop)
        .ok_or_else(invalid)
}

/// 画面でタイトルの代わりに出す文字列の上限文字数。切り詰めた場合は末尾に省略記号を
/// 付け、続きがあることを示す。
const MAX_FALLBACK_LABEL_CHARS: usize = 30;

/// タイトル未設定のタスク(`only`を渡せばそのタスクだけ)の、タイトルの代わりに出す1行。
/// タイトルがあれば呼び名は使わないので求めない。ユーザー発言を古い順に見て、最初に作れた
/// もので決める。一覧とヘッダーで選び方を食い違わせないため、どちらもここを通す。空白だけの
/// 発言(添付だけを送った発言等)は名前にならないので飛ばす。その判定は[`fallback_label`]だけが
/// 持つ(SQLの`TRIM`はUnicodeの空白を落とせず、写すと食い違う)。
fn fallback_labels(conn: &Connection, only: Option<i64>) -> Result<HashMap<i64, String>> {
    let mut untitled = conn.prepare_cached(
        "SELECT id FROM tasks
         WHERE title IS NULL AND deleted_at IS NULL AND (?1 IS NULL OR id = ?1)",
    )?;
    let task_ids = untitled
        .query_map([only], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // タスクごとに古い順に読み、呼び名を作れた発言で止める。行は読み進めた分しか引かれない。
    let mut messages = conn.prepare_cached(
        "SELECT content FROM messages
         WHERE task_id = ?1 AND role = 'user' AND kind = 'normal' AND deleted_at IS NULL
         ORDER BY created_at ASC, id ASC",
    )?;
    let mut labels = HashMap::new();
    for task_id in task_ids {
        let mut rows = messages.query([task_id])?;
        while let Some(row) = rows.next()? {
            if let Some(label) = fallback_label(&row.get::<_, String>(0)?) {
                labels.insert(task_id, label);
                break;
            }
        }
    }
    Ok(labels)
}

/// ユーザー発言から、タイトルの代わりに出せる1行を作る(DBには書き戻さない)。空白しか
/// 無い発言では`None`を返す。
fn fallback_label(first_user_message: &str) -> Option<String> {
    let squeezed = collapse_whitespace(first_user_message);
    (!squeezed.is_empty()).then(|| ellipsize(&squeezed, MAX_FALLBACK_LABEL_CHARS))
}

/// タスクを論理削除する。配下の工程の`deleted_at`は書き換えない。ツールには公開せず、画面・
/// CLIからのみ呼ぶ。削除後の行を返す(`get_task`は削除済みを引けないので、操作の記録に載せる
/// 値はここで取る)。
pub fn delete_task(conn: &Connection, task_id: i64) -> Result<Task> {
    conn.query_row(
        &format!(
            "UPDATE tasks SET deleted_at = ?1, updated_at = ?1
             WHERE id = ?2 AND deleted_at IS NULL
             RETURNING {TASK_COLUMNS}"
        ),
        rusqlite::params![now_iso8601(), task_id],
        row_to_task,
    )
    .optional()?
    .ok_or(CoreError::TaskNotFound(task_id))
}

/// [`row_to_task`]が読む列の並び。
const TASK_COLUMNS: &str =
    "id, title, description, deadline, archived_at, deleted_at, created_at, updated_at";

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
    fn update_task_normalizes_title_control_chars_invisible_chars_and_quotes() {
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

        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("abc\u{202E}def\u{200B}ghi".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.title.as_deref(), Some("abcdefghi"));
    }

    #[test]
    fn update_task_rejects_title_over_the_limit_without_truncating() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);

        // 上限ちょうどは通る。数えるのは引用符と描かれない文字を除いた後。
        let at_limit = "あ".repeat(MAX_TITLE_CHARS);
        let updated = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some(format!("「{at_limit}\u{200B}」")),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.title.as_deref(), Some(at_limit.as_str()));

        let err = update_task(
            &conn,
            id,
            TaskUpdate {
                title: Some("い".repeat(MAX_TITLE_CHARS + 1)),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(&err, CoreError::InvalidArgument { name, .. } if name == "title"));
        assert_eq!(get_task(&conn, id).unwrap().title, Some(at_limit));
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
    fn update_task_rejects_title_that_is_blank_after_normalizing() {
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

        for blank in ["", "   \n\"\"   ", "\u{200B}"] {
            let err = update_task(
                &conn,
                id,
                TaskUpdate {
                    title: Some(blank.to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(
                matches!(&err, CoreError::InvalidArgument { name, .. } if name == "title"),
                "expected InvalidArgument for {blank:?}, got {err:?}"
            );
        }
        assert_eq!(
            get_task(&conn, id).unwrap().title.as_deref(),
            Some("買い物")
        );
    }

    #[test]
    fn update_task_rejects_description_over_the_limit() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        let set = |description: String| {
            update_task(
                &conn,
                id,
                TaskUpdate {
                    description: FieldChange::Set(description),
                    ..Default::default()
                },
            )
        };

        set("あ".repeat(MAX_DESCRIPTION_CHARS)).unwrap();
        let err = set("い".repeat(MAX_DESCRIPTION_CHARS + 1)).unwrap_err();
        assert!(matches!(&err, CoreError::InvalidArgument { name, .. } if name == "description"));
        assert_eq!(
            get_task(&conn, id).unwrap().description,
            Some("あ".repeat(MAX_DESCRIPTION_CHARS))
        );
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
        // 表示側だけの処理であり、`title`は未設定のまま。
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
    fn list_tasks_skips_blank_first_user_message() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        seed_user_message(&conn, id, " \n");
        seed_user_message(&conn, id, "写真の件");

        assert_eq!(
            list_tasks(&conn).unwrap()[0].fallback_label.as_deref(),
            Some("写真の件")
        );
    }

    #[test]
    fn fallback_skips_a_message_of_only_non_ascii_whitespace() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        seed_user_message(&conn, id, "\u{3000}\u{00A0}");
        seed_user_message(&conn, id, "写真の件");

        assert_eq!(
            list_tasks(&conn).unwrap()[0].fallback_label.as_deref(),
            Some("写真の件")
        );
        assert_eq!(
            get_task_detail_view(&conn, id)
                .unwrap()
                .fallback_label
                .as_deref(),
            Some("写真の件")
        );
    }

    #[test]
    fn fallback_labels_are_per_task_and_only_for_untitled_tasks() {
        let conn = db::open_in_memory().unwrap();
        let first = seed_task(&conn);
        let titled = seed_task(&conn);
        let second = seed_task(&conn);
        seed_user_message(&conn, first, "1つ目");
        seed_user_message(&conn, titled, "付いたタイトルがある");
        seed_user_message(&conn, second, "2つ目");
        update_task(
            &conn,
            titled,
            TaskUpdate {
                title: Some("発表".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        let labels: Vec<_> = list_tasks(&conn)
            .unwrap()
            .into_iter()
            .map(|item| item.fallback_label)
            .collect();
        assert_eq!(
            labels,
            vec![Some("1つ目".to_string()), None, Some("2つ目".to_string())]
        );
        assert_eq!(
            get_task_detail_view(&conn, titled).unwrap().fallback_label,
            None
        );
    }

    #[test]
    fn detail_view_has_same_fallback_as_list_and_keeps_title_null() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        seed_user_message(&conn, id, "  来週の  発表資料を\n作りたい  ");

        let view = get_task_detail_view(&conn, id).unwrap();
        assert_eq!(
            view.fallback_label,
            list_tasks(&conn).unwrap()[0].fallback_label
        );
        assert!(view.fallback_label.is_some());
        assert!(view.task.title.is_none());
    }

    #[test]
    fn detail_view_counts_steps_like_the_list() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        let steps = db::task_steps::add_steps(
            &conn,
            id,
            &["a".to_string(), "b".to_string(), "c".to_string()],
        )
        .unwrap();
        db::task_steps::update_step(&conn, steps[0].id, None, Some(true)).unwrap();
        db::task_steps::delete_step(&conn, steps[1].id).unwrap();

        let view = get_task_detail_view(&conn, id).unwrap();
        let item = &list_tasks(&conn).unwrap()[0];
        assert_eq!((view.steps_done, view.steps_total), (1, 2));
        assert_eq!((item.summary.steps_done, item.summary.steps_total), (1, 2));
    }

    #[test]
    fn detail_view_of_deleted_task_is_not_found() {
        let conn = db::open_in_memory().unwrap();
        let id = seed_task(&conn);
        delete_task(&conn, id).unwrap();

        assert!(matches!(
            get_task_detail_view(&conn, id),
            Err(CoreError::TaskNotFound(_))
        ));
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

        let deleted = delete_task(&conn, id).unwrap();

        assert!(deleted.deleted_at.is_some());
        assert_eq!(deleted.updated_at, deleted.deleted_at.clone().unwrap());
        assert!(matches!(
            get_task(&conn, id).unwrap_err(),
            CoreError::TaskNotFound(_)
        ));
        // 2回目は削除済みなので見つからない(削除日時を上書きしない)。
        assert!(matches!(
            delete_task(&conn, id).unwrap_err(),
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
