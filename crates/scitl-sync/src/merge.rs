//! 結合(`docs/sync/index.md` 8節・9節)。待ち受ける側(A)が、受け取った行(B)と自分の行だけから
//! 結果を決める。通信もDBも触らず、運ぶ形(`uid`で指し合う行)の上で決める。
//!
//! 単位(タスク・総合チャット)は、Bから差分だけが届いていれば、行ごとに版で決める。両方がその
//! 単位を変えていたときと、版で決まらない行があるときは、単位の行をすべて求めてから([`Plan::needs`])
//! 単位ごとに決める。会話の形と、削除と知らずにした変更は、単位のすべての行を見ないと分からない
//! (差分だけでは、相手が持っていない行を知りようがない)。

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::transport::{
    AttachmentRow, MemoryRow, MessageRow, StepRow, TaskRow, Transport, Version,
};
use crate::InvalidRow;

/// 2つの版の関係。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Order {
    Same,
    /// 左が右を含む(全項目で右以上で、どこかが大きい)。
    Newer,
    Older,
    /// どちらも含まない(別々に変えた)。
    Concurrent,
}

pub(crate) fn compare(a: &Version, b: &Version) -> Order {
    let mut greater = false;
    let mut less = false;
    for device in a.keys().chain(b.keys()) {
        let x = a.get(device).copied().unwrap_or(0);
        let y = b.get(device).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Greater => greater = true,
            Ordering::Less => less = true,
            Ordering::Equal => {}
        }
    }
    match (greater, less) {
        (false, false) => Order::Same,
        (true, false) => Order::Newer,
        (false, true) => Order::Older,
        (true, true) => Order::Concurrent,
    }
}

/// 両方の最大。
pub(crate) fn join(a: &Version, b: &Version) -> Version {
    let mut out = a.clone();
    for (device, n) in b {
        let entry = out.entry(device.clone()).or_insert(0);
        *entry = (*entry).max(*n);
    }
    out
}

/// 状態を表す日時は、どちらにもあれば早い方(同じ操作を2台でしても揃わない。8節)。
fn earlier(value: &mut Option<String>, other: &Option<String>) {
    if let (Some(v), Some(o)) = (value.as_mut(), other) {
        if o < v {
            *v = o.clone();
        }
    }
}

fn later(value: &mut String, other: &str) {
    if other > value.as_str() {
        *value = other.to_string();
    }
}

/// 版を持つ行。比べる欄と日時の扱いは`docs/sync/index.md` 8節の表。
pub(crate) trait Versioned: Clone + PartialEq {
    fn uid(&self) -> &str;
    fn version(&self) -> &Version;
    fn version_mut(&mut self) -> &mut Version;
    /// 状態を表す欄(比べる欄)が同じか。
    fn same_state(&self, other: &Self) -> bool;
    /// 日時を相手と合わせる(状態を表す日時は早い方、`updated_at`は遅い方)。
    fn merge_dates(&mut self, other: &Self);
    /// 書いたあと変わらない列が同じか。
    fn same_fixed(&self, other: &Self) -> bool;
    fn is_deleted(&self) -> bool;
    fn delete(&mut self, now: &str);
    const TABLE: &'static str;
}

impl Versioned for TaskRow {
    const TABLE: &'static str = "tasks";
    fn uid(&self) -> &str {
        &self.uid
    }
    fn version(&self) -> &Version {
        &self.version
    }
    fn version_mut(&mut self) -> &mut Version {
        &mut self.version
    }
    fn same_state(&self, o: &Self) -> bool {
        self.title == o.title
            && self.description == o.description
            && self.deadline == o.deadline
            && self.archived_at.is_some() == o.archived_at.is_some()
            && self.deleted_at.is_some() == o.deleted_at.is_some()
    }
    fn merge_dates(&mut self, o: &Self) {
        earlier(&mut self.archived_at, &o.archived_at);
        earlier(&mut self.deleted_at, &o.deleted_at);
        later(&mut self.updated_at, &o.updated_at);
    }
    fn same_fixed(&self, o: &Self) -> bool {
        self.created_at == o.created_at
    }
    fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
    fn delete(&mut self, now: &str) {
        self.deleted_at.get_or_insert_with(|| now.to_string());
    }
}

