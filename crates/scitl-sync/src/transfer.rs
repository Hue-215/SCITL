//! 丸ごとの移送(`docs/sync/index.md` 12節の段階2)。片方にタスク・発言・メモリが1件も無いときだけ
//! 受け付け、ある側から無い側へ全部を写す。結合はしないので、どちらにもデータがあれば断る。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;
use serde::Serialize;

use scitl_core::attachments::AttachmentStore;
use scitl_core::blocking;
use scitl_core::db::{self, SharedConnection};

use crate::files;
use crate::transport::{AbortReason, AttachmentRow, Counts, Message, Transport};
use crate::{convert, device, validate, DeviceRegistry, InvalidRow, Session, SyncError};

/// 1つのメッセージに詰める行の量の目安(直列化した大きさ)。
const BATCH_BYTES: usize = 4 * 1024 * 1024;
/// 受け取る行の合計(直列化した大きさ)の上限。行は書く前にすべてメモリに載せて確かめる。
const MAX_ROWS_BYTES: usize = 512 * 1024 * 1024;
/// 添付の実体を分けて送る大きさ(base64にする前)。
const FILE_CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// 同期の役。つなぐ側が先に送る。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Listener,
    Connector,
}

/// 移送が触るもの。
pub struct Context {
    pub db: SharedConnection,
    pub attachments: AttachmentStore,
    pub registry: Arc<dyn DeviceRegistry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "direction", content = "counts", rename_all = "snake_case")]
pub enum Outcome {
    Sent(Counts),
    Received(Counts),
}

/// 確かめ終えた接続の上で、全部を写す。どちらが写すかは、データの有無で決まる。`device_id`は
/// 確かめた端末ID(`device::confirm_device_id`)。
pub async fn copy(
    session: &mut Session,
    ctx: &Context,
    side: Side,
    device_id: &str,
) -> Result<Outcome, SyncError> {
    let result = run(session, ctx, side, device_id).await;
    if let Err(e) = &result {
        if let Some(reason) = reason_to_tell(e) {
            let _ = session.send(&Message::Abort { reason }).await;
        }
    }
    result
}

/// 失敗を相手に知らせる理由。相手から届いた失敗と、接続そのものの失敗は知らせない。
fn reason_to_tell(e: &SyncError) -> Option<AbortReason> {
    match e {
        SyncError::InvalidRows(_) => Some(AbortReason::InvalidRows),
        SyncError::NoLongerEmpty => Some(AbortReason::NotEmpty),
        SyncError::PeerRefused(_)
        | SyncError::PeerClosed
        | SyncError::Io(_)
        | SyncError::Tls(_)
        | SyncError::TimedOut
        | SyncError::SameDevice
        | SyncError::BothHaveData
        | SyncError::NothingToCopy => None,
        _ => Some(AbortReason::Failed),
    }
}

async fn run(
    session: &mut Session,
    ctx: &Context,
    side: Side,
    device_id: &str,
) -> Result<Outcome, SyncError> {
    let has_data = db::with_conn(ctx.db.clone(), db::sync::has_data).await?;
    let ours = Message::Intro {
        device_id: device_id.to_string(),
        has_data,
    };
    let theirs = match side {
        Side::Connector => {
            session.send(&ours).await?;
            session.recv().await?
        }
        Side::Listener => {
            let theirs = session.recv().await?;
            session.send(&ours).await?;
            theirs
        }
    };
    let Message::Intro {
        device_id: peer_device_id,
        has_data: peer_has_data,
    } = theirs
    else {
        return Err(unexpected(&theirs));
    };
    if !is_device_id(&peer_device_id) {
        return Err(SyncError::Malformed("device ID".to_string()));
    }
    if peer_device_id == device_id {
        return Err(SyncError::SameDevice);
    }
    match (has_data, peer_has_data) {
        (true, true) => Err(SyncError::BothHaveData),
        (false, false) => Err(SyncError::NothingToCopy),
        (true, false) => send_all(session, ctx, &peer_device_id).await,
        (false, true) => receive_all(session, ctx, device_id, &peer_device_id).await,
    }
}

