pub mod add_steps;
mod args;
pub mod delete_step;
pub mod external;
pub mod get_current_task_detail;
pub mod get_task_detail;
pub mod get_task_list;
pub mod read_attachment;
pub mod update_step;
pub mod update_task;

use std::sync::LazyLock;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::messages::Chat;
use crate::db::{self, task_steps};
use crate::error::{CoreError, Result};
use crate::llm::ToolSchema;

/// ツール実行結果を次ターン以降の入力履歴に残すか否かの分類(docs/spec/rebuild/tools.md 4節)。
/// 状態系は次ターンの最新状態JSONで完全に代替できるため履歴に残さない。事実系(検索・
/// 外部MCPツール等)は「現在の状態」として言い表せないため、実行記録から履歴を組み立てる。
/// 実行したときに決まった値を実行記録に書き写し、履歴はその値だけを見る
/// (`orchestration::tool_record`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    State,
    Fact,
}

/// 内部ツール1回の実行結果。
///
/// 結果を得たターンでだけモデルへ渡す中身(添付の本文・画像)は、実行記録に残さない
/// (docs/spec/rebuild/tools.md「添付の読み込み」)。
pub struct ToolOutput {
    /// 実行記録に残す結果。事実系なら次ターン以降の履歴にも載る。
    pub result: Value,
    /// このターンの往復で`result`の代わりにモデルへ返す結果。`None`なら`result`を返す。
    pub turn_result: Option<Value>,
    /// 結果と一緒にこのターンでだけモデルへ見せる画像(添付の実体のハッシュ)。実体の
    /// 読み出しはファイルI/Oなので、DBのロックの外で呼び出し元が行う。
    pub image_hashes: Vec<String>,
}

impl From<Value> for ToolOutput {
    fn from(result: Value) -> Self {
        Self {
            result,
            turn_result: None,
            image_hashes: Vec::new(),
        }
    }
}

pub struct ToolDefinition {
    pub schema: &'static ToolSchema,
    pub kind: ToolKind,
}

/// 公開面(docs/spec/rebuild/tools.md 2節)。MCPは枠のみ(Issue #73)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    General,
    Task,
    Mcp,
}

impl Surface {
    /// 会話ごとの面。総合チャットは読み取り専用のツールだけを公開する(tools.md 5節)。
    fn of(chat: Chat) -> Self {
        match chat {
            Chat::General => Self::General,
            Chat::Task(_) => Self::Task,
        }
    }
}

/// 面ごとの公開ツール定義。実装関数は1つのまま、公開するスキーマだけを面で分ける
/// (docs/spec/rebuild/tools.md 1節「確定方針」)。実行の振り分け([`execute`])と
/// 同じ集合を並べる(`each_chat_runs_exactly_the_tools_it_exposes`が確かめる)。
///
/// 定義は固定なので、各ツールの`schema()`も面ごとの一覧も最初の1回だけ組み立てる(引数
/// スキーマの無害化を伴い、実行のたびに引数の検証([`args::Args::parse`])でも引くため)。
pub fn tool_definitions(surface: Surface) -> &'static [ToolDefinition] {
    static GENERAL: LazyLock<Vec<ToolDefinition>> =
        LazyLock::new(|| build_definitions(Surface::General));
    static TASK: LazyLock<Vec<ToolDefinition>> = LazyLock::new(|| build_definitions(Surface::Task));
    match surface {
        Surface::General => &GENERAL,
        Surface::Task => &TASK,
        Surface::Mcp => &[],
    }
}

fn build_definitions(surface: Surface) -> Vec<ToolDefinition> {
    match surface {
        Surface::General => vec![
            ToolDefinition {
                schema: get_task_list::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: get_task_detail::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: read_attachment::schema(),
                kind: ToolKind::Fact,
            },
        ],
        Surface::Task => vec![
            ToolDefinition {
                schema: get_task_list::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: get_current_task_detail::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: update_task::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: add_steps::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: update_step::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: delete_step::schema(),
                kind: ToolKind::State,
            },
            ToolDefinition {
                schema: read_attachment::schema(),
                kind: ToolKind::Fact,
            },
        ],
        Surface::Mcp => Vec::new(),
    }
}

/// 会話で公開する内部ツールの一覧。タスクチャットの`task_id`はターン開始時に
/// オーケストレーション層が束縛するため、引数として公開しない(architecture.md 7節)。
pub fn schemas(chat: Chat) -> Vec<ToolSchema> {
    tool_definitions(Surface::of(chat))
        .iter()
        .map(|def| def.schema.clone())
        .collect()
}

/// 会話で公開する内部ツールの名前。外部ツールの名前空間化で衝突を避けるために使う
/// (`external::ExternalToolset::build`)。
pub fn names(chat: Chat) -> Vec<String> {
    tool_definitions(Surface::of(chat))
        .iter()
        .map(|def| def.schema.name().to_string())
        .collect()
}

