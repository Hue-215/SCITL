//! 結合(`docs/sync/index.md`「版と結合」「会話の枝と、競合の単位」)。待ち受ける側(A)が、受け取った行(B)と自分の行だけから
//! 結果を決める。通信もDBも触らず、運ぶ形(`uid`で指し合う行)の上で決める。
//!
//! 単位(タスク・総合チャット)は、Bから差分だけが届いていれば、行ごとに版で決める。両方がその
//! 単位を変えていたときと、版で決まらない行があるときは、単位の行をすべて求めてから([`Plan::needs`])
//! 単位ごとに決める。会話の形と、削除と知らずにした変更は、単位のすべての行を見ないと分からない
//! (差分だけでは、相手が持っていない行を知りようがない)。

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

use sha2::{Digest, Sha256};
use scitl_core::CoreError;

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

/// 状態を表す日時は、どちらにもあれば早い方(同じ操作を2台でしても揃わない。「版と結合」)。
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

/// 版を持つ行。比べる欄と日時の扱いは`docs/sync/index.md`「版と結合」の表。
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
/// 自分の分は進めない(どの端末で結合しても同じ結果にする。「版と結合」)。
pub(crate) fn merge_pair<R: Versioned>(a: &R, b: &R) -> Option<R> {
    let (mut out, other) = match compare(a.version(), b.version()) {
        // 同じ版で値が違うのは、端末IDの重複の印(「端末ID」)。競合として扱う。
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

/// 単位。競合はこの単位で選ばせる(「会話の枝と、競合の単位」)。
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
        row_units(t).for_each(|unit| units.insert(unit));
        units
    }

    /// 発言の会話(`task`)が、この集まりの単位か。
    pub(crate) fn has_chat(&self, task: Option<&str>) -> bool {
        match task {
            Some(uid) => self.tasks.contains(uid),
            None => self.general,
        }
    }

    /// `t`の行がすべて、この集まりの単位の行か(求めた単位の行だけが届いたか)。メモリは単位に
    /// 入らず、添付は一緒に届いた発言のものに限る。
    pub(crate) fn covers(&self, t: &Transport) -> bool {
        let messages: HashSet<&str> = t.messages.iter().map(|m| m.uid.as_str()).collect();
        t.memories.is_empty()
            && t.tasks.iter().all(|r| self.tasks.contains(&r.uid))
            && t.steps.iter().all(|r| self.tasks.contains(&r.task))
            && t.messages.iter().all(|r| self.has_chat(r.task.as_deref()))
            && t.attachments
                .iter()
                .all(|a| messages.contains(a.message.as_str()))
    }
}

/// 行の単位を、行の並びの順に(同じ単位も繰り返す)。メモリと添付は単位の行に数えない。
fn row_units(t: &Transport) -> impl Iterator<Item = Unit> + '_ {
    t.tasks
        .iter()
        .map(|r| Unit::Task(r.uid.clone()))
        .chain(t.steps.iter().map(|r| Unit::Task(r.task.clone())))
        .chain(t.messages.iter().map(|r| Unit::of_chat(&r.task)))
}

/// 1つの単位の行(分けた元の行を指す)。
#[derive(Default)]
struct UnitRows<'a> {
    tasks: Vec<&'a TaskRow>,
    steps: Vec<&'a StepRow>,
    messages: Vec<&'a MessageRow>,
    attachments: Vec<&'a AttachmentRow>,
}

impl UnitRows<'_> {
    fn to_transport(&self) -> Transport {
        fn owned<R: Clone>(rows: &[&R]) -> Vec<R> {
            rows.iter().map(|r| (*r).clone()).collect()
        }
        Transport {
            tasks: owned(&self.tasks),
            steps: owned(&self.steps),
            memories: Vec::new(),
            messages: owned(&self.messages),
            attachments: owned(&self.attachments),
        }
    }
}