impl Versioned for StepRow {
    const TABLE: &'static str = "task_steps";
    fn uid(&self) -> &str {
        &self.uid
    }
    fn version(&self) -> &Version {
        &self.version
    }
    fn version_mut(&mut self) -> &mut Version {
        &mut self.version
    }
    fn same_state(&self, o: &Self) -> bool {
        self.description == o.description
            && self.done_at.is_some() == o.done_at.is_some()
            && self.deleted_at.is_some() == o.deleted_at.is_some()
    }
    fn merge_dates(&mut self, o: &Self) {
        earlier(&mut self.done_at, &o.done_at);
        earlier(&mut self.deleted_at, &o.deleted_at);
    }
    fn same_fixed(&self, o: &Self) -> bool {
        self.task == o.task && self.order_index == o.order_index && self.created_at == o.created_at
    }
    fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
    fn delete(&mut self, now: &str) {
        self.deleted_at.get_or_insert_with(|| now.to_string());
    }
}

impl Versioned for MemoryRow {
    const TABLE: &'static str = "memories";
    fn uid(&self) -> &str {
        &self.uid
    }
    fn version(&self) -> &Version {
        &self.version
    }
    fn version_mut(&mut self) -> &mut Version {
        &mut self.version
    }
    fn same_state(&self, o: &Self) -> bool {
        self.content == o.content && self.deleted_at.is_some() == o.deleted_at.is_some()
    }
    fn merge_dates(&mut self, o: &Self) {
        earlier(&mut self.deleted_at, &o.deleted_at);
        later(&mut self.updated_at, &o.updated_at);
    }
    fn same_fixed(&self, o: &Self) -> bool {
        self.created_at == o.created_at
    }
    fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
    fn delete(&mut self, now: &str) {
        self.deleted_at.get_or_insert_with(|| now.to_string());
    }
}

impl Versioned for MessageRow {
    const TABLE: &'static str = "messages";
    fn uid(&self) -> &str {
        &self.uid
    }
    fn version(&self) -> &Version {
        &self.version
    }
    fn version_mut(&mut self) -> &mut Version {
        &mut self.version
    }
    fn same_state(&self, o: &Self) -> bool {
        self.deleted_at.is_some() == o.deleted_at.is_some()
    }
    fn merge_dates(&mut self, o: &Self) {
        earlier(&mut self.deleted_at, &o.deleted_at);
    }
    /// 試行(`attempt`)は端末ごとに見え方が違いうるので比べない。
    fn same_fixed(&self, o: &Self) -> bool {
        self.task == o.task
            && self.role == o.role
            && self.kind == o.kind
            && self.content == o.content
            && self.source == o.source
            && self.error_kind == o.error_kind
            && self.error_detail == o.error_detail
            && self.parts == o.parts
            && self.turn_id == o.turn_id
            && self.created_at == o.created_at
            && self.prev_uid == o.prev_uid
    }
    fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
    fn delete(&mut self, now: &str) {
        self.deleted_at.get_or_insert_with(|| now.to_string());
    }
}

/// 2つの行を版で結合する。決まらなければ(版が並んで値が違う、同じ版で値が違う)`None`。
/// 自分の分は進めない(どの端末で結合しても同じ結果にする。8節)。
pub(crate) fn merge_pair<R: Versioned>(a: &R, b: &R) -> Option<R> {
    let (mut out, other) = match compare(a.version(), b.version()) {
        // 同じ版で値が違うのは、端末IDの重複の印(10節)。競合として扱う。
        Order::Same if !a.same_state(b) => return None,
        Order::Same | Order::Newer => (a.clone(), b),
        Order::Older => (b.clone(), a),
        Order::Concurrent if a.same_state(b) => {
            let mut out = a.clone();
            *out.version_mut() = join(a.version(), b.version());
            (out, b)
        }
        Order::Concurrent => return None,
    };
    out.merge_dates(other);
    Some(out)
}