/// 会話で公開する内部ツールの分類。公開していない名前には`None`を返す。
pub fn kind(chat: Chat, name: &str) -> Option<ToolKind> {
    tool_definitions(Surface::of(chat))
        .iter()
        .find(|def| def.schema.name() == name)
        .map(|def| def.kind)
}

/// 会話での内部ツールの実行。会話で公開していない名前は[`CoreError::UnknownTool`]に
/// する。総合チャットで更新系のツールを呼ばれても、ここで止まる(権限の分離を
/// モデルの自己制御に頼らない。tools.md 5節)。タスクチャットの`task_id`は呼び出し元
/// (orchestration)が文脈から渡す(モデルには公開しない)。`image_input`はモデルが画像入力に
/// 対応するか(`attachments::delivery`)。
pub fn execute(
    conn: &Connection,
    chat: Chat,
    image_input: bool,
    tool_name: &str,
    arguments: &Value,
) -> Result<ToolOutput> {
    // 更新系は、対象の確認から変更後の全体の読み直しまでを1単位にする。確認のあとや
    // 返す全体に、別プロセスの書き込みが割り込まない(data-model.md 4節)。
    let update = |execute: fn(&Connection, i64, &Value) -> Result<Value>, task_id| {
        db::in_transaction(conn, |conn| execute(conn, task_id, arguments))
    };
    let result = match (chat, tool_name) {
        (_, read_attachment::NAME) => {
            return read_attachment::execute(conn, chat, image_input, arguments)
        }
        (_, get_task_list::NAME) => get_task_list::execute(conn, arguments),
        (Chat::General, get_task_detail::NAME) => get_task_detail::execute(conn, arguments),
        (Chat::Task(task_id), get_current_task_detail::NAME) => {
            get_current_task_detail::execute(conn, task_id, arguments)
        }
        (Chat::Task(task_id), update_task::NAME) => update(update_task::execute, task_id),
        (Chat::Task(task_id), add_steps::NAME) => update(add_steps::execute, task_id),
        (Chat::Task(task_id), update_step::NAME) => update(update_step::execute, task_id),
        (Chat::Task(task_id), delete_step::NAME) => update(delete_step::execute, task_id),
        (_, other) => Err(CoreError::UnknownTool(other.to_string())),
    };
    result.map(ToolOutput::from)
}

/// `step_id`はタスクIDと違いモデルの文脈に頼らず渡させる引数のため
/// (docs/spec/rebuild/tools.md 1節)、対象タスクの取り違え(同節が修正した過去の不具合)を
/// 防ぐには呼び出し側で所属チェックが要る。工程の更新・削除ツールで共有する。
fn require_step_in_task(conn: &Connection, task_id: i64, step_id: i64) -> Result<()> {
    let belongs = task_steps::list_for_task(conn, task_id)?
        .iter()
        .any(|step| step.id == step_id);
    if belongs {
        Ok(())
    } else {
        Err(CoreError::TaskStepNotFound(step_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn every_internal_tool_schema_builds() {
        // `ToolSchema::internal`は引数スキーマを読み直せないと止まる。どの面の定義も組み立てる。
        for surface in [Surface::General, Surface::Task, Surface::Mcp] {
            build_definitions(surface);
        }
        assert!(!schemas(Chat::General).is_empty());
        assert!(!schemas(Chat::Task(1)).is_empty());
    }

    /// 公開する集合と実行できる集合が食い違うと、公開していないツールが呼べてしまう
    /// (総合チャットで更新系が動く)か、公開したツールが未知のツールになる。
    #[test]
    fn each_chat_runs_exactly_the_tools_it_exposes() {
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        let all_names: Vec<String> = [Surface::General, Surface::Task]
            .into_iter()
            .flat_map(tool_definitions)
            .map(|def| def.schema.name().to_string())
            .collect();
        for chat in [Chat::General, Chat::Task(task_id)] {
            let exposed = names(chat);
            for name in &all_names {
                // 引数は検証の手前で止まってもよいので、空で呼ぶ。未知のツールかどうかだけを見る。
                let unknown = matches!(
                    execute(&conn, chat, true, name, &serde_json::json!({})),
                    Err(CoreError::UnknownTool(_))
                );
                assert_eq!(!unknown, exposed.contains(name), "{chat}: {name}");
            }
        }
        assert!(!names(Chat::General).contains(&update_task::NAME.to_string()));
    }

    #[test]
    fn unknown_tool_is_reported_as_unknown_tool() {
        // モデルに返る文言が「未知の引数」にならないこと(ツール名の誤りだと伝える)。
        let conn = db::open_in_memory().unwrap();
        let task_id = db::tasks::create_task(&conn).unwrap().id;
        let err = execute(
            &conn,
            Chat::Task(task_id),
            true,
            "no_such_tool",
            &serde_json::json!({}),
        )
        .err()
        .unwrap();
        assert!(matches!(&err, CoreError::UnknownTool(name) if name == "no_such_tool"));
    }
}
