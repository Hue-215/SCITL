//! Androidで、利用者がアプリを見ていない間に終わった応答を通知で知らせる
//! (`architecture/concurrency.md`「Androidで裏へ回ったとき」)。
//!
//! 通知を出すのは`gen/android`のKotlin(`ReplyNotifier`)で、アプリが前に出ている間は出さない
//! (前に出ているかはActivityが知っている)。ここは文面を作り、JNIで頼む。通知を押すと、
//! その会話を開く頼みがActivityに残る。それを[`take_requested_chat`]で引き取り、
//! [`resolve_requested_chat`]で確かめてから画面へ渡す。

use rusqlite::Connection;

use crate::db::messages::Chat;
use crate::db::tasks;
use crate::i18n::{self, Language};
use crate::orchestration::turn_error;
use crate::orchestration::{FinishedTurn, TurnOutcome};
use crate::text;

/// 通知を出すKotlinのクラスと、会話を開く頼みを持つActivityの名前(パッケージは
/// [`crate::APP_IDENTIFIER`])。Kotlin側と揃える。Kotlinの名前と定数はRustの定数を参照できないので、
/// scitl-tauriのテストが照合する。
pub const NOTIFIER_CLASS: &str = "ReplyNotifier";
pub const ACTIVITY_CLASS: &str = "MainActivity";

/// 総合チャットを表す値(Kotlin側の`CHAT_GENERAL`)。タスクチャットはタスクのID(1以上)で表す。
pub const CHAT_GENERAL: i64 = 0;
/// 開く会話の頼みが無いことを表す値(Kotlin側の`CHAT_NONE`)。
pub const CHAT_NONE: i64 = -1;

/// 通知の題と本文の文字数の上限。通知は折り畳んだ状態で1行、開いても数行しか出ない。
const TITLE_MAX_CHARS: usize = 60;
const BODY_MAX_CHARS: usize = 240;

/// 通知の文面。モデル・利用者が書いた文字列は、通知へ出す形にしてある
/// (`architecture/sanitize.md`の「通知(Android)」の行)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// 会話の名前。
    pub title: String,
    /// 返信の冒頭、またはエラー発言の文言。
    pub body: String,
    /// OSの設定に出る、通知のチャンネルの名前。
    pub channel: &'static str,
}

/// 終わった応答生成の通知の文面を作る。
///
/// 題と本文は1行に畳み、描かれない文字を除いて、長さの上限で切る([`text::display_label`])。
/// 通知はOSがほかのアプリの通知と並べて描く文字で、画面のように要素の境界で閉じ込められないため。
/// 本文のMarkdownの記号はそのまま出る。
pub fn notice(lang: Language, finished: &FinishedTurn) -> Notice {
    let title = match finished.chat {
        Chat::General => i18n::text(lang, "chat.general_title").to_string(),
        Chat::Task(_) => {
            let name = finished.task_name.as_deref().unwrap_or_default();
            let name = text::display_label(name, TITLE_MAX_CHARS);
            if name.is_empty() {
                i18n::text(lang, "task.untitled").to_string()
            } else {
                name
            }
        }
    };
    let body = match &finished.outcome {
        TurnOutcome::Replied { text } => text::display_label(text, BODY_MAX_CHARS),
        TurnOutcome::Failed { kind } => turn_error::localized_message(lang, kind),
    };
    let body = if body.is_empty() {
        i18n::text(lang, "reply_notice.no_text").to_string()
    } else {
        body
    };
    Notice {
        title,
        body,
        channel: i18n::text(lang, "reply_notice.channel"),
    }
}

/// 会話を、通知に持たせる値にする。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn chat_code(chat: Chat) -> i64 {
    match chat {
        Chat::General => CHAT_GENERAL,
        Chat::Task(task_id) => task_id,
    }
}

/// 通知から届いた値を、開く会話にする。総合チャットか、存在して削除されていないタスクの
/// 会話だけを通す。
///
/// 値はActivityを起動するIntentに載って届く。Activityはほかのアプリからも起動できるので、
/// 自分の出した通知から来たとは限らない、信用できない入力として扱う。通しても、起こるのは
/// 画面がその会話を表示することだけ。
pub fn resolve_requested_chat(conn: &Connection, code: i64) -> Option<Chat> {
    match code {
        CHAT_GENERAL => Some(Chat::General),
        task_id if task_id > 0 => tasks::get_task(conn, task_id)
            .ok()
            .map(|_| Chat::Task(task_id)),
        _ => None,
    }
}

#[cfg(target_os = "android")]
pub use android::{post, take_requested_chat};

#[cfg(target_os = "android")]
mod android {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    use super::{chat_code, notice, ACTIVITY_CLASS, CHAT_NONE, NOTIFIER_CLASS};
    use crate::android::{app_class, with_application};
    use crate::i18n::Language;
    use crate::orchestration::FinishedTurn;

    /// 終わった応答生成を通知で知らせる。アプリが前に出ている間と、通知の許可が無い間は、
    /// Kotlin側が出さない。頼めなければ診断に出すだけにする。
    pub fn post(lang: Language, finished: &FinishedTurn) {
        let notice = notice(lang, finished);
        let chat = chat_code(finished.chat);
        let posted = with_application(|env, context| {
            let class = app_class(env, context, NOTIFIER_CLASS)?;
            let title = env.new_string(&notice.title)?;
            let body = env.new_string(&notice.body)?;
            let channel = env.new_string(notice.channel)?;
            env.call_static_method(
                &class,
                jni_str!("post"),
                jni_sig!(
                    "(Landroid/content/Context;JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;)V"
                ),
                &[
                    JValue::from(context),
                    JValue::Long(chat),
                    JValue::from(&title),
                    JValue::from(&body),
                    JValue::from(&channel),
                ],
            )?;
            Ok(())
        });
        if let Err(e) = posted {
            crate::diagnostics::report(format_args!("could not post the notification: {e}"));
        }
    }