/// 単位。競合はこの単位で選ばせる(9節)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Unit {
    Task(String),
    General,
    Memory(String),
}

impl Unit {
    pub(crate) fn id(&self) -> String {
        match self {
            Self::Task(uid) => format!("task:{uid}"),
            Self::General => "general".to_string(),
            Self::Memory(uid) => format!("memory:{uid}"),
        }
    }

    fn of_chat(task: &Option<String>) -> Self {
        match task {
            Some(uid) => Self::Task(uid.clone()),
            None => Self::General,
        }
    }
}

/// 単位の集まり(タスクと総合チャット。メモリは行ごとに決まるので入らない)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Units {
    pub tasks: BTreeSet<String>,
    pub general: bool,
}

impl Units {
    pub(crate) fn contains(&self, unit: &Unit) -> bool {
        match unit {
            Unit::Task(uid) => self.tasks.contains(uid),
            Unit::General => self.general,
            Unit::Memory(_) => false,
        }
    }

    pub(crate) fn insert(&mut self, unit: Unit) {
        match unit {
            Unit::Task(uid) => {
                self.tasks.insert(uid);
            }
            Unit::General => self.general = true,
            Unit::Memory(_) => {}
        }
    }

    pub(crate) fn extend(&mut self, other: &Units) {
        self.tasks.extend(other.tasks.iter().cloned());
        self.general |= other.general;
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tasks.is_empty() && !self.general
    }

    /// 行の単位をすべて集める(メモリと添付は入れない)。
    pub(crate) fn of(t: &Transport) -> Self {
        let mut units = Self::default();
        for r in &t.tasks {
            units.insert(Unit::Task(r.uid.clone()));
        }
        for r in &t.steps {
            units.insert(Unit::Task(r.task.clone()));
        }
        for r in &t.messages {
            units.insert(Unit::of_chat(&r.task));
        }
        units
    }
}

/// 利用者に選ばせる単位。両側のその単位の行(メモリは行1つ)を持つ。
#[derive(Debug, Clone)]
pub(crate) struct Conflict {
    pub unit: Unit,
    /// Aの行。
    pub ours: Transport,
    /// Bの行。
    pub theirs: Transport,
}

/// どちらの側を取るか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pick {
    Ours,
    Theirs,
}

/// 結合後のタスクの全体の記録(8節)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskStateRecord {
    pub task: String,
    pub uid: String,
    pub created_at: String,
}

/// 結合の結果。
#[derive(Debug, Default)]
pub(crate) struct Plan {
    /// 単位の行をすべて求めてからでないと決まらない単位。空でなければ、ほかは使わない。
    pub needs: Units,
    /// Aに書く行(足す行と、変える行)。発言は会話ごとにBの並びの順。
    pub rows: Transport,
    pub conflicts: Vec<Conflict>,
    pub records: Vec<TaskStateRecord>,
    /// 同じ`uid`で、書いたあと変わらない列が違う行。
    pub invalid: Vec<InvalidRow>,
}

/// 結合に使うもの。
pub(crate) struct Inputs<'a> {
    /// Bから受け取った行(差分と、求めた単位の行)。
    pub incoming: &'a Transport,
    /// Aの行のうち、`incoming`と同じ`uid`の行と、`complete`の単位のすべての行。
    pub local: &'a Transport,
    /// 両側の行がすべてそろっている単位。
    pub complete: &'a Units,
    /// Aが前回Bへ渡した位置より後に変えた単位。
    pub changed_here: &'a Units,
}