fn is_device_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn unexpected(message: &Message) -> SyncError {
    match message {
        Message::Abort { reason } => SyncError::PeerRefused(*reason),
        _ => SyncError::Malformed("unexpected message".to_string()),
    }
}

async fn send_all(
    session: &mut Session,
    ctx: &Context,
    peer_device_id: &str,
) -> Result<Outcome, SyncError> {
    let snapshot = db::with_conn(ctx.db.clone(), db::sync::stamp_and_snapshot).await?;
    let (transport, mut invalid) = convert::to_transport(&snapshot.rows);
    let mut found = validate::check(&transport).err().unwrap_or_default();
    found.extend(check_own_files(ctx, &transport).await?);
    // 置き換えで外れた行は、検証でも同じ行が外れうる。1つの行は1度だけ示す。
    for row in with_local_ids(found, &snapshot.rows) {
        if !invalid.iter().any(|r| r.uid == row.uid) {
            invalid.push(row);
        }
    }
    if !invalid.is_empty() {
        return Err(SyncError::InvalidRows(invalid));
    }

    send_batches(session, &transport.tasks, |rows| Message::Tasks { rows }).await?;
    send_batches(session, &transport.steps, |rows| Message::Steps { rows }).await?;
    send_batches(session, &transport.memories, |rows| Message::Memories {
        rows,
    })
    .await?;
    send_batches(session, &transport.messages, |rows| Message::Messages {
        rows,
    })
    .await?;
    send_batches(session, &transport.attachments, |rows| {
        Message::Attachments { rows }
    })
    .await?;
    session
        .send(&Message::RowsEnd {
            counter: snapshot.device.counter,
        })
        .await?;

    let hashes = match session.recv().await? {
        Message::NeedFiles { hashes } => hashes,
        other => return Err(unexpected(&other)),
    };
    let sent = files::rows_by_file(&transport.attachments);
    let mut counts = transport.counts();
    for hash in &hashes {
        if !sent.contains_key(hash.as_str()) {
            return Err(SyncError::Malformed(
                "asked for a file that was not sent".to_string(),
            ));
        }
        let bytes = read_file(&ctx.attachments, hash).await?;
        send_file(session, hash, &bytes).await?;
        counts.files += 1;
    }
    session.send(&Message::FilesEnd).await?;

    let peer_counter = match session.recv().await? {
        Message::Done { counter } => counter,
        other => return Err(unexpected(&other)),
    };
    let peer = peer_device_id.to_string();
    let sent_seq = snapshot.device.counter;
    db::with_conn(ctx.db.clone(), move |conn| {
        db::sync::record_cursor(conn, &peer, sent_seq, peer_counter)
    })
    .await?;
    Ok(Outcome::Sent(counts))
}

async fn read_file(store: &AttachmentStore, hash: &str) -> Result<Vec<u8>, SyncError> {
    let store = store.clone();
    let hash = hash.to_string();
    Ok(blocking::run(move || store.read(&hash)).await?)
}

/// 送る前に、行が指す自分の実体を確かめる(送る側で外れた行を示すため。`docs/sync/index.md` 5節)。
async fn check_own_files(ctx: &Context, t: &Transport) -> Result<Vec<InvalidRow>, SyncError> {
    let mut invalid = Vec::new();
    for (hash, rows) in files::rows_by_file(&t.attachments) {
        let store = ctx.attachments.clone();
        let wanted = hash.to_string();
        let bytes = blocking::run(move || {
            if store.contains(&wanted)? {
                store.read(&wanted).map(Some)
            } else {
                Ok(None)
            }
        })
        .await?;
        match bytes {
            Some(bytes) => invalid.extend(files::check(hash, &bytes, &rows)),
            None => invalid.extend(rows.iter().map(|r| InvalidRow {
                table: "attachments",
                id: None,
                uid: r.uid.clone(),
                reason: "the file is missing".to_string(),
            })),
        }
    }
    Ok(invalid)
}

