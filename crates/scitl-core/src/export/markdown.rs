//! 書き出すMarkdownの組み立て。書き出す値の無害化はここに閉じる。
//!
//! - 利用者・モデルが決められる1行の値(タイトル・締切・工程・添付名等)は[`inline_text`]を通す。
//!   SCITL自身が作る値(日時・中身から決めたMIME)はそのまま書く
//! - 複数行の本文(発言・説明・ツール実行記録)は[`fenced`]で囲み、中身は変えない
//! - リンク先はこのファイルが組み立てる相対パスだけで、区間ごとに[`encode_path_segment`]を通す
//!
//! 固定文言は英語で書く。

use std::fmt::Write;

use percent_encoding::{utf8_percent_encode, AsciiSet};

use crate::db::attachments::AttachmentKind;
use crate::db::messages::{Kind, Message, Role};
use crate::db::task_steps::TaskStep;
use crate::db::tasks::Task;
use crate::text::{encode_all_but, visible_line};

/// 会話の1行と、その発言に付いた添付。
pub(super) struct Entry<'a> {
    pub message: &'a Message,
    pub attachments: Vec<AttachmentLink>,
}

/// 書き出した添付を指すリンク。
pub(super) struct AttachmentLink {
    pub name: String,
    pub kind: AttachmentKind,
    pub mime_type: String,
    pub size_bytes: i64,
    /// 書き出したファイルの、Markdownファイルからの相対パスの区間(符号化する前)。実体を
    /// 読めず書き出せなかった添付は`None`。
    pub path: Option<Vec<String>>,
}

pub(super) fn render_task(task: &Task, steps: &[TaskStep], conversation: &[Entry]) -> String {
    let mut out = String::new();
    let title = task
        .title
        .as_deref()
        .map_or_else(|| "Untitled".to_string(), inline_text);
    let _ = writeln!(out, "# Task {}: {title}\n", task.id);

    let status = match &task.archived_at {
        Some(at) => format!("archived ({at})"),
        None => "active".to_string(),
    };
    let deadline = task
        .deadline
        .as_deref()
        .map_or_else(|| "none".to_string(), inline_text);
    let _ = writeln!(out, "- Status: {status}");
    let _ = writeln!(out, "- Deadline: {deadline}");
    let _ = writeln!(out, "- Created: {}", task.created_at);
    let _ = writeln!(out, "- Updated: {}\n", task.updated_at);

    out.push_str("## Description\n\n");
    match &task.description {
        Some(description) => out.push_str(&fenced("markdown", description)),
        None => out.push_str("None\n"),
    }
    out.push('\n');

    out.push_str("## Steps\n\n");
    if steps.is_empty() {
        out.push_str("None\n");
    }
    for step in steps {
        let mark = if step.done_at.is_some() { 'x' } else { ' ' };
        let _ = writeln!(out, "- [{mark}] {}", inline_text(&step.description));
    }
    out.push('\n');

    push_conversation(&mut out, conversation);
    out
}

pub(super) fn render_general_chat(conversation: &[Entry]) -> String {
    let mut out = String::from("# General chat\n\n");
    push_conversation(&mut out, conversation);
    out
}

/// 並びは渡された順のまま。ツール実行記録は属するターンの返信より前に保存されているので、
/// 取得した順に並べれば発生順になる。
fn push_conversation(out: &mut String, conversation: &[Entry]) {
    out.push_str("## Conversation\n\n");
    if conversation.is_empty() {
        out.push_str("None\n");
    }
    for entry in conversation {
        push_entry(out, entry);
    }
}