    /// 通知を押して届いた、開く会話の頼みを引き取る。引き取った頼みはActivityから消える。
    /// 無ければ`None`。値は確かめていない([`super::resolve_requested_chat`]を通す)。
    pub fn take_requested_chat() -> Option<i64> {
        let taken = with_application(|env, context| {
            let class = app_class(env, context, ACTIVITY_CLASS)?;
            env.call_static_method(&class, jni_str!("takeRequestedChat"), jni_sig!("()J"), &[])?
                .j()
        });
        match taken {
            Ok(CHAT_NONE) => None,
            Ok(code) => Some(code),
            Err(e) => {
                crate::diagnostics::report(format_args!("could not take the chat to open: {e}"));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn finished(chat: Chat, task_name: Option<&str>, outcome: TurnOutcome) -> FinishedTurn {
        FinishedTurn {
            chat,
            task_name: task_name.map(str::to_string),
            outcome,
        }
    }

    fn replied(text: &str) -> TurnOutcome {
        TurnOutcome::Replied {
            text: text.to_string(),
        }
    }

    #[test]
    fn a_reply_is_shown_as_one_line_under_the_name_of_its_chat() {
        let notice = notice(
            Language::En,
            &finished(
                Chat::Task(3),
                Some("Trip plan"),
                replied("# Plan\n\n1. Book the flight\n2. Pack"),
            ),
        );
        assert_eq!(notice.title, "Trip plan");
        assert_eq!(notice.body, "# Plan 1. Book the flight 2. Pack");
    }

    #[test]
    fn a_long_reply_is_cut_with_an_ellipsis() {
        let notice = notice(
            Language::En,
            &finished(Chat::General, None, replied(&"あ".repeat(1000))),
        );
        assert_eq!(notice.body.chars().count(), BODY_MAX_CHARS + 1);
        assert!(notice.body.ends_with('…'));
    }

    /// 通知はOSが描くので、並び順を変える文字や制御文字を持ち込ませない。
    #[test]
    fn invisible_and_control_characters_do_not_reach_the_notification() {
        let notice = notice(
            Language::En,
            &finished(
                Chat::Task(1),
                Some("na\u{202e}me\u{7}"),
                replied("a\u{202e}b\u{200b}c\u{1b}[31md"),
            ),
        );
        assert_eq!(notice.title, "name");
        assert_eq!(notice.body, "abc [31md");
    }

    #[test]
    fn chats_without_a_name_and_replies_without_text_get_fixed_wording() {
        for lang in Language::ALL {
            let general = notice(lang, &finished(Chat::General, None, replied("  \n ")));
            assert_eq!(general.title, i18n::text(lang, "chat.general_title"));
            assert_eq!(general.body, i18n::text(lang, "reply_notice.no_text"));
            assert!(!general.body.starts_with("reply_notice."), "{lang:?}");
            assert!(!general.channel.starts_with("reply_notice."), "{lang:?}");

            let untitled = notice(lang, &finished(Chat::Task(1), None, replied("ok")));
            assert_eq!(untitled.title, i18n::text(lang, "task.untitled"));
            // 見えない文字だけの名前も、名前が無いものとして扱う。
            let blank = notice(
                lang,
                &finished(Chat::Task(1), Some("\u{200b}"), replied("ok")),
            );
            assert_eq!(blank.title, i18n::text(lang, "task.untitled"));
        }
    }

    #[test]
    fn a_failure_is_shown_with_the_wording_of_its_error_message() {
        let notice = notice(
            Language::En,
            &finished(
                Chat::General,
                None,
                TurnOutcome::Failed {
                    kind: "connection_failed".to_string(),
                },
            ),
        );
        assert_eq!(
            notice.body,
            i18n::text(Language::En, "turn_error.connection_failed")
        );
    }

    #[test]
    fn a_requested_chat_must_be_the_general_chat_or_an_existing_task() {
        let conn = db::open_in_memory().unwrap();
        let now = db::now_iso8601();
        conn.execute(
            "INSERT INTO tasks (created_at, updated_at) VALUES (?1, ?1)",
            [&now],
        )
        .unwrap();
        let task_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO tasks (created_at, updated_at, deleted_at) VALUES (?1, ?1, ?1)",
            [&now],
        )
        .unwrap();
        let deleted_id = conn.last_insert_rowid();

        assert_eq!(
            resolve_requested_chat(&conn, CHAT_GENERAL),
            Some(Chat::General)
        );
        assert_eq!(
            resolve_requested_chat(&conn, task_id),
            Some(Chat::Task(task_id))
        );
        assert_eq!(
            resolve_requested_chat(&conn, chat_code(Chat::Task(task_id))),
            Some(Chat::Task(task_id))
        );
        for code in [deleted_id, task_id + 100, CHAT_NONE, -5, i64::MIN, i64::MAX] {
            assert_eq!(resolve_requested_chat(&conn, code), None, "{code}");
        }
    }
}
