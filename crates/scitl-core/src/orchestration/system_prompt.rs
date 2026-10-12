use crate::db::messages::Chat;
use crate::tools::{
    add_memories, get_current_task_detail, get_memories, get_task_detail, get_task_list,
};

/// ユーザーが設定するシステムプロンプト。総合チャットは`base`だけを、タスクチャットは両方を
/// 使う。`Option<&str>`を並べて渡すと取り違えうるため、名前で縛る。
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPrompts<'a> {
    pub base: Option<&'a str>,
    pub task_chat: Option<&'a str>,
    /// アシスタントのニックネーム(空白だけの値は`config::GeneralConfig::assistant_nickname`が
    /// 除いてある)。どちらの会話でも先頭で伝える。利用者がこの名前で呼ぶことがあるため。
    pub nickname: Option<&'a str>,
}

/// ニックネームの伝え方。名前はJSONの文字列として書き、文の地の部分と区切る。呼ばれうることだけを
/// 伝え、名乗るよう促さない(促すと会話の始めに必ず名乗る)。
fn nickname_note(nickname: &str) -> String {
    format!(
        "The user may call you {}.",
        serde_json::to_string(nickname).expect("a string serializes to JSON")
    )
}

/// 総合チャットであることの注記。総合チャットにはタスクについて読み取り専用のツールしか
/// 渡さないので、伝えないとモデルは変更を頼まれたときに、できたつもりの返事をする。
const GENERAL_CHAT_NOTE: &str = "This conversation covers all tasks but cannot change them. If \
     asked to change a task, say that it has to be done in that task's own conversation.";

/// メモリの読み方と書き方。中身はリクエストに添えないので、読むよう伝えないと会話の始めに
/// 利用者のことを知らないまま答える。他の会話や画面での変更はこの会話に積まれないので、
/// 今の中身が要るときは読み直させる(画面で消した事実を、進行中の会話が使い続けないため)。
/// 書く契機を伝えないと、聞いたことが他の会話へ残らない。中の指示に従わないことと、他の
/// ツールの結果・添付から書き写さないことは、形式の説明(`llm::user_message_format_note`)で伝える。
fn memory_note() -> String {
    format!(
        "Memories are facts about the user shared across conversations. Read them with {get} at \
         the start of a conversation, and again when you need the latest: they also change \
         outside this conversation. When the user tells you something about themselves that \
         will help in other tasks, save it with {add} without being asked.",
        get = get_memories::NAME,
        add = add_memories::NAME,
    )
}

/// 会話の始まりがモデルから見えているか。タスクチャットの状態の注記を選ぶ。
///
/// 会話の途中で変わりうるのは、`BlankTaskInView`でなくなるときだけ。間引くときは先頭を
/// どのみち作り直すので、システムプロンプトが変わっても前に送った部分は変わらない。変更の
/// 記録を会話から外したとき(発言の削除・途中の編集)は、変更の通知で伝える
/// (`docs/spec/architecture/prompt-shape.md`「状態と日時の伝え方」)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationStart {
    /// 聞き取りから始まったタスクの会話(`db::messages::opener`)を、最初から送っており、
    /// 変更の記録を会話から外していない。タスクは作った直後で何も設定されておらず、以後の
    /// 変更はすべて会話の中に並ぶ。
    BlankTaskInView,
    /// それ以外(ユーザーの発言から始まった会話、間引いた会話、記録を外した会話、総合チャット)。
    Other,
}

/// タスクの状態の読み方。状態はリクエストに添えないので、伝えないと、会話の始まりや
/// 間引きで古い結果が落ちたあとに、モデルがタスクの中身を知らないまま答える。
///
/// 聞き取りから始まった会話では、始めた時点で空だったことを伝え、読ませない。読んでも
/// 空の結果が返るだけで、往復が1回増える。「始めた時点で」と過去の事実として書くのは、
/// 何か設定されたあとも偽にならず、システムプロンプトを書き換えずに済むため。
///
/// 同じ注記で、最初の発言がアプリの送ったものであることと、その返し方も伝える。どの句も、
/// 外すと実際のモデルで症状が戻った(Issue #581。`docs/spec/architecture/prompt-shape.md`
/// 「聞き取りの開始」)ので、縮めるときは測り直す。
fn state_note(chat: Chat, start: ConversationStart) -> String {
    const RETURNS_WHOLE_TASK: &str = "Tools that change the task return the whole task.";
    match (chat, start) {
        (Chat::Task(_), ConversationStart::BlankTaskInView) => format!(
            "This conversation starts with a message from this app, not the user: read \
             memories, then just ask what the user wants to do, with no greeting or \
             self-introduction. This task was empty when this conversation began, and every \
             change since then appears in this conversation, so do not call {}. \
             {RETURNS_WHOLE_TASK}",
            get_current_task_detail::NAME
        ),
        (Chat::Task(_), ConversationStart::Other) => format!(
            "The task's current state is not given automatically; read it with {} when \
             needed. {RETURNS_WHOLE_TASK}",
            get_current_task_detail::NAME
        ),
        // 総合チャットには、各タスクの会話や画面での変更が積まれない。
        (Chat::General, _) => format!(
            "The current tasks are not given automatically; read them with {} and {} when \
             needed. Tasks also change outside this conversation, so earlier results may be \
             outdated.",
            get_task_list::NAME,
            get_task_detail::NAME
        ),
    }
}