/// 行を単位ごとに分ける(単位は`t`に現れた順、行は`t`の並びの順)。添付は発言の単位に入れ、発言が
/// `t`に無い添付はどこにも入れない。
fn split_by_unit(t: &Transport) -> Vec<(Unit, UnitRows<'_>)> {
    fn slot(
        units: &mut Vec<(Unit, UnitRows<'_>)>,
        index: &mut HashMap<Unit, usize>,
        unit: Unit,
    ) -> usize {
        *index.entry(unit.clone()).or_insert_with(|| {
            units.push((unit, UnitRows::default()));
            units.len() - 1
        })
    }
    let mut units = Vec::new();
    let mut index = HashMap::new();
    for r in &t.tasks {
        let i = slot(&mut units, &mut index, Unit::Task(r.uid.clone()));
        units[i].1.tasks.push(r);
    }
    for r in &t.steps {
        let i = slot(&mut units, &mut index, Unit::Task(r.task.clone()));
        units[i].1.steps.push(r);
    }
    let mut chat_of_message: HashMap<&str, usize> = HashMap::new();
    for r in &t.messages {
        let i = slot(&mut units, &mut index, Unit::of_chat(&r.task));
        units[i].1.messages.push(r);
        chat_of_message.insert(r.uid.as_str(), i);
    }
    for r in &t.attachments {
        if let Some(&i) = chat_of_message.get(r.message.as_str()) {
            units[i].1.attachments.push(r);
        }
    }
    units
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

/// 結合後のタスクの全体の記録(「版と結合」)。
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

/// 選んだ単位の解決の行(選んだ側の値、版は両方の最大に自分の分を進める。8節・9節)。
/// 書き込むときは`number`に確定の番号、`now`にそのときの日時を渡す。書き込む前の件数は
/// (`write_plan`集め方に現れない)番号と日時を受け取らない。
pub(crate) fn resolution_rows(
    plan: &Plan,
    picks: &HashMap<String, Pick>,
    me: &str,
    number: i64,
    now: &str,
) -> Result<Transport, CoreError> {
    let mut out = Transport::default();
    for conflict in &plan.conflicts {
        let pick = picks
            .get(&conflict.unit.id())
            .copied()
            .ok_or_else(|| CoreError::Internal("a conflict was not chosen".to_string()))?;
        let resolved = resolve(conflict, pick, me, number, now);
        out.tasks.extend(resolved.tasks);
        out.steps.extend(resolved.steps);
        out.memories.extend(resolved.memories);
        out.messages.extend(resolved.messages);
        out.attachments.extend(resolved.attachments);
    }
    Ok(out)
}

/// Aに書く行(計画の行と解決の行。重複は無い)。確定の書き込み(`transfer::commit`)に使う。
pub(crate) fn written_rows(
    plan: &Plan,
    picks: &HashMap<String, Pick>,
    me: &str,
    number: i64,
    now: &str,
) -> Result<Transport, CoreError> {
    let resolution = resolution_rows(plan, picks, me, number, now)?;
    let mut out = plan.rows.clone();
    out.tasks.extend(resolution.tasks);
    out.steps.extend(resolution.steps);
    out.memories.extend(resolution.memories);
    out.messages.extend(resolution.messages);
    out.attachments.extend(resolution.attachments);
    Ok(out)
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
    let incoming_units = split_by_unit(inputs.incoming);
    for (unit, theirs) in &incoming_units {
        if inputs.complete.contains(unit) {
            continue;
        }
        if inputs.changed_here.contains(unit) || undecided(theirs, &local) {
            plan.needs.insert(unit.clone());
        }
    }
    if !plan.needs.is_empty() {
        return plan;
    }

    let local_units: HashMap<Unit, UnitRows<'_>> =
        split_by_unit(inputs.local).into_iter().collect();
    let mut out = Transport::default();
    for (unit, theirs) in &incoming_units {
        if inputs.complete.contains(unit) {
            let ours = local_units
                .get(unit)
                .map(UnitRows::to_transport)
                .unwrap_or_default();
            whole_unit(unit, ours, theirs.to_transport(), &mut out, &mut plan);
        } else {
            by_row(theirs, &local, &mut out);
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
    // 発言は、Bの行の順(会話の並び)に並べ直す。
    let order: HashMap<&str, usize> = inputs
        .incoming
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| (m.uid.as_str(), i))
        .collect();
    out.messages
        .sort_by_key(|m| order.get(m.uid.as_str()).copied().unwrap_or(usize::MAX));
    plan.rows = out;
    plan
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

/// 版で決まらない行が、単位の行(`theirs`)にあるか。
fn undecided(theirs: &UnitRows<'_>, local: &Index<'_>) -> bool {
    fn any<R: Versioned>(rows: &[&R], local: &HashMap<&str, &R>) -> bool {
        rows.iter().any(|b| {
            local
                .get(b.uid())
                .is_some_and(|a| merge_pair(*a, *b).is_none())
        })
    }
    any(&theirs.tasks, &local.tasks)
        || any(&theirs.steps, &local.steps)
        || any(&theirs.messages, &local.messages)
}

/// 差分だけが届いた単位(`theirs`は、その単位の受け取った行)。Aはこの単位を前回から変えていないので、
/// Bの行は版でAの行を含むか同じになる。行ごとに版で決める。
fn by_row(theirs: &UnitRows<'_>, local: &Index<'_>, out: &mut Transport) {
    fn each<R: Versioned>(rows: &[&R], local: &HashMap<&str, &R>, out: &mut Vec<R>) {
        for b in rows {
            match local.get(b.uid()) {
                None => out.push((*b).clone()),
                Some(a) => {
                    if let Some(m) = merge_pair(*a, *b).filter(|m| m != *a) {
                        out.push(m);
                    }
                }
            }
        }
    }
    each(&theirs.tasks, &local.tasks, &mut out.tasks);
    each(&theirs.steps, &local.steps, &mut out.steps);
    each(&theirs.messages, &local.messages, &mut out.messages);
}

/// 両側の行がすべてそろった単位を決める。`ours`・`theirs`はその単位の両側の行。添付は、無いものを
/// [`plan`]が単位によらず足す。
fn whole_unit(
    unit: &Unit,
    ours: Transport,
    theirs: Transport,
    out: &mut Transport,
    plan: &mut Plan,
) {
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
        .filter(|m| m.is_alive_normal())
        .map(|m| m.uid.as_str())
        .collect()
}

/// 結合した会話の形が破れたか(「会話の枝と、競合の単位」)。消えていない通常発言が、`prev_uid`を辿って1本の列に
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

/// 片方がタスクを削除し、もう片方がそれを知らずに工程か会話を変えたか(「会話の枝と、競合の単位」)。
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

/// 利用者が選んだ側で、競合した単位の結果を作る(「選んだ結果の作り方」)。単位のすべての行に
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
    let ours_attachments: HashSet<&str> = ours.attachments.iter().map(|a| a.uid.as_str()).collect();
    Transport {
        tasks: rows(&ours.tasks, &theirs.tasks, pick, me, number, now),
        steps: rows(&ours.steps, &theirs.steps, pick, me, number, now),
        memories: rows(&ours.memories, &theirs.memories, pick, me, number, now),
        messages: rows(&ours.messages, &theirs.messages, pick, me, number, now),
        // 添付は書いたあと変わらないので、無いものを足すだけ(発言を消せば添付も見えなくなる)。
        attachments: theirs
            .attachments
            .iter()
            .filter(|r| !ours_attachments.contains(r.uid.as_str()))
            .cloned()
            .collect(),
    }
}

/// 消えていないメモリのうち、同じ本文の2つ目以降(`uid`の大きい方)。残すのは`uid`の最小の行(「版と結合」)。
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

#[cfg(test)]
mod tests;