pub(crate) fn plan(inputs: &Inputs<'_>) -> Plan {
    let mut plan = Plan::default();
    let local = Index::of(inputs.local);

    // 書いたあと変わらない列が違う行は、どの単位にあっても同期を断る。
    fixed_columns(&inputs.incoming.tasks, &local.tasks, &mut plan.invalid);
    fixed_columns(&inputs.incoming.steps, &local.steps, &mut plan.invalid);
    fixed_columns(
        &inputs.incoming.memories,
        &local.memories,
        &mut plan.invalid,
    );
    fixed_columns(
        &inputs.incoming.messages,
        &local.messages,
        &mut plan.invalid,
    );
    for r in &inputs.incoming.attachments {
        if local
            .attachments
            .get(r.uid.as_str())
            .is_some_and(|l| *l != r)
        {
            plan.invalid
                .push(fixed_column_differs("attachments", &r.uid));
        }
    }
    if !plan.invalid.is_empty() {
        return plan;
    }

    // 両方が変えた単位と、版で決まらない行のある単位は、単位の行をすべて見る。
    for unit in units_with_rows(inputs.incoming) {
        if inputs.complete.contains(&unit) {
            continue;
        }
        if inputs.changed_here.contains(&unit) || undecided(&unit, inputs.incoming, &local) {
            plan.needs.insert(unit);
        }
    }
    if !plan.needs.is_empty() {
        return plan;
    }

    let mut out = Out::default();
    for unit in units_with_rows(inputs.incoming) {
        if inputs.complete.contains(&unit) {
            whole_unit(&unit, inputs, &local, &mut out, &mut plan);
        } else {
            by_row(&unit, inputs.incoming, &local, &mut out);
        }
    }
    for b in &inputs.incoming.memories {
        match local.memories.get(b.uid.as_str()) {
            None => out.memories.push(b.clone()),
            Some(a) => match merge_pair(*a, b) {
                Some(m) if m != **a => out.memories.push(m),
                Some(_) => {}
                None => plan.conflicts.push(Conflict {
                    unit: Unit::Memory(b.uid.clone()),
                    ours: Transport {
                        memories: vec![(*a).clone()],
                        ..Transport::default()
                    },
                    theirs: Transport {
                        memories: vec![b.clone()],
                        ..Transport::default()
                    },
                }),
            },
        }
    }
    // 添付は書いたあと変わらないので、無いものを足すだけ。
    for b in &inputs.incoming.attachments {
        if !local.attachments.contains_key(b.uid.as_str()) {
            out.attachments.push(b.clone());
        }
    }
    plan.rows = out.into_transport(inputs.incoming);
    plan
}

/// 単位を、`incoming`に現れる順に(同じ単位は1度だけ)。
fn units_with_rows(t: &Transport) -> Vec<Unit> {
    let mut seen = HashSet::new();
    let mut units = Vec::new();
    let mut push = |unit: Unit| {
        if seen.insert(unit.clone()) {
            units.push(unit);
        }
    };
    for r in &t.tasks {
        push(Unit::Task(r.uid.clone()));
    }
    for r in &t.steps {
        push(Unit::Task(r.task.clone()));
    }
    for r in &t.messages {
        push(Unit::of_chat(&r.task));
    }
    units
}

fn fixed_column_differs(table: &'static str, uid: &str) -> InvalidRow {
    InvalidRow {
        table,
        id: None,
        uid: uid.to_string(),
        reason: "a column that never changes differs from this device's row".to_string(),
    }
}

fn fixed_columns<R: Versioned>(
    incoming: &[R],
    local: &HashMap<&str, &R>,
    invalid: &mut Vec<InvalidRow>,
) {
    for r in incoming {
        if local.get(r.uid()).is_some_and(|l| !l.same_fixed(r)) {
            invalid.push(fixed_column_differs(R::TABLE, r.uid()));
        }
    }
}