fn push_entry(out: &mut String, entry: &Entry) {
    let message = entry.message;
    let _ = writeln!(out, "### {} ({})\n", speaker(message), message.created_at);

    if message.kind == Kind::ToolExecution {
        // 読めない値はCHECK制約(`json_valid`)で入らないが、読めなければ保存値のまま出す。
        let pretty = serde_json::from_str::<serde_json::Value>(&message.content)
            .and_then(|v| serde_json::to_string_pretty(&v))
            .unwrap_or_else(|_| message.content.clone());
        out.push_str(&fenced("json", &pretty));
        out.push('\n');
    } else if !message.content.trim().is_empty() {
        // 失敗したターンで受け取り終えた本文。画面と同じく、エラーの文言より前に置く。
        if let Some(partial) = &message.partial_reply {
            out.push_str(&fenced("markdown", partial));
            out.push('\n');
        }
        let info = if message.role == Role::Error {
            "text"
        } else {
            "markdown"
        };
        out.push_str(&fenced(info, &message.content));
        out.push('\n');
    }

    if !entry.attachments.is_empty() {
        out.push_str("Attachments:\n\n");
        for attachment in &entry.attachments {
            let _ = writeln!(out, "- {}", attachment_item(attachment));
        }
        out.push('\n');
    }
}

fn speaker(message: &Message) -> String {
    match (message.role, &message.source) {
        (Role::User, _) => "User".to_string(),
        (Role::Assistant, _) => "Assistant".to_string(),
        (Role::Error, _) => "Error".to_string(),
        (Role::Tool, Some(source)) => format!("Operation ({})", inline_text(source)),
        (Role::Tool, None) => "Tool execution".to_string(),
    }
}

fn attachment_item(attachment: &AttachmentLink) -> String {
    let mut name = inline_text(&attachment.name);
    if name.is_empty() {
        name = "(unnamed)".to_string();
    }
    let facts = format!(
        "{}, size in bytes: {}",
        attachment.mime_type, attachment.size_bytes
    );
    match &attachment.path {
        Some(segments) => {
            let target: Vec<String> = segments.iter().map(|s| encode_path_segment(s)).collect();
            let bang = if attachment.kind == AttachmentKind::Image {
                "!"
            } else {
                ""
            };
            format!("{bang}[{name}]({}) ({facts})", target.join("/"))
        }
        None => format!("{name} ({facts}, not exported)"),
    }
}

/// 1行の自由入力を、Markdownの構造として読まれない形にする。見えない書式文字を除いて1行に畳み
/// ([`visible_line`])、ASCIIの句読点をバックスラッシュでエスケープする。CommonMarkはASCIIの
/// 句読点すべてのエスケープを認めるので、文字の種類を選ばずに一律に掛ければ、見出し・リスト・
/// 強調・リンク・画像・生HTML・表・実体参照のどれにもならない。
///
/// 括弧だけは例外にする。`\(`・`\[`は、数式を描くビューア(MathJax・KaTeX)が数式の区切りとして
/// 読む。`(`・`)`はそれだけでは構造を作らないので残し、`[`・`]`は数値文字参照にする(文字参照は
/// 構造を作らない)。
fn inline_text(s: &str) -> String {
    let line = visible_line(s);
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        match c {
            '(' | ')' => out.push(c),
            '[' => out.push_str("&#91;"),
            ']' => out.push_str("&#93;"),
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// 複数行の本文を、中身を変えずにコードブロックで囲む。フェンスは中身に現れるバッククォートの
/// 最長の連続より長くするので、中身のどの行も閉じのフェンスにならない。フェンスの中は
/// 生HTML・画像記法を含めて何も解釈されない。
fn fenced(info: &str, body: &str) -> String {
    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest + 1).max(3));
    let newline = if body.ends_with('\n') { "" } else { "\n" };
    format!("{fence}{info}\n{body}{newline}{fence}\n")
}