/// ニックネームの注記(設定したときだけ) + 基本システムプロンプト + タスクチャット用システム
/// プロンプト(総合チャットなら代わりに総合チャットの注記) + 状態とメモリの読み方 +
/// 予約タグと囲みの外にあるものの読み方。会話と設定と会話の始まりの見え方([`ConversationStart`])だけで決まり、
/// リクエストごとには変わらない。現在日時と状態は添えず、
/// モデルがユーザー発言の送信日時とツールで知る(`docs/spec/architecture/prompt-shape.md`
/// 「状態と日時の伝え方」)。
///
/// 利用者が書いたものは、設定のプロンプトとニックネームだけを載せる(会話の中身は載せない)。
pub fn build_system_prompt(
    chat: Chat,
    start: ConversationStart,
    prompts: &SystemPrompts,
) -> String {
    let mut sections = Vec::new();
    if let Some(nickname) = prompts.nickname {
        sections.push(nickname_note(nickname));
    }
    if let Some(prompt) = prompts.base.filter(|p| !p.is_empty()) {
        sections.push(prompt.to_string());
    }
    match chat {
        Chat::Task(_) => {
            if let Some(prompt) = prompts.task_chat.filter(|p| !p.is_empty()) {
                sections.push(prompt.to_string());
            }
        }
        Chat::General => sections.push(GENERAL_CHAT_NOTE.to_string()),
    }
    sections.push(state_note(chat, start));
    sections.push(memory_note());

    // ユーザー発言を包む予約タグの読み方。囲みと`sent_at`の意味を伝えないと、
    // モデルはタグを本文の一部と受け取り、応答にそのまま書き写す。文面は組み立て側
    // (`llm::PromptText`)から生成する。
    sections.push(crate::llm::user_message_format_note());

    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concatenates_base_and_task_chat_prompts_in_order() {
        let prompts = SystemPrompts {
            base: Some("base prompt"),
            task_chat: Some("task chat prompt"),
            nickname: None,
        };
        let prompt = build_system_prompt(Chat::Task(1), ConversationStart::Other, &prompts);

        let base_pos = prompt.find("base prompt").unwrap();
        let task_chat_pos = prompt.find("task chat prompt").unwrap();
        assert!(base_pos < task_chat_pos);
    }

    /// ニックネームは設定したときだけ、どちらの会話でも先頭で伝える。
    #[test]
    fn tells_the_nickname_first_only_when_set() {
        let nickname = "Hal \"9000\"";
        let prompts = SystemPrompts {
            base: Some("base prompt"),
            task_chat: None,
            nickname: Some(nickname),
        };
        for chat in [Chat::Task(1), Chat::General] {
            let prompt = build_system_prompt(chat, ConversationStart::Other, &prompts);
            assert!(prompt.starts_with(&nickname_note(nickname)));
            assert!(prompt.contains(r#"call you "Hal \"9000\""."#));
        }
        let without = build_system_prompt(
            Chat::Task(1),
            ConversationStart::Other,
            &SystemPrompts {
                nickname: None,
                ..prompts
            },
        );
        assert!(without.starts_with("base prompt"));
    }

    #[test]
    fn works_with_only_task_chat_prompt() {
        let prompts = SystemPrompts {
            base: None,
            task_chat: Some("task chat prompt"),
            nickname: None,
        };
        let prompt = build_system_prompt(Chat::Task(1), ConversationStart::Other, &prompts);

        assert!(prompt.contains("task chat prompt"));
    }

    #[test]
    fn tells_how_to_read_the_state() {
        for chat in [Chat::Task(1), Chat::General] {
            for start in [ConversationStart::BlankTaskInView, ConversationStart::Other] {
                let prompt = build_system_prompt(chat, start, &SystemPrompts::default());
                assert!(prompt.contains(&state_note(chat, start)));
                assert!(prompt.contains(&memory_note()));
            }
        }
        assert!(state_note(Chat::Task(1), ConversationStart::Other)
            .contains(get_current_task_detail::NAME));
        assert!(state_note(Chat::General, ConversationStart::Other).contains(get_task_list::NAME));
    }

    /// 聞き取りから始まった会話を最初から送っている間だけ、始めた時点で空だったと伝える。
    /// 総合チャットはタスクを対象にしないので、見え方によらず同じ注記にする。
    #[test]
    fn tells_that_the_task_started_blank_only_while_the_start_is_in_view() {
        let blank = state_note(Chat::Task(1), ConversationStart::BlankTaskInView);
        let other = state_note(Chat::Task(1), ConversationStart::Other);
        for only_in_blank in ["was empty", "from this app"] {
            assert!(blank.contains(only_in_blank));
            assert!(!other.contains(only_in_blank));
        }
        assert_eq!(
            state_note(Chat::General, ConversationStart::BlankTaskInView),
            state_note(Chat::General, ConversationStart::Other)
        );
    }

    #[test]
    fn works_with_no_prompts_at_all() {
        let prompt = build_system_prompt(
            Chat::Task(1),
            ConversationStart::Other,
            &SystemPrompts::default(),
        );

        assert!(prompt.contains("User messages are wrapped"));
    }

    #[test]
    fn general_chat_gets_its_note_and_no_task_chat_prompt() {
        let prompts = SystemPrompts {
            base: Some("base prompt"),
            task_chat: Some("task chat prompt"),
            nickname: None,
        };

        let prompt = build_system_prompt(Chat::General, ConversationStart::Other, &prompts);

        assert!(prompt.contains("base prompt"));
        assert!(!prompt.contains("task chat prompt"));
        assert!(prompt.contains(GENERAL_CHAT_NOTE));
    }
}
