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

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::db::messages::Chat;
use crate::db::{self, task_steps};
use crate::error::{CoreError, Result};
use crate::llm::ToolSchema;

/// ツール実行結果を次ターン以降の入力履歴に残すか否かの分類。状態系は次ターンの最新状態
/// JSONで完全に代替できるため履歴に残さない。事実系(検索・外部MCPツール等)は「現在の状態」
/// として言い表せないため、実行記録から履歴を組み立てる。実行したときに決まった値を
/// 実行記録に書き写し、履歴はその値だけを見る(`orchestration::tool_record`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    State,
    Fact,
}

/// 内部ツール1回の実行結果。
///
/// 結果を得たターンでだけモデルへ渡す中身(添付の本文・画像)は、実行記録に残さない。
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

/// 内部ツール1つ。公開する定義と実行の振り分けを同じ値から作り、公開したツールだけが
/// 実行できる状態を構造で保つ。
pub(crate) struct InternalTool {
    schema: fn() -> &'static ToolSchema,
    kind: ToolKind,
    run: Run,
}

/// 実行の形。タスクを対象にする形は、タスクチャットの面([`TASK`])にだけ並べる。
#[derive(Clone, Copy)]
enum Run {
    /// 会話によらない読み取り。
    Read(fn(&Connection, &Value) -> Result<Value>),
    /// 会話の対象タスクの読み取り。
    ReadTask(fn(&Connection, i64, &Value) -> Result<Value>),
    /// 会話の対象タスクの更新。対象の確認から変更後の全体の読み直しまでを1単位にする。
    /// 確認のあとや返す全体に、別プロセスの書き込みが割り込まない。
    UpdateTask(fn(&Connection, i64, &Value) -> Result<Value>),
    /// 会話の添付の読み込み。結果にこのターンでだけ渡す中身を伴う。
    ReadAttachment,
}

/// 公開面。
// TODO(#73): `Mcp`はまだ枠だけで、何も公開しない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    General,
    Task,
    Mcp,
}

impl Surface {
    /// 会話ごとの面。総合チャットは読み取り専用のツールだけを公開する。
    fn of(chat: Chat) -> Self {
        match chat {
            Chat::General => Self::General,
            Chat::Task(_) => Self::Task,
        }
    }

    /// 面ごとの公開ツール。実装関数は1つのまま、公開するスキーマだけを面で分ける。
    fn tools(self) -> &'static [InternalTool] {
        match self {
            Self::General => GENERAL,
            Self::Task => TASK,
            Self::Mcp => &[],
        }
    }
}

const GENERAL: &[InternalTool] = &[
    get_task_list::TOOL,
    get_task_detail::TOOL,
    read_attachment::TOOL,
];

const TASK: &[InternalTool] = &[
    get_task_list::TOOL,
    get_current_task_detail::TOOL,
    update_task::TOOL,
    add_steps::TOOL,
    update_step::TOOL,
    delete_step::TOOL,
    read_attachment::TOOL,
];

fn find(chat: Chat, name: &str) -> Option<&'static InternalTool> {
    Surface::of(chat)
        .tools()
        .iter()
        .find(|tool| (tool.schema)().name() == name)
}

/// 会話で公開する内部ツールの一覧。タスクチャットの`task_id`はターン開始時に
/// オーケストレーション層が束縛するため、引数として公開しない。
pub fn schemas(chat: Chat) -> Vec<ToolSchema> {
    Surface::of(chat)
        .tools()
        .iter()
        .map(|tool| (tool.schema)().clone())
        .collect()
}

/// 会話で公開する内部ツールの名前。外部ツールの名前空間化で衝突を避けるために使う
/// (`external::ExternalToolset::build`)。
pub fn names(chat: Chat) -> Vec<String> {
    Surface::of(chat)
        .tools()
        .iter()
        .map(|tool| (tool.schema)().name().to_string())
        .collect()
}

/// 会話で公開する内部ツールの分類。公開していない名前には`None`を返す。
pub fn kind(chat: Chat, name: &str) -> Option<ToolKind> {
    find(chat, name).map(|tool| tool.kind)
}

/// 会話での内部ツールの実行。会話で公開していない名前は[`CoreError::UnknownTool`]にする。
/// 総合チャットで更新系のツールを呼ばれても、ここで止まる(権限の分離をモデルの自己制御に
/// 頼らない)。タスクチャットの`task_id`は呼び出し元(orchestration)が文脈から渡す(モデルには
/// 公開しない)。`image_input`はモデルが画像入力に対応するか(`attachments::delivery`)。
pub fn execute(
    conn: &Connection,
    chat: Chat,
    image_input: bool,
    tool_name: &str,
    arguments: &Value,
) -> Result<ToolOutput> {
    let unknown = || CoreError::UnknownTool(tool_name.to_string());
    let tool = find(chat, tool_name).ok_or_else(unknown)?;
    let result = match (tool.run, chat) {
        (Run::ReadAttachment, _) => {
            return read_attachment::execute(conn, chat, image_input, arguments)
        }
        (Run::Read(run), _) => run(conn, arguments),
        (Run::ReadTask(run), Chat::Task(task_id)) => run(conn, task_id, arguments),
        (Run::UpdateTask(run), Chat::Task(task_id)) => {
            db::in_transaction(conn, |conn| run(conn, task_id, arguments))
        }
        // タスクを対象にする形は総合チャットの面に並べていない。
        (Run::ReadTask(_) | Run::UpdateTask(_), Chat::General) => Err(unknown()),
    };
    result.map(ToolOutput::from)
}

/// 工程が会話の対象タスクに属するかを確かめる。`step_id`はモデルが渡す引数なので、別の
/// タスクの工程を指されうる。工程の更新・削除ツールで共有する。
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
            for tool in surface.tools() {
                (tool.schema)();
            }
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
            .flat_map(Surface::tools)
            .map(|tool| (tool.schema)().name().to_string())
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
