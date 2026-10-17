//! 書き込む前の件数(`docs/sync/index.md`)。結合の計画から、書き込む行を書き込まずに組み立て、
//! この端末に書き込むものと、書き込んだあと相手の端末に書き込むことになるもの(4節7)を数える。
//! 通信もDBも触らないので、書き込む前に利用者に確かめさせることができる。

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use scitl_core::CoreError;

use crate::merge::{duplicate_memories, Plan, Pick};
use crate::transport::{self, Transport};

/// 確認に出す数字。分類と集め方は、書き込んだあとの結果の画面の`Counts`と同じ。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct WriteCounts {
    /// 書く・送るタスクの行か工程の数。
    pub tasks: usize,
    /// 書く・送る会話の数(タスクの会話と総合チャット)。
    pub conversations: usize,
    /// 書く・送るメモリの行の数。
    pub memories: usize,
}

/// 書き込む前の確認に出す数字。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct WritePlan {
    /// この端末に書き込むもの。
    pub ours: WriteCounts,
    /// 書き込んだあと、相手の端末に書き込むことになるもの。
    pub theirs: WriteCounts,
}

impl WritePlan {
    /// どちらにも書き込む行が無い(確認を省く)。
    pub(crate) fn empty(&self) -> bool {
        *self == Self::default()
    }
}

/// 運ぶ行の`uid`。書き込んだ行を、変えた行の上に重ねるときに使う。
trait Uid {
    fn uid(&self) -> &str;
}

macro_rules! uid {
    ($row:ty) => {
        impl Uid for $row {
            fn uid(&self) -> &str {
                &self.uid
            }
        }
    };
}
uid!(transport::TaskRow);
uid!(transport::StepRow);
uid!(transport::MemoryRow);
uid!(transport::MessageRow);
uid!(transport::AttachmentRow);

impl transport::MessageRow {
    /// 試行の番号を除いた行。運ぶときの試行は端末ごとに振り直されるので比べないため。
    fn without_attempt(&self) -> Self {
        Self {
            attempt: None,
            ..self.clone()
        }
    }
}

/// 書き込む前の件数。
///
/// `planned`はAに書く行のうち解決の単位を除くもの(計画の行)、`plan`と`picks`は結合の計画と
/// 選んだ側、`changed`は前回Bへ渡した位置より後に変えた行、`incoming`はBが送った行、`alive`は
/// 消えていないメモリの(`uid`,本文)、`message_uids`はAの発言のすべての`uid`(既にある
/// 結合後の全体の記録は置かないので数えない)。
///
/// この端末に書く行は、書くたびに数える(書き込んだあとの`Counts`と同じ集め方)。相手に書き込む
/// ことになる行は、書き込んだ行と変えた行のうち、Bが送ってきた行と同じでないもの(4節7)。送る
/// 集合は書き込んだあとのDBから読む形に組み立てるので、メモリも`uid`ごとに1度だけ数える。
pub(crate) fn counts(
    planned: &Transport,
    plan: &Plan,
    picks: &HashMap<String, Pick>,
    changed: &Transport,
    incoming: &Transport,
    alive: &[(String, String)],
    message_uids: &HashSet<String>,
) -> Result<WritePlan, CoreError> {
    // 解決の単位の行。数字を数えるための値で、版の番号と日時は書き込むときに決まるものを使わない
    // (集め方と、その行が消えるかどうかには現れない)。書く行・送る行とも重複は起きない。
    let resolved = crate::merge::resolution_rows(plan, picks, "", 0, "")?;

    // この端末に書くもの。
    let folded = folded(planned, &resolved, alive);
    let mut ours = Written::default();
    ours.absorb(planned);
    ours.absorb(&resolved);
    absorb_records(plan, message_uids, &mut ours.conversations);
    ours.memories += folded.len();

    // 送り返す行(4節7)。書き込んだ行を変えた行の上に重ね、Bが送ってきた行と同じものを除く。
    let back = Transport {
        tasks: overlay(&changed.tasks, &planned.tasks),
        steps: overlay(&changed.steps, &planned.steps),
        memories: overlay(&changed.memories, &planned.memories),
        messages: overlay(&changed.messages, &planned.messages),
        attachments: overlay(&changed.attachments, &planned.attachments),
    };
    let back = without_echo(back, incoming);

    // 相手の端末に書き込むことになるもの。送る行は`uid`ごとに1度だけ数える。
    let mut theirs = Sent::default();
    theirs.absorb(&back);
    theirs.absorb(&resolved);
    absorb_records(plan, message_uids, &mut theirs.conversations);
    for uid in folded {
        theirs.memories.insert(uid);
    }

    Ok(WritePlan {
        ours: WriteCounts {
            tasks: ours.tasks.len(),
            conversations: ours.conversations.len(),
            memories: ours.memories,
        },
        theirs: WriteCounts {
            tasks: theirs.tasks.len(),
            conversations: theirs.conversations.len(),
            memories: theirs.memories.len(),
        },
    })
}