/// リンク先のパスの1区間。RFC 3986の非予約文字だけを残し、ほかはUTF-8のバイトごとに符号化する。
/// 空白・括弧・`<`等をリンク先の構文に触れさせないため。
fn encode_path_segment(segment: &str) -> String {
    const NOT_UNRESERVED: &AsciiSet = &encode_all_but(b"-._~");
    utf8_percent_encode(segment, NOT_UNRESERVED).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: Role, kind: Kind, content: &str, source: Option<&str>) -> Message {
        Message {
            id: 1,
            task_id: Some(1),
            role,
            content: content.to_string(),
            kind,
            source: source.map(str::to_string),
            reasoning: Some("secret thoughts".to_string()),
            error_kind: None,
            error_detail: Some("provider body".to_string()),
            partial_reply: None,
            turn_id: None,
            attempt_no: None,
            created_at: "2026-09-28T12:00:00Z".to_string(),
            attachments: Vec::new(),
        }
    }

    fn task(title: Option<&str>) -> Task {
        Task {
            id: 12,
            title: title.map(str::to_string),
            description: None,
            deadline: None,
            archived_at: None,
            deleted_at: None,
            created_at: "2026-09-01T00:00:00Z".to_string(),
            updated_at: "2026-09-02T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn free_input_cannot_form_markdown_structure() {
        assert_eq!(inline_text("# heading"), "\\# heading");
        assert_eq!(inline_text("- item"), "\\- item");
        assert_eq!(inline_text("1. item"), "1\\. item");
        assert_eq!(
            inline_text("<script>alert(1)</script>"),
            "\\<script\\>alert(1)\\<\\/script\\>"
        );
        assert_eq!(
            inline_text("![x](http://e.test/t.png)"),
            "\\!&#91;x&#93;(http\\:\\/\\/e\\.test\\/t\\.png)"
        );
        assert_eq!(inline_text("&lt;"), "\\&lt\\;");
        assert_eq!(inline_text("a | b"), "a \\| b");
        // 数式の区切り(`\\(`・`\\[`)を作らない。
        assert_eq!(inline_text("f(x) [a]"), "f(x) &#91;a&#93;");
        assert_eq!(
            inline_text("[[note]] ![[embed]]"),
            "&#91;&#91;note&#93;&#93; \\!&#91;&#91;embed&#93;&#93;"
        );
    }

    #[test]
    fn free_input_is_folded_to_one_visible_line() {
        assert_eq!(inline_text("a\n## b"), "a \\#\\# b");
        assert_eq!(inline_text("a\u{202E}b\u{200B}c"), "abc");
        assert_eq!(inline_text("日本語のタイトル"), "日本語のタイトル");
    }

    #[test]
    fn fence_outlasts_every_backtick_run_in_the_body() {
        assert_eq!(fenced("markdown", "plain"), "```markdown\nplain\n```\n");
        assert_eq!(
            fenced("markdown", "a\n````\nb\n"),
            "`````markdown\na\n````\nb\n`````\n"
        );
        let body = format!("x {} y", "`".repeat(10));
        assert!(fenced("text", &body).starts_with(&format!("{}text\n", "`".repeat(11))));
    }

    #[test]
    fn body_is_kept_verbatim_inside_the_fence() {
        let body = "## Assistant (2026-01-01T00:00:00Z)\n<img src=\"http://e.test/x\">\n![a](http://e.test/b.png)";
        let out = render_general_chat(&[Entry {
            message: &message(Role::User, Kind::Normal, body, None),
            attachments: Vec::new(),
        }]);
        assert!(
            out.contains(&format!("```markdown\n{body}\n```\n")),
            "{out}"
        );
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(
            encode_path_segment("report v2 (final).pdf"),
            "report%20v2%20%28final%29.pdf"
        );
        assert_eq!(encode_path_segment("資料.txt"), "%E8%B3%87%E6%96%99.txt");
        assert_eq!(encode_path_segment("a_b-c.~"), "a_b-c.~");
    }

    #[test]
    fn task_file_lists_metadata_steps_and_conversation() {
        let mut t = task(Some("# 買い物"));
        t.description = Some("牛乳を買う\n- 2本".to_string());
        t.deadline = Some("2026-10-01".to_string());
        t.archived_at = Some("2026-09-03T00:00:00Z".to_string());
        let step = |description: &str, done: bool| TaskStep {
            id: 1,
            task_id: 12,
            description: description.to_string(),
            done_at: done.then(|| "2026-09-02T00:00:00Z".to_string()),
            order_index: 0,
            created_at: "2026-09-01T00:00:00Z".to_string(),
        };
        let user = message(Role::User, Kind::Normal, "hello", None);
        let record = message(
            Role::Tool,
            Kind::ToolExecution,
            r#"{"tool":"update_task","arguments":{},"result":{}}"#,
            None,
        );
        let op = message(
            Role::Tool,
            Kind::ToolExecution,
            r#"{"tool":"delete_task"}"#,
            Some("ui"),
        );
        let reply = message(Role::Assistant, Kind::Normal, "done", None);
        let out = render_task(
            &t,
            &[step("店へ行く", true), step("- 払う", false)],
            &[
                Entry {
                    message: &user,
                    attachments: Vec::new(),
                },
                Entry {
                    message: &record,
                    attachments: Vec::new(),
                },
                Entry {
                    message: &op,
                    attachments: Vec::new(),
                },
                Entry {
                    message: &reply,
                    attachments: Vec::new(),
                },
            ],
        );

        assert!(out.starts_with("# Task 12: \\# 買い物\n"), "{out}");
        assert!(out.contains("- Status: archived (2026-09-03T00:00:00Z)\n"));
        assert!(out.contains("- Deadline: 2026\\-10\\-01\n"));
        assert!(out.contains("```markdown\n牛乳を買う\n- 2本\n```\n"));
        assert!(out.contains("- [x] 店へ行く\n- [ ] \\- 払う\n"));
        let order: Vec<_> = [
            "### User",
            "### Tool execution",
            "### Operation (ui)",
            "### Assistant",
        ]
        .iter()
        .map(|h| {
            out.find(h)
                .unwrap_or_else(|| panic!("{h} missing in {out}"))
        })
        .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{out}");
        assert!(out.contains("```json\n{\n  \"arguments\": {},"), "{out}");
        assert!(!out.contains("secret thoughts"));
        assert!(!out.contains("provider body"));
    }

    /// 失敗したターンで受け取り終えた本文は、エラーの文言より前に出す。
    #[test]
    fn a_failed_reply_keeps_the_text_received_before_the_error() {
        let mut error = message(Role::Error, Kind::Normal, "The request failed.", None);
        error.partial_reply = Some("工程を足します".to_string());
        let out = render_general_chat(&[Entry {
            message: &error,
            attachments: Vec::new(),
        }]);

        let partial = out.find("```markdown\n工程を足します\n```\n").unwrap();
        let failure = out.find("```text\nThe request failed.\n```\n").unwrap();
        assert!(partial < failure, "{out}");
    }

    #[test]
    fn untitled_task_and_empty_sections_say_so() {
        let out = render_task(&task(None), &[], &[]);
        assert!(out.starts_with("# Task 12: Untitled\n"));
        assert!(out.contains("## Description\n\nNone\n"));
        assert!(out.contains("## Steps\n\nNone\n"));
        assert!(out.contains("## Conversation\n\nNone\n"));
    }

    #[test]
    fn attachments_link_to_the_written_files() {
        let user = message(Role::User, Kind::Normal, " ", None);
        let link = |name: &str, kind, path: Option<Vec<String>>| AttachmentLink {
            name: name.to_string(),
            kind,
            mime_type: "image/png".to_string(),
            size_bytes: 3,
            path,
        };
        let out = render_general_chat(&[Entry {
            message: &user,
            attachments: vec![
                link(
                    "a b.png",
                    AttachmentKind::Image,
                    Some(vec!["attachments".into(), "5".into(), "a b.png".into()]),
                ),
                link(
                    "](x)",
                    AttachmentKind::Other,
                    Some(vec!["attachments".into(), "6".into(), "](x)".into()]),
                ),
                link("gone.bin", AttachmentKind::Other, None),
            ],
        }]);
        // 空白だけの本文(添付だけを送った発言)はコードブロックにしない。
        assert!(!out.contains("```"), "{out}");
        assert!(
            out.contains("- ![a b\\.png](attachments/5/a%20b.png) (image/png, size in bytes: 3)\n"),
            "{out}"
        );
        assert!(
            out.contains("- [&#93;(x)](attachments/6/%5D%28x%29) (image/png, size in bytes: 3)\n"),
            "{out}"
        );
        assert!(
            out.contains("- gone\\.bin (image/png, size in bytes: 3, not exported)\n"),
            "{out}"
        );
    }
}