/// 版で決まらない行がその単位にあるか。
fn undecided(unit: &Unit, incoming: &Transport, local: &Index<'_>) -> bool {
    fn any<R: Versioned>(
        rows: &[R],
        local: &HashMap<&str, &R>,
        in_unit: impl Fn(&R) -> bool,
    ) -> bool {
        rows.iter().filter(|r| in_unit(r)).any(|b| {
            local
                .get(b.uid())
                .is_some_and(|a| merge_pair(*a, b).is_none())
        })
    }
    match unit {
        Unit::Task(task) => {
            any(&incoming.tasks, &local.tasks, |r| &r.uid == task)
                || any(&incoming.steps, &local.steps, |r| &r.task == task)
                || any(&incoming.messages, &local.messages, |r| {
                    r.task.as_ref() == Some(task)
                })
        }
        Unit::General => any(&incoming.messages, &local.messages, |r| r.task.is_none()),
        Unit::Memory(_) => false,
    }
}

/// 差分だけが届いた単位。Aはこの単位を前回から変えていないので、Bの行は版でAの行を含むか
/// 同じになる。行ごとに版で決める。
fn by_row(unit: &Unit, incoming: &Transport, local: &Index<'_>, out: &mut Out) {
    fn each<R: Versioned>(
        rows: &[R],
        local: &HashMap<&str, &R>,
        in_unit: impl Fn(&R) -> bool,
        out: &mut Vec<R>,
    ) {
        for b in rows.iter().filter(|r| in_unit(r)) {
            match local.get(b.uid()) {
                None => out.push(b.clone()),
                Some(a) => {
                    if let Some(m) = merge_pair(*a, b).filter(|m| m != *a) {
                        out.push(m);
                    }
                }
            }
        }
    }
    match unit {
        Unit::Task(task) => {
            each(
                &incoming.tasks,
                &local.tasks,
                |r| &r.uid == task,
                &mut out.tasks,
            );
            each(
                &incoming.steps,
                &local.steps,
                |r| &r.task == task,
                &mut out.steps,
            );
            each(
                &incoming.messages,
                &local.messages,
                |r| r.task.as_ref() == Some(task),
                &mut out.messages,
            );
        }
        Unit::General => each(
            &incoming.messages,
            &local.messages,
            |r| r.task.is_none(),
            &mut out.messages,
        ),
        Unit::Memory(_) => {}
    }
}

/// 両側の行がすべてそろった単位を決める。
fn whole_unit(unit: &Unit, inputs: &Inputs<'_>, local: &Index<'_>, out: &mut Out, plan: &mut Plan) {
    let ours = unit_rows(unit, inputs.local);
    let theirs = unit_rows(unit, inputs.incoming);
    let tasks = pairs(&ours.tasks, &theirs.tasks);
    let steps = pairs(&ours.steps, &theirs.steps);
    let messages = pairs(&ours.messages, &theirs.messages);

    let conflict = tasks.iter().any(|p| p.merged.is_none())
        || steps.iter().any(|p| p.merged.is_none())
        || messages.iter().any(|p| p.merged.is_none())
        || broken_chat(&messages, &ours.messages, &theirs.messages)
        || deleted_unknowingly(&tasks, &steps, &messages);
    if conflict {
        plan.conflicts.push(Conflict {
            unit: unit.clone(),
            ours,
            theirs,
        });
        return;
    }

    for p in &tasks {
        p.write_into(&mut out.tasks);
    }
    for p in &steps {
        p.write_into(&mut out.steps);
    }
    for p in &messages {
        p.write_into(&mut out.messages);
    }
    for b in &theirs.attachments {
        if !local.attachments.contains_key(b.uid.as_str()) {
            out.attachments.push(b.clone());
        }
    }

    if let Unit::Task(task) = unit {
        let changed = |here: bool| {
            tasks.iter().any(|p| p.changed_on(here)) || steps.iter().any(|p| p.changed_on(here))
        };
        let merged_task = tasks.first().and_then(|p| p.merged.clone());
        if let Some(merged_task) = merged_task.filter(|t| !t.is_deleted()) {
            if changed(true) && changed(false) {
                let merged_steps: Vec<StepRow> =
                    steps.iter().filter_map(|p| p.merged.clone()).collect();
                plan.records.push(TaskStateRecord {
                    task: task.clone(),
                    uid: record_uid(&merged_task, &merged_steps),
                    created_at: merged_task.updated_at.clone(),
                });
            }
        }
    }
}