/// `base`に`take`の行を重ねる。同じ`uid`は`take`の行(書き込んだ行)が残る。
fn overlay<R: Uid + Clone>(base: &[R], take: &[R]) -> Vec<R> {
    let replaced: HashSet<&str> = take.iter().map(Uid::uid).collect();
    let mut out: Vec<R> = base
        .iter()
        .filter(|r| !replaced.contains(r.uid()))
        .cloned()
        .collect();
    out.extend(take.iter().cloned());
    out
}

/// Bが送ってきた行と同じ行を、送り返す行から除く。試行の番号は端末ごとに振り直されるので比べない
/// (9節)。確定での書き込みから、返す行に集めるためにも使う。
pub(crate) fn without_echo(mut written: Transport, incoming: &Transport) -> Transport {
    fn keep<R>(
        rows: &mut Vec<R>,
        incoming: &[R],
        uid: impl Fn(&R) -> &str,
        same: impl Fn(&R, &R) -> bool,
    ) {
        let sent: HashMap<&str, &R> = incoming.iter().map(|r| (uid(r), r)).collect();
        rows.retain(|r| !sent.get(uid(r)).is_some_and(|s| same(s, r)));
    }
    keep(&mut written.tasks, &incoming.tasks, |r| &r.uid, PartialEq::eq);
    keep(&mut written.steps, &incoming.steps, |r| &r.uid, PartialEq::eq);
    keep(
        &mut written.memories,
        &incoming.memories,
        |r| &r.uid,
        PartialEq::eq,
    );
    keep(
        &mut written.messages,
        &incoming.messages,
        |r| &r.uid,
        |a, b| a.without_attempt() == b.without_attempt(),
    );
    keep(
        &mut written.attachments,
        &incoming.attachments,
        |r| &r.uid,
        PartialEq::eq,
    );
    written
}

/// 書き込んだあとに畳む(書き直す)メモリの`uid`。同じ本文の`uid`の小さい方を残す(8節)。
/// 書き込む行と消えていないメモリから、書き込んだあとの重なりを組み立てて数える。
fn folded(planned: &Transport, resolved: &Transport, alive: &[(String, String)]) -> Vec<String> {
    let mut after: HashMap<String, String> = alive.iter().cloned().collect();
    for r in planned.memories.iter().chain(resolved.memories.iter()) {
        match r.deleted_at {
            None => {
                after.insert(r.uid.clone(), r.content.clone());
            }
            Some(_) => {
                after.remove(&r.uid);
            }
        }
    }
    duplicate_memories(after.iter().map(|(uid, c)| (uid.as_str(), c.as_str())))
        .into_iter()
        .map(str::to_string)
        .collect()
}

/// 既にある結合後の全体の記録は書かないので数えない(8節)。
fn absorb_records(
    plan: &Plan,
    message_uids: &HashSet<String>,
    conversations: &mut HashSet<Option<String>>,
) {
    for record in &plan.records {
        if !message_uids.contains(&record.uid) {
            conversations.insert(Some(record.task.clone()));
        }
    }
}

/// この端末に書くもの。書くたびに数える(`Counts`の集め方。実行記録を含む発言も1行ずつ書く)。
#[derive(Default)]
struct Written {
    tasks: HashSet<String>,
    conversations: HashSet<Option<String>>,
    memories: usize,
}

impl Written {
    fn absorb(&mut self, t: &Transport) {
        for r in &t.tasks {
            self.tasks.insert(r.uid.clone());
        }
        for r in &t.steps {
            self.tasks.insert(r.task.clone());
        }
        self.memories += t.memories.len();
        for r in &t.messages {
            self.conversations.insert(r.task.clone());
        }
    }
}

/// 相手の端末に書き込むことになるものは、行ごとに1度だけ運ばれるので、`uid`ごとに数える。
#[derive(Default)]
struct Sent {
    tasks: HashSet<String>,
    conversations: HashSet<Option<String>>,
    memories: HashSet<String>,
}

impl Sent {
    fn absorb(&mut self, t: &Transport) {
        for r in &t.tasks {
            self.tasks.insert(r.uid.clone());
        }
        for r in &t.steps {
            self.tasks.insert(r.task.clone());
        }
        for r in &t.memories {
            self.memories.insert(r.uid.clone());
        }
        for r in &t.messages {
            self.conversations.insert(r.task.clone());
        }
    }
}
