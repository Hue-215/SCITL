/// 内部ツール1つの、名前(`NAME`)・公開する定義(`schema`)・登録(`TOOL`)。定義は最初に
/// 使うときに1度だけ組み立てる。`run`は実行の形([`Run`])で、あとに説明と引数スキーマを並べる。
macro_rules! internal_tool {
    (
        $(#[$doc:meta])*
        name: $name:literal,
        run: $run:expr,
        $description:expr,
        $parameters:expr $(,)?
    ) => {
        pub const NAME: &str = $name;

        pub(super) const TOOL: $crate::tools::InternalTool = $crate::tools::InternalTool {
            schema,
            run: $run,
        };

        $(#[$doc])*
        pub fn schema() -> &'static $crate::llm::ToolSchema {
            static SCHEMA: std::sync::LazyLock<$crate::llm::ToolSchema> =
                std::sync::LazyLock::new(|| {
                    $crate::llm::ToolSchema::internal(NAME, $description, $parameters)
                });
            &SCHEMA
        }
    };
}

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
use serde_json::Value;

use crate::db::messages::Chat;
use crate::db::{self, task_steps};
use crate::error::{CoreError, Result};
use crate::llm::ToolSchema;

/// 内部ツール1回の実行結果。
///
/// 往復でモデルへ渡す中身(添付の本文・画像)は、実行記録に残さない。渡した中身は送った形の
/// 保存(`orchestration::transcript`)に残り、次のターン以降もその形で並ぶ。保存を使えずに
/// 実行記録から組み立てるターンでは、記録に残した結果だけが並ぶ。
pub struct ToolOutput {
    /// 実行記録に残す結果。
    pub result: Value,
    /// 往復で`result`の代わりにモデルへ返す結果。`None`なら`result`を返す。
    pub turn_result: Option<Value>,
    /// 結果と一緒にモデルへ見せる画像(添付の実体のハッシュ)。実体の読み出しはファイルI/Oなので、
    /// DBのロックの外で呼び出し元が行う。
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
    run: Run,
}

/// 実行の形。タスクを対象にする形は、タスクチャットの一覧([`TASK`])にだけ並べる。
#[derive(Clone, Copy)]
enum Run {
    /// 会話によらない読み取り。
    Read(fn(&Connection, &Value) -> Result<Value>),
    /// 会話の対象タスクの読み取り。
    ReadTask(fn(&Connection, i64, &Value) -> Result<Value>),
    /// 会話の対象タスクの更新。対象の確認から変更後の全体の読み直しまでを1単位にする。
    /// 確認のあとや返す全体に、別プロセスの書き込みが割り込まない。
    UpdateTask(fn(&Connection, i64, &Value) -> Result<Value>),
    /// 会話の添付の読み込み。結果に実行記録へ残さない中身を伴う。
    ReadAttachment,
}

/// 会話で公開する内部ツール。総合チャットは読み取り専用のツールだけを公開する。実装関数は
/// 1つのまま、公開する集合だけを会話で分ける。
fn tools_of(chat: Chat) -> &'static [InternalTool] {
    match chat {
        Chat::General => GENERAL,
        Chat::Task(_) => TASK,
    }
}

/// 1つの会話で公開する内部ツールの数の最大。外部ツールの数の上限を、内部ツールが一番多い
/// 会話に合わせて決めるために使う(`external::MAX_EXTERNAL_TOOLS`)。
pub(crate) const MAX_INTERNAL_TOOLS: usize = if GENERAL.len() > TASK.len() {
    GENERAL.len()
} else {
    TASK.len()
};

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
    tools_of(chat)
        .iter()
        .find(|tool| (tool.schema)().name() == name)
}

/// 会話で公開する内部ツールの一覧。タスクチャットの`task_id`はターン開始時に
/// オーケストレーション層が束縛するため、引数として公開しない。
pub fn schemas(chat: Chat) -> Vec<ToolSchema> {
    tools_of(chat)
        .iter()
        .map(|tool| (tool.schema)().clone())
        .collect()
}

/// 会話で公開する内部ツールの名前。外部ツールの名前空間化で衝突を避けるために使う
/// (`external::ExternalToolset::build`)。
pub fn names(chat: Chat) -> Vec<String> {
    tools_of(chat)
        .iter()
        .map(|tool| (tool.schema)().name().to_string())
        .collect()
}

/// その名前のツールを実行すると、効果が後に残りうるか。内部ツールは更新系だけが残り、読み取り
/// (添付の読み込みを含む)は残らない。外部ツールと知らない名前は、読むだけか判別できないので
/// 残りうるものとする。捨てた試行の記録を伝えるか(`orchestration::history`)の判断に使う。
pub fn has_lasting_effect(name: &str) -> bool {
    [GENERAL, TASK]
        .into_iter()
        .flatten()
        .find(|tool| (tool.schema)().name() == name)
        .is_none_or(|tool| match tool.run {
            Run::UpdateTask(_) => true,
            Run::Read(_) | Run::ReadTask(_) | Run::ReadAttachment => false,
        })
}

/// 実行記録に残した結果を、記録から組み立てる履歴に載せる形にする。往復でだけ渡した中身
/// ([`ToolOutput`])は記録に無いので、中身を渡したと伝える結果は、渡していない形に直す。
pub fn recorded_result(name: &str, result: Value) -> Value {
    if name == read_attachment::NAME {
        read_attachment::without_content(result)
    } else {
        result
    }
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
        // タスクを対象にする形は総合チャットの一覧に並べていない。
        (Run::ReadTask(_) | Run::UpdateTask(_), Chat::General) => Err(unknown()),
    };
    result.map(ToolOutput::from)
}

/// 工程が会話の対象タスクに属するかを確かめる。`step_id`はモデルが渡す引数なので、別の
/// タスクの工程を指されうる。工程の更新・削除ツールで共有する。
fn require_step_in_task(conn: &Connection, task_id: i64, step_id: i64) -> Result<()> {
    if task_steps::belongs_to_task(conn, task_id, step_id)? {
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
        // `ToolSchema::internal`は引数スキーマを読み直せないと止まる。どの会話の定義も組み立てる。
        for tool in [GENERAL, TASK].into_iter().flatten() {
            (tool.schema)();
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
        let all_names: Vec<String> = [GENERAL, TASK]
            .into_iter()
            .flatten()
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
    fn only_updates_and_unknown_tools_have_lasting_effects() {
        for name in [
            update_task::NAME,
            add_steps::NAME,
            update_step::NAME,
            delete_step::NAME,
            "server__tool",
        ] {
            assert!(has_lasting_effect(name), "{name}");
        }
        for name in [
            get_task_list::NAME,
            get_task_detail::NAME,
            get_current_task_detail::NAME,
            read_attachment::NAME,
        ] {
            assert!(!has_lasting_effect(name), "{name}");
        }
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
