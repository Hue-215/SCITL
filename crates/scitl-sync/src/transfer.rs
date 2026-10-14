//! 丸ごとの移送(`docs/sync/index.md` 12節の段階2)。片方にタスク・発言・メモリが1件も無いときだけ
//! 受け付け、ある側から無い側へ全部を写す。結合はしないので、どちらにもデータがあれば断る。
//!
//! 流れ: 端末IDとデータの有無を伝え合い、写す側が版を振って全部の行を送る。受け取る側は検証し、
//! 自分に無い添付の実体を求め、実体を置いてから行を1つのトランザクションで書き、カーソルを書いて
//! 終わりを知らせる。写す側も、それを受けてカーソルを書く。

use std::collections::{HashMap, HashSet};

use base64::Engine;
use serde::Serialize;
use sha2::{Digest, Sha256};

use scitl_core::attachments::{classify, AttachmentStore};
use scitl_core::blocking;
use scitl_core::db::{self, SharedConnection};

use crate::transport::{AbortReason, Counts, Message, Transport};
use crate::{convert, validate, InvalidRow, Session, SyncError};

/// 1つのメッセージに詰める行の量の目安(直列化した大きさ)。
const BATCH_BYTES: usize = 4 * 1024 * 1024;
/// 添付の実体を分けて送る大きさ(base64にする前)。
const FILE_CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// 受け取る添付の実体1つの上限。実体はメモリに組み立ててから確かめて置く。
const MAX_FILE_BYTES: usize = 256 * 1024 * 1024;

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
        (false, true) => receive_all(session, ctx, &peer_device_id).await,
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
    if let Err(rows) = validate::check(&transport) {
        // 置き換えで外れた行は、検証でも同じ行が外れうる。1つの行は1度だけ示す。
        for row in with_local_ids(rows, &snapshot.rows) {
            if !invalid.iter().any(|r| r.uid == row.uid) {
                invalid.push(row);
            }
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
    let sent: HashSet<&str> = transport
        .attachments
        .iter()
        .filter_map(|a| a.file_hash.as_deref())
        .collect();
    let mut counts = transport.counts();
    for hash in &hashes {
        if !sent.contains(hash.as_str()) {
            return Err(SyncError::Malformed(
                "asked for a file that was not sent".to_string(),
            ));
        }
        let store = ctx.attachments.clone();
        let wanted = hash.clone();
        let bytes = blocking::run(move || store.read(&wanted))
            .await
            .map_err(|_| {
                SyncError::InvalidRows(attachment_rows_with(
                    hash,
                    &snapshot.rows,
                    "the file is missing",
                ))
            })?;
        if format!("{:x}", Sha256::digest(&bytes)) != *hash {
            return Err(SyncError::InvalidRows(attachment_rows_with(
                hash,
                &snapshot.rows,
                "the file does not match its hash",
            )));
        }
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

fn attachment_rows_with(hash: &str, local: &db::sync::Rows, reason: &str) -> Vec<InvalidRow> {
    local
        .attachments
        .iter()
        .filter(|a| a.file_hash.as_deref() == Some(hash))
        .map(|a| InvalidRow {
            table: "attachments",
            id: Some(a.id),
            uid: a.uid.clone(),
            reason: reason.to_string(),
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
    peer_device_id: &str,
) -> Result<Outcome, SyncError> {
    let mut t = Transport::default();
    let peer_counter = loop {
        match session.recv().await? {
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
    let files = expected_files(&t)?;

    let store = ctx.attachments.clone();
    let wanted: Vec<String> = files.keys().cloned().collect();
    let needed: Vec<String> = blocking::run(move || {
        wanted
            .into_iter()
            .filter_map(|hash| match store.contains(&hash) {
                Ok(true) => None,
                Ok(false) => Some(Ok(hash)),
                Err(e) => Some(Err(e)),
            })
            .collect()
    })
    .await?;
    session
        .send(&Message::NeedFiles {
            hashes: needed.clone(),
        })
        .await?;
    let received = receive_files(session, ctx, &files, needed).await?;

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

/// 運ぶ実体ごとに、行が言う大きさと、画像として扱う行のMIME。同じ実体を指す行の大きさが
/// 食い違えば、どれかの行が壊れている。
struct ExpectedFile {
    size: usize,
    image_mime: Option<String>,
}

fn expected_files(t: &Transport) -> Result<HashMap<String, ExpectedFile>, SyncError> {
    let mut files: HashMap<String, ExpectedFile> = HashMap::new();
    let mut invalid = Vec::new();
    for a in &t.attachments {
        let Some(hash) = &a.file_hash else { continue };
        let size = usize::try_from(a.size_bytes).unwrap_or(usize::MAX);
        if size > MAX_FILE_BYTES {
            invalid.push(InvalidRow {
                table: "attachments",
                id: None,
                uid: a.uid.clone(),
                reason: "the file is too large to sync".to_string(),
            });
            continue;
        }
        let entry = files.entry(hash.clone()).or_insert(ExpectedFile {
            size,
            image_mime: None,
        });
        if entry.size != size {
            invalid.push(InvalidRow {
                table: "attachments",
                id: None,
                uid: a.uid.clone(),
                reason: "the size differs from another attachment of the same file".to_string(),
            });
        }
        if a.kind == "image" {
            entry.image_mime = Some(a.mime_type.clone());
        }
    }
    if invalid.is_empty() {
        Ok(files)
    } else {
        Err(SyncError::InvalidRows(invalid))
    }
}

/// 求めた実体を受け取り、ハッシュ・大きさ・(画像なら)形式が行と合うことを確かめてから置く。
async fn receive_files(
    session: &mut Session,
    ctx: &Context,
    files: &HashMap<String, ExpectedFile>,
    needed: Vec<String>,
) -> Result<usize, SyncError> {
    let engine = base64::engine::general_purpose::STANDARD;
    let mut waiting: HashSet<String> = needed.into_iter().collect();
    let mut received = 0;
    let mut current: Option<(String, Vec<u8>)> = None;
    loop {
        match session.recv().await? {
            Message::File { hash, data, last } => {
                if !waiting.contains(&hash) || current.as_ref().is_some_and(|(h, _)| *h != hash) {
                    return Err(SyncError::Malformed(
                        "a file that was not asked for".to_string(),
                    ));
                }
                let expected = &files[&hash];
                let (_, bytes) = current.get_or_insert_with(|| (hash.clone(), Vec::new()));
                let chunk = engine
                    .decode(data)
                    .map_err(|_| SyncError::Malformed("file data".to_string()))?;
                if bytes.len() + chunk.len() > expected.size {
                    return Err(SyncError::Malformed(
                        "a file larger than its row".to_string(),
                    ));
                }
                bytes.extend_from_slice(&chunk);
                if last {
                    let (hash, bytes) = current.take().expect("a file in progress");
                    check_file(&hash, &bytes, expected)?;
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

fn check_file(hash: &str, bytes: &[u8], expected: &ExpectedFile) -> Result<(), SyncError> {
    let fail = |reason: &str| SyncError::Malformed(format!("file {hash}: {reason}"));
    if format!("{:x}", Sha256::digest(bytes)) != hash {
        return Err(fail("does not match its hash"));
    }
    if bytes.len() != expected.size {
        return Err(fail("does not match its size"));
    }
    // 画像は預かる時点で正規化してあり、形式は中身から決めた値。受け付けていた頃の「その他」は
    // 画像のMIMEを持ちうるので、種別が画像の行だけを見る。
    if let Some(mime) = &expected.image_mime {
        let classified = classify(bytes);
        if classified.kind.as_str() != "image" || classified.mime_type != mime {
            return Err(fail("is not the image its row says"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