/// 記録の`uid`は、結合後のタスクと工程の行(版と日時を含む)から決める。別の2台が同じ結合をしても
/// 2重にならず、記録の中身(`content`・`created_at`)はこの行だけから決まるので、同じ`uid`で中身が
/// 違う記録はできない。
fn record_uid(task: &TaskRow, steps: &[StepRow]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"scitl-sync task state\0");
    hasher.update(serde_json::to_vec(task).expect("a row serializes"));
    let mut steps: Vec<&StepRow> = steps.iter().collect();
    steps.sort_by(|a, b| a.uid.cmp(&b.uid));
    for step in steps {
        hasher.update(serde_json::to_vec(step).expect("a row serializes"));
    }
    hasher.finalize()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 同じ`uid`の、Aの行とBの行と、版で決めた結果(決まらなければ`None`)。
struct Pair<R> {
    ours: Option<R>,
    theirs: Option<R>,
    merged: Option<R>,
}

impl<R: Versioned> Pair<R> {
    /// その側が、相手の知らない変更を持つか(その側にしか無い、版が新しい、並ぶ)。
    fn changed_on(&self, here: bool) -> bool {
        let (mine, other) = if here {
            (&self.ours, &self.theirs)
        } else {
            (&self.theirs, &self.ours)
        };
        match (mine, other) {
            (Some(_), None) => true,
            (Some(m), Some(o)) => matches!(
                compare(m.version(), o.version()),
                Order::Newer | Order::Concurrent
            ),
            _ => false,
        }
    }

    /// Aに書く必要があれば書く行に入れる。
    fn write_into(&self, out: &mut Vec<R>) {
        if let Some(m) = &self.merged {
            if self.ours.as_ref() != Some(m) {
                out.push(m.clone());
            }
        }
    }
}

/// 両側の行を`uid`で組にする。並びはBの行の順のあとに、Aにしか無い行。
fn pairs<R: Versioned>(ours: &[R], theirs: &[R]) -> Vec<Pair<R>> {
    let by_uid: HashMap<&str, &R> = ours.iter().map(|r| (r.uid(), r)).collect();
    let mut out: Vec<Pair<R>> = theirs
        .iter()
        .map(|b| {
            let a = by_uid.get(b.uid()).copied();
            Pair {
                ours: a.cloned(),
                theirs: Some(b.clone()),
                merged: match a {
                    Some(a) => merge_pair(a, b),
                    None => Some(b.clone()),
                },
            }
        })
        .collect();
    let theirs: HashSet<&str> = theirs.iter().map(|r| r.uid()).collect();
    out.extend(
        ours.iter()
            .filter(|a| !theirs.contains(a.uid()))
            .map(|a| Pair {
                ours: Some(a.clone()),
                theirs: None,
                merged: Some(a.clone()),
            }),
    );
    out
}

/// 消えていない通常発言の`uid`。
fn alive<'a>(messages: impl Iterator<Item = &'a MessageRow>) -> BTreeSet<&'a str> {
    messages
        .filter(|m| m.kind == "normal" && !m.is_deleted())
        .map(|m| m.uid.as_str())
        .collect()
}

/// 結合した会話の形が破れたか(9節)。消えていない通常発言が、`prev_uid`を辿って1本の列に
/// ならない。結合の結果がどちらか一方の会話と同じなら、形を見ない。
fn broken_chat(messages: &[Pair<MessageRow>], ours: &[MessageRow], theirs: &[MessageRow]) -> bool {
    let merged: Vec<&MessageRow> = messages.iter().filter_map(|p| p.merged.as_ref()).collect();
    let alive_merged = alive(merged.iter().copied());
    if alive_merged == alive(ours.iter()) || alive_merged == alive(theirs.iter()) {
        return false;
    }
    let mut prevs = HashSet::new();
    merged
        .iter()
        .filter(|m| alive_merged.contains(m.uid.as_str()))
        .any(|m| {
            let dangling = m
                .prev_uid
                .as_deref()
                .is_some_and(|p| !alive_merged.contains(p));
            dangling || !prevs.insert(m.prev_uid.as_deref())
        })
}