/// 送る側で見つけた行に、その端末の中の連番を添える(どの行を直せばよいか分かるように)。
fn with_local_ids(rows: Vec<InvalidRow>, local: &db::sync::Rows) -> Vec<InvalidRow> {
    let ids: HashMap<&str, i64> = local
        .tasks
        .iter()
        .map(|r| (r.uid.as_str(), r.id))
        .chain(local.steps.iter().map(|r| (r.uid.as_str(), r.id)))
        .chain(local.memories.iter().map(|r| (r.uid.as_str(), r.id)))
        .chain(local.messages.iter().map(|r| (r.uid.as_str(), r.id)))
        .chain(local.attachments.iter().map(|r| (r.uid.as_str(), r.id)))
        .collect();
    rows.into_iter()
        .map(|mut row| {
            row.id = ids.get(row.uid.as_str()).copied();
            row
        })
        .collect()
}

async fn send_batches<T: Serialize + Clone>(
    session: &mut Session,
    rows: &[T],
    wrap: impl Fn(Vec<T>) -> Message,
) -> Result<(), SyncError> {
    let mut batch = Vec::new();
    let mut size = 0;
    for row in rows {
        let row_size = serde_json::to_vec(row).map(|v| v.len()).unwrap_or(0);
        if !batch.is_empty() && size + row_size > BATCH_BYTES {
            session.send(&wrap(std::mem::take(&mut batch))).await?;
            size = 0;
        }
        batch.push(row.clone());
        size += row_size;
    }
    if !batch.is_empty() {
        session.send(&wrap(batch)).await?;
    }
    Ok(())
}

async fn send_file(session: &mut Session, hash: &str, bytes: &[u8]) -> Result<(), SyncError> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut chunks = bytes.chunks(FILE_CHUNK_BYTES).peekable();
    if chunks.peek().is_none() {
        return session
            .send(&Message::File {
                hash: hash.to_string(),
                data: String::new(),
                last: true,
            })
            .await;
    }
    while let Some(chunk) = chunks.next() {
        session
            .send(&Message::File {
                hash: hash.to_string(),
                data: engine.encode(chunk),
                last: chunks.peek().is_none(),
            })
            .await?;
    }
    Ok(())
}

async fn receive_all(
    session: &mut Session,
    ctx: &Context,
    device_id: &str,
    peer_device_id: &str,
) -> Result<Outcome, SyncError> {
    let mut t = Transport::default();
    let mut received_bytes = 0;
    let peer_counter = loop {
        let (message, len) = session.recv_with_len().await?;
        received_bytes += len;
        if received_bytes > MAX_ROWS_BYTES {
            return Err(SyncError::Malformed("too many rows".to_string()));
        }
        match message {
            Message::Tasks { rows } => t.tasks.extend(rows),
            Message::Steps { rows } => t.steps.extend(rows),
            Message::Memories { rows } => t.memories.extend(rows),
            Message::Messages { rows } => t.messages.extend(rows),
            Message::Attachments { rows } => t.attachments.extend(rows),
            Message::RowsEnd { counter } => break counter,
            other => return Err(unexpected(&other)),
        }
    };
    validate::check(&t).map_err(SyncError::InvalidRows)?;
    let by_file = files::rows_by_file(&t.attachments);

    // 既に持っている実体も、行と合うかを確かめる(運んだときだけ外れる行を作らない)。
    let mut needed = Vec::new();
    let mut invalid = Vec::new();
    for (hash, rows) in &by_file {
        let store = ctx.attachments.clone();
        let wanted = hash.to_string();
        let existing = blocking::run(move || {
            if store.contains(&wanted)? {
                store.read(&wanted).map(Some)
            } else {
                Ok(None)
            }
        })
        .await?;
        match existing {
            Some(bytes) => invalid.extend(files::check(hash, &bytes, rows)),
            None => needed.push(hash.to_string()),
        }
    }
    if !invalid.is_empty() {
        return Err(SyncError::InvalidRows(invalid));
    }
    session
        .send(&Message::NeedFiles {
            hashes: needed.clone(),
        })
        .await?;
    let received = receive_files(session, ctx, &by_file, needed).await?;

    // 戻したDBは、戻す前に渡した自分の番号を、届いた版の中に持つ(`docs/sync/index.md` 10節)。
    let counter = db::with_conn(ctx.db.clone(), db::sync::device_state)
        .await?
        .counter;
    if rolled_back(&t, device_id, counter) {
        device::renew_device_id(ctx.db.clone(), ctx.registry.clone()).await?;
    }

    let mut counts = t.counts();
    counts.files = received;
    let peer = peer_device_id.to_string();
    let counter = db::with_conn(ctx.db.clone(), move |conn| {
        db::sync::import(conn, |importer| {
            if importer.has_data()? {
                return Ok(None);
            }
            convert::import(importer, &t)?;
            importer.finish(&peer, peer_counter).map(Some)
        })
    })
    .await?
    .ok_or(SyncError::NoLongerEmpty)?;
    session.send(&Message::Done { counter }).await?;
    Ok(Outcome::Received(counts))
}

