use super::*;

fn v(pairs: &[(&str, i64)]) -> Version {
    pairs.iter().map(|(d, n)| (d.repeat(32), *n)).collect()
}

fn step(uid: &str, version: Version) -> StepRow {
    StepRow {
        uid: uid.repeat(32),
        task: "t".repeat(32),
        description: "買い出し".to_string(),
        done_at: None,
        deleted_at: None,
        order_index: 0,
        created_at: "2026-10-01T00:00:00Z".to_string(),
        version,
    }
}

fn message(uid: &str, prev: Option<&str>, version: Version) -> MessageRow {
    MessageRow {
        uid: uid.repeat(32),
        task: None,
        role: "user".to_string(),
        kind: "normal".to_string(),
        content: uid.to_string(),
        source: None,
        error_kind: None,
        error_detail: None,
        parts: None,
        turn_id: None,
        attempt: None,
        deleted_at: None,
        created_at: "2026-10-01T00:00:00Z".to_string(),
        prev_uid: prev.map(|p| p.repeat(32)),
        version,
    }
}

#[test]
fn versions_are_ordered_device_by_device() {
    assert_eq!(compare(&v(&[("a", 1)]), &v(&[("a", 1)])), Order::Same);
    assert_eq!(compare(&v(&[("a", 2)]), &v(&[("a", 1)])), Order::Newer);
    assert_eq!(compare(&v(&[]), &v(&[("a", 1)])), Order::Older);
    assert_eq!(
        compare(&v(&[("a", 2), ("b", 1)]), &v(&[("a", 1), ("b", 2)])),
        Order::Concurrent
    );
    assert_eq!(
        join(&v(&[("a", 2), ("b", 1)]), &v(&[("a", 1), ("c", 3)])),
        v(&[("a", 2), ("b", 1), ("c", 3)])
    );
}

/// どの端末で結合しても同じ結果になる(自分の分を進めない)。
#[test]
fn the_same_state_changed_on_both_sides_merges_the_same_either_way() {
    let mut a = step("s", v(&[("a", 2)]));
    a.done_at = Some("2026-10-02T00:00:00Z".to_string());
    let mut b = step("s", v(&[("b", 3)]));
    b.done_at = Some("2026-10-01T12:00:00Z".to_string());
    let merged = merge_pair(&a, &b).unwrap();
    assert_eq!(merged, merge_pair(&b, &a).unwrap());
    assert_eq!(merged.version, v(&[("a", 2), ("b", 3)]));
    assert_eq!(merged.done_at.as_deref(), Some("2026-10-01T12:00:00Z"));
}

#[test]
fn a_newer_row_wins_and_an_undecided_one_is_left_to_the_user() {
    let old = step("s", v(&[("a", 1)]));
    let mut new = step("s", v(&[("a", 1), ("b", 1)]));
    new.description = "調理".to_string();
    assert_eq!(merge_pair(&old, &new).unwrap().description, "調理");
    assert_eq!(merge_pair(&new, &old).unwrap().description, "調理");

    let mut other = step("s", v(&[("a", 2)]));
    other.description = "片付け".to_string();
    assert!(merge_pair(&new, &other).is_none());
    // 同じ版で値が違うのは、端末IDの重複の印。
    let mut twin = old.clone();
    twin.description = "片付け".to_string();
    assert!(merge_pair(&old, &twin).is_none());
}

#[test]
fn the_smallest_uid_of_the_same_memory_is_kept() {
    let rows = [("b", "朝型"), ("a", "朝型"), ("c", "夜型"), ("d", "朝型")];
    assert_eq!(duplicate_memories(rows.into_iter()), ["b", "d"]);
}

fn general(ours: Vec<MessageRow>, theirs: Vec<MessageRow>) -> Plan {
    let units = Units {
        general: true,
        ..Units::default()
    };
    plan(&Inputs {
        incoming: &Transport {
            messages: theirs,
            ..Transport::default()
        },
        local: &Transport {
            messages: ours,
            ..Transport::default()
        },
        complete: &units,
        changed_here: &units,
    })
}

#[test]
fn a_chat_continued_from_the_same_point_on_both_sides_is_a_conflict() {
    let first = message("a", None, v(&[]));
    let plan = general(
        vec![first.clone(), message("b", Some("a"), v(&[]))],
        vec![first.clone(), message("c", Some("a"), v(&[]))],
    );
    assert_eq!(plan.conflicts.len(), 1);
    assert_eq!(plan.conflicts[0].unit, Unit::General);

    // 片方が続けただけなら、その側の会話になる。
    let plan = general(
        vec![first.clone()],
        vec![first.clone(), message("c", Some("a"), v(&[]))],
    );
    assert!(plan.conflicts.is_empty());
    assert_eq!(plan.rows.messages.len(), 1);
}

/// 片方が消した範囲の後ろに、もう片方が続けた。
#[test]
fn continuing_after_a_message_deleted_on_the_other_side_is_a_conflict() {
    let first = message("a", None, v(&[]));
    let second = message("b", Some("a"), v(&[]));
    let mut deleted = second.clone();
    deleted.deleted_at = Some("2026-10-02T00:00:00Z".to_string());
    deleted.version = v(&[("a", 5)]);
    let plan = general(
        vec![first.clone(), deleted, message("d", Some("a"), v(&[]))],
        vec![first, second, message("c", Some("b"), v(&[]))],
    );
    assert_eq!(plan.conflicts.len(), 1);
}

/// 選んだ側の値を取り、選ばなかった側にしか無い行を消し、どの行も版を進める。
#[test]
fn resolving_takes_the_chosen_side_and_deletes_what_only_the_other_had() {
    let first = message("a", None, v(&[("a", 1)]));
    let conflict = Conflict {
        unit: Unit::General,
        ours: Transport {
            messages: vec![first.clone(), message("b", Some("a"), v(&[("a", 2)]))],
            ..Transport::default()
        },
        theirs: Transport {
            messages: vec![first.clone(), message("c", Some("a"), v(&[("b", 1)]))],
            ..Transport::default()
        },
    };
    let me = "a".repeat(32);
    let out = resolve(&conflict, Pick::Theirs, &me, 9, "2026-10-03T00:00:00Z");
    let by_uid: HashMap<&str, &MessageRow> =
        out.messages.iter().map(|m| (m.uid.as_str(), m)).collect();
    assert_eq!(out.messages.len(), 3);
    assert!(by_uid[&*"b".repeat(32)].deleted_at.is_some());
    assert!(by_uid[&*"c".repeat(32)].deleted_at.is_none());
    for m in &out.messages {
        assert_eq!(m.version.get(&me), Some(&9), "{}", m.uid);
    }
    assert_eq!(
        by_uid[&*"c".repeat(32)].version.get(&"b".repeat(32)),
        Some(&1)
    );
    // Bにしか無い行は、Bの並びの順で先に来る(足すときの会話の並び)。
    assert_eq!(out.messages[1].uid, "c".repeat(32));
}