/// 片方がタスクを削除し、もう片方がそれを知らずに工程か会話を変えたか(9節)。
fn deleted_unknowingly(
    tasks: &[Pair<TaskRow>],
    steps: &[Pair<StepRow>],
    messages: &[Pair<MessageRow>],
) -> bool {
    let Some(task) = tasks.first() else {
        return false;
    };
    if !task.merged.as_ref().is_some_and(|t| t.is_deleted()) {
        return false;
    }
    let deleted = |r: &Option<TaskRow>| r.as_ref().is_some_and(|t| t.is_deleted());
    // 削除しなかった側(どちらも削除していれば無い)。
    let kept_by_ours = match (deleted(&task.ours), deleted(&task.theirs)) {
        (true, false) => false,
        (false, true) => true,
        _ => return false,
    };
    steps.iter().any(|p| p.changed_on(kept_by_ours))
        || messages.iter().any(|p| p.changed_on(kept_by_ours))
}

/// 単位の行(タスクの行・工程・会話と、その発言の添付。メモリは行1つ)。
fn unit_rows(unit: &Unit, t: &Transport) -> Transport {
    let in_chat = |task: &Option<String>| match unit {
        Unit::Task(uid) => task.as_ref() == Some(uid),
        Unit::General => task.is_none(),
        Unit::Memory(_) => false,
    };
    let messages: Vec<MessageRow> = t
        .messages
        .iter()
        .filter(|m| in_chat(&m.task))
        .cloned()
        .collect();
    let message_uids: HashSet<&str> = messages.iter().map(|m| m.uid.as_str()).collect();
    let attachments = t
        .attachments
        .iter()
        .filter(|a| message_uids.contains(a.message.as_str()))
        .cloned()
        .collect();
    match unit {
        Unit::Task(uid) => Transport {
            tasks: t.tasks.iter().filter(|r| &r.uid == uid).cloned().collect(),
            steps: t.steps.iter().filter(|r| &r.task == uid).cloned().collect(),
            memories: Vec::new(),
            messages,
            attachments,
        },
        Unit::General => Transport {
            messages,
            attachments,
            ..Transport::default()
        },
        Unit::Memory(uid) => Transport {
            memories: t
                .memories
                .iter()
                .filter(|r| &r.uid == uid)
                .cloned()
                .collect(),
            ..Transport::default()
        },
    }
}

/// 利用者が選んだ側で、競合した単位の結果を作る(9節「選んだ結果の作り方」)。単位のすべての行に
/// ついて、選んだ側の値を取り、版は両方の最大に自分の分(`me`の`number`)を進める。選ばなかった
/// 側にしか無い行は論理削除する。値が変わらなかった行も版を進める。
pub(crate) fn resolve(
    conflict: &Conflict,
    pick: Pick,
    me: &str,
    number: i64,
    now: &str,
) -> Transport {
    fn rows<R: Versioned>(
        ours: &[R],
        theirs: &[R],
        pick: Pick,
        me: &str,
        number: i64,
        now: &str,
    ) -> Vec<R> {
        let ours_by_uid: HashMap<&str, &R> = ours.iter().map(|r| (r.uid(), r)).collect();
        let theirs_by_uid: HashMap<&str, &R> = theirs.iter().map(|r| (r.uid(), r)).collect();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        // Bにしか無い行を、Bの会話の並びの順で足すため、Bの行を先に見る。
        for r in theirs.iter().chain(ours.iter()) {
            if !seen.insert(r.uid()) {
                continue;
            }
            let (a, b) = (ours_by_uid.get(r.uid()), theirs_by_uid.get(r.uid()));
            let (chosen, other) = match pick {
                Pick::Ours => (a, b),
                Pick::Theirs => (b, a),
            };
            let (mut result, mut version) = match (chosen, other) {
                // 日時は自動の結合と同じく合わせる(Bは、受け取った行に同じ合わせ方を掛ける)。
                (Some(c), Some(o)) => {
                    let mut result = (*c).clone();
                    result.merge_dates(o);
                    (result, join(c.version(), o.version()))
                }
                (Some(c), None) => ((*c).clone(), c.version().clone()),
                (None, Some(o)) => {
                    let mut deleted = (*o).clone();
                    deleted.delete(now);
                    (deleted, o.version().clone())
                }
                (None, None) => continue,
            };
            version.insert(me.to_string(), number);
            *result.version_mut() = version;
            out.push(result);
        }
        out
    }
    let (ours, theirs) = (&conflict.ours, &conflict.theirs);
    Transport {
        tasks: rows(&ours.tasks, &theirs.tasks, pick, me, number, now),
        steps: rows(&ours.steps, &theirs.steps, pick, me, number, now),
        memories: rows(&ours.memories, &theirs.memories, pick, me, number, now),
        messages: rows(&ours.messages, &theirs.messages, pick, me, number, now),
        // 添付は書いたあと変わらないので、無いものを足すだけ(発言を消せば添付も見えなくなる)。
        attachments: theirs
            .attachments
            .iter()
            .filter(|r| !ours.attachments.iter().any(|a| a.uid == r.uid))
            .cloned()
            .collect(),
    }
}