/// 届いた版に、自分の端末IDで自分のカウンターより大きい番号があるか。
fn rolled_back(t: &Transport, device_id: &str, counter: i64) -> bool {
    let versions = t
        .tasks
        .iter()
        .map(|r| &r.version)
        .chain(t.steps.iter().map(|r| &r.version))
        .chain(t.memories.iter().map(|r| &r.version))
        .chain(t.messages.iter().map(|r| &r.version));
    versions
        .filter_map(|v| v.get(device_id))
        .any(|n| *n > counter)
}

/// 求めた実体を受け取り、行と合うことを確かめてから置く。
async fn receive_files(
    session: &mut Session,
    ctx: &Context,
    by_file: &HashMap<&str, Vec<&AttachmentRow>>,
    needed: Vec<String>,
) -> Result<usize, SyncError> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut waiting: HashSet<String> = needed.into_iter().collect();
    let mut received = 0;
    // 組み立て中の実体と、受け取った塊の数。
    let mut current: Option<(String, Vec<u8>, usize)> = None;
    loop {
        match session.recv().await? {
            Message::File { hash, data, last } => {
                if !waiting.contains(&hash) || current.as_ref().is_some_and(|(h, _, _)| *h != hash)
                {
                    return Err(SyncError::Malformed(
                        "a file that was not asked for".to_string(),
                    ));
                }
                let rows = &by_file[hash.as_str()];
                // 行は検証済みで、同じ実体を指す行の大きさは揃っている。
                let size = usize::try_from(rows[0].size_bytes).unwrap_or(usize::MAX);
                let (_, bytes, chunks) =
                    current.get_or_insert_with(|| (hash.clone(), Vec::new(), 0));
                *chunks += 1;
                let chunk = engine
                    .decode(data)
                    .map_err(|_| SyncError::Malformed("file data".to_string()))?;
                if bytes.len() + chunk.len() > size || *chunks > size / FILE_CHUNK_BYTES + 1 {
                    return Err(SyncError::Malformed(
                        "a file larger than its row".to_string(),
                    ));
                }
                bytes.extend_from_slice(&chunk);
                if last {
                    let (hash, bytes, _) = current.take().expect("a file in progress");
                    let invalid = files::check(&hash, &bytes, rows);
                    if !invalid.is_empty() {
                        return Err(SyncError::InvalidRows(invalid));
                    }
                    let store = ctx.attachments.clone();
                    blocking::run(move || store.put(&bytes)).await?;
                    waiting.remove(&hash);
                    received += 1;
                }
            }
            Message::FilesEnd if waiting.is_empty() && current.is_none() => return Ok(received),
            other => return Err(unexpected(&other)),
        }
    }
}

#[cfg(test)]
mod tests;