/// 消えていないメモリのうち、同じ本文の2つ目以降(`uid`の大きい方)。残すのは`uid`の最小の行(8節)。
pub(crate) fn duplicate_memories<'a>(
    memories: impl Iterator<Item = (&'a str, &'a str)>,
) -> Vec<&'a str> {
    let mut by_content: HashMap<&str, Vec<&str>> = HashMap::new();
    for (uid, content) in memories {
        by_content.entry(content).or_default().push(uid);
    }
    let mut out = Vec::new();
    for mut uids in by_content.into_values() {
        uids.sort_unstable();
        out.extend(uids.into_iter().skip(1));
    }
    out.sort_unstable();
    out
}

/// 行を`uid`で引く。
struct Index<'a> {
    tasks: HashMap<&'a str, &'a TaskRow>,
    steps: HashMap<&'a str, &'a StepRow>,
    memories: HashMap<&'a str, &'a MemoryRow>,
    messages: HashMap<&'a str, &'a MessageRow>,
    attachments: HashMap<&'a str, &'a AttachmentRow>,
}

impl<'a> Index<'a> {
    fn of(t: &'a Transport) -> Self {
        Self {
            tasks: t.tasks.iter().map(|r| (r.uid.as_str(), r)).collect(),
            steps: t.steps.iter().map(|r| (r.uid.as_str(), r)).collect(),
            memories: t.memories.iter().map(|r| (r.uid.as_str(), r)).collect(),
            messages: t.messages.iter().map(|r| (r.uid.as_str(), r)).collect(),
            attachments: t.attachments.iter().map(|r| (r.uid.as_str(), r)).collect(),
        }
    }
}

/// 書く行を集める。
#[derive(Default)]
struct Out {
    tasks: Vec<TaskRow>,
    steps: Vec<StepRow>,
    memories: Vec<MemoryRow>,
    messages: Vec<MessageRow>,
    attachments: Vec<AttachmentRow>,
}

impl Out {
    /// 発言は、Bの行の順(会話の並び)に並べ直す。Aにしか無い行は書かない(書く行に入らない)。
    fn into_transport(mut self, incoming: &Transport) -> Transport {
        let order: HashMap<&str, usize> = incoming
            .messages
            .iter()
            .enumerate()
            .map(|(i, m)| (m.uid.as_str(), i))
            .collect();
        self.messages
            .sort_by_key(|m| order.get(m.uid.as_str()).copied().unwrap_or(usize::MAX));
        Transport {
            tasks: self.tasks,
            steps: self.steps,
            memories: self.memories,
            messages: self.messages,
            attachments: self.attachments,
        }
    }
}

#[cfg(test)]
mod tests;
