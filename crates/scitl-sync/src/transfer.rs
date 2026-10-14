//! 同期1回の流れ(`docs/sync/index.md` 4節)。つなぐ側(B)が差分を送り、待ち受ける側(A)が結合して
//! 確定し、結果を返す。Bはそれを確定し、照合の値で同じ結果になったことを確かめる。
//!
//! 片方が空でも同じ流れで、初めての相手には全部の行を送る(カーソルが0)。途中で切れても、
//! やり直せば揃う(版を確定してから送るので、同じ番号を別の編集に使い回さない)。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;
use futures::future::BoxFuture;
use serde::Serialize;
use sha2::{Digest, Sha256};

use scitl_core::attachments::AttachmentStore;
use scitl_core::blocking;
use scitl_core::db::sync::{self as db_sync, Cursor, Filter, Tx};
use scitl_core::db::{self, SharedConnection};
use scitl_core::in_flight::StopSignal;
use scitl_core::CoreError;

use crate::conflict::{self, ConflictChoice, ConflictView};
use crate::convert::{self, Writer};
use crate::merge::{self, Conflict, Inputs, Pick, Units};
use crate::transport::{AbortReason, AttachmentRow, Counts, Message, Transport};
use crate::validate::{self, Known};
use crate::{device, files, DeviceRegistry, InvalidRow, Session, SyncError};

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

/// 同期が触るもの。
pub struct Context {
    pub db: SharedConnection,
    pub attachments: AttachmentStore,
    pub registry: Arc<dyn DeviceRegistry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outcome {
    /// この端末で書いた行の数。
    pub changed: Counts,
    /// 利用者が選んだ競合の数(待ち受ける側だけ)。
    pub conflicts: usize,
}

/// 途中の段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 待ち受ける側が、利用者に競合を選ばせている(つなぐ側で知らせる)。
    PeerChoosing,
    /// 書き始める段に入った。ここからは止められない。
    Finishing,
}

/// 競合を利用者に選ばせる(待ち受ける側)。取りやめたら`None`。
pub trait Resolver: Sync {
    fn choose(&self, conflicts: Vec<ConflictView>) -> BoxFuture<'_, Option<Vec<ConflictChoice>>>;
}

/// 確かめ終えた接続の上で同期する。`device_id`は確かめた端末ID(`device::confirm_device_id`)。
///
/// `stop`に止める指示が来たら[`SyncError::Cancelled`]で終える。止められるのは、自分が書き始める
/// 前まで。それより後に止めると、書き終えたのに止めたように見えるので、止めずに終わりまで進め、
/// その段に入ったことを`stage`で知らせる。
pub async fn sync(
    session: &mut Session,
    ctx: &Context,
    side: Side,
    device_id: &str,
    stop: &StopSignal,
    stage: impl Fn(Stage) + Sync,
    resolver: &dyn Resolver,
) -> Result<Outcome, SyncError> {
    let result = run(session, ctx, side, device_id, stop, &stage, resolver).await;
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
        SyncError::Changed => Some(AbortReason::Changed),
        SyncError::Mismatch => Some(AbortReason::Mismatch),
        SyncError::Cancelled => Some(AbortReason::Cancelled),
        SyncError::PeerRefused(_)
        | SyncError::PeerClosed
        | SyncError::Io(_)
        | SyncError::Tls(_)
        | SyncError::TimedOut
        | SyncError::SameDevice => None,
        _ => Some(AbortReason::Failed),
    }
}

/// 伝え合ったあとに両側が同じに決める値(7節)。
struct Agreed {
    /// 自分が送る行の位置(自分の番号。これより後に変わった行を送る)。
    send_from: i64,
}

async fn run(
    session: &mut Session,
    ctx: &Context,
    side: Side,
    device_id: &str,
    stop: &StopSignal,
    stage: &(dyn Fn(Stage) + Sync),
    resolver: &dyn Resolver,
) -> Result<Outcome, SyncError> {
    let agreed = cancellable(stop, introduce(session, ctx, side, device_id)).await?;
    match side {
        Side::Connector => connector(session, ctx, &agreed, stop, stage).await,
        Side::Listener => listener(session, ctx, &agreed, stop, stage, resolver).await,
    }
}

/// 止める指示が来るまで`future`を待つ。来たら落として[`SyncError::Cancelled`]。
async fn cancellable<T>(
    stop: &StopSignal,
    future: impl std::future::Future<Output = Result<T, SyncError>>,
) -> Result<T, SyncError> {
    stop.unless_requested(future)
        .await
        .unwrap_or(Err(SyncError::Cancelled))
}

/// 同期の層の読み書きを1つのトランザクションで行う。`f`が失敗すれば何も残さない。
async fn transaction<T: Send + 'static>(
    db: SharedConnection,
    f: impl FnOnce(&Tx<'_>) -> Result<T, SyncError> + Send + 'static,
) -> Result<T, SyncError> {
    db::with_conn(db, move |conn| {
        let mut failure = None;
        let result = db_sync::transaction(conn, |tx| {
            f(tx).map_err(|e| {
                failure = Some(e);
                // 取り消すための値。下で`failure`に置き換える。
                CoreError::Cancelled
            })
        });
        Ok(match (result, failure) {
            (_, Some(e)) => Err(e),
            (Ok(value), None) => Ok(value),
            (Err(e), None) => Err(SyncError::Core(e)),
        })
    })
    .await?
}

async fn exchange(session: &mut Session, side: Side, ours: &Message) -> Result<Message, SyncError> {
    match side {
        Side::Connector => {
            session.send(ours).await?;
            session.recv().await
        }
        Side::Listener => {
            let theirs = session.recv().await?;
            session.send(ours).await?;
            Ok(theirs)
        }
    }
}

/// 端末IDとカーソルを伝え合い、送る位置を決める(7節)。どちらかの番号が巻き戻っていれば、
/// 両方とも全部を送り直し、巻き戻った側は端末IDを作り直す(10節)。
async fn introduce(
    session: &mut Session,
    ctx: &Context,
    side: Side,
    device_id: &str,
) -> Result<Agreed, SyncError> {
    let theirs = exchange(
        session,
        side,
        &Message::Intro {
            device_id: device_id.to_string(),
        },
    )
    .await?;
    let Message::Intro {
        device_id: peer_device_id,
    } = theirs
    else {
        return Err(unexpected(&theirs));
    };
    if !validate::is_hex(&peer_device_id, 32) {
        return Err(SyncError::Malformed("device ID".to_string()));
    }
    if peer_device_id == device_id {
        return Err(SyncError::SameDevice);
    }

    let peer = peer_device_id.clone();
    let (counter, cursor, seen_of_peer) = transaction(ctx.db.clone(), move |tx| {
        let cursor = tx.cursor(&peer)?;
        let seen = tx.seen_of(&peer)?.max(cursor.received_seq);
        Ok((tx.device_state()?.counter, cursor, seen))
    })
    .await?;
    let theirs = exchange(
        session,
        side,
        &Message::State {
            counter,
            sent_seq: cursor.sent_seq,
            received_seq: cursor.received_seq,
            seen_of_you: seen_of_peer,
        },
    )
    .await?;
    let Message::State {
        counter: peer_counter,
        received_seq: peer_received,
        seen_of_you,
        ..
    } = theirs
    else {
        return Err(unexpected(&theirs));
    };
    let rolled_back_here = seen_of_you > counter;
    let rolled_back_there = seen_of_peer > peer_counter;
    if rolled_back_here {
        device::renew_device_id(ctx.db.clone(), ctx.registry.clone()).await?;
    }
    let send_from = if rolled_back_here || rolled_back_there {
        0
    } else {
        cursor.sent_seq.min(peer_received).max(0)
    };
    Ok(Agreed { send_from })
}

/// 受け取った版に、この端末の番号として、まだ振っていない番号が無いこと。正しい相手なら、巻き戻りは
/// 伝え合う段で分かっている(7節)。ここで外れる行を取り込むと、この端末のあとの編集が古い番号に負ける。
fn check_own_numbers(t: &Transport, me: &db_sync::DeviceState) -> Result<(), SyncError> {
    let versions = t
        .tasks
        .iter()
        .map(|r| &r.version)
        .chain(t.steps.iter().map(|r| &r.version))
        .chain(t.memories.iter().map(|r| &r.version))
        .chain(t.messages.iter().map(|r| &r.version));
    for version in versions {
        if version.get(&me.device_id).is_some_and(|n| *n > me.counter) {
            return Err(SyncError::Malformed(
                "a version has a number this device has not given".to_string(),
            ));
        }
    }
    Ok(())
}

fn unexpected(message: &Message) -> SyncError {
    match message {
        Message::Abort { reason } => SyncError::PeerRefused(*reason),
        _ => SyncError::Malformed("unexpected message".to_string()),
    }
}

/// 自分の行を読んで確かめる(送る側の検証。外れた行は、その端末の連番を添えて示す)。
fn read_own(tx: &Tx<'_>, filter: Filter<'_>) -> Result<Transport, SyncError> {
    let rows = tx.rows(filter)?;
    let ids = convert::Ids::<i64, String>::from_uids(&tx.uids()?);
    let (transport, mut invalid) = convert::to_transport(&rows, &ids);
    if let Err(found) = validate::check(&transport, &Known::load(tx)?) {
        // 置き換えで外れた行は、検証でも同じ行が外れうる。1つの行は1度だけ示す。
        for row in with_local_ids(found, &rows) {
            if !invalid.iter().any(|r| r.uid == row.uid) {
                invalid.push(row);
            }
        }
    }
    if invalid.is_empty() {
        Ok(transport)
    } else {
        Err(SyncError::InvalidRows(invalid))
    }
}

/// 送る側で見つけた行に、その端末の中の連番を添える(どの行を直せばよいか分かるように)。
fn with_local_ids(rows: Vec<InvalidRow>, local: &db_sync::Rows) -> Vec<InvalidRow> {
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

// ---- つなぐ側(B)

async fn connector(
    session: &mut Session,
    ctx: &Context,
    agreed: &Agreed,
    stop: &StopSignal,
    stage: &(dyn Fn(Stage) + Sync),
) -> Result<Outcome, SyncError> {
    // 4: 印のある行に版を振って確定し、前回Aへ渡した位置より後に変わった行を送る。
    let send_from = agreed.send_from;
    let (delta, counter) = transaction(ctx.db.clone(), move |tx| {
        tx.stamp()?;
        Ok((
            read_own(tx, Filter::Since(send_from))?,
            tx.device_state()?.counter,
        ))
    })
    .await?;
    let (merged, merged_counter, digest, peer_device_id, sent_uids) = cancellable(stop, async {
        check_own_files(ctx, &delta.attachments).await?;
        send_rows(session, &delta).await?;
        session.send(&Message::RowsEnd).await?;
        serve_listener(session, ctx, delta, counter, stage).await
    })
    .await?;

    // 8: Aの結果を確かめ、無い実体を受け取ってから、1つのトランザクションで書く。
    let known = transaction(ctx.db.clone(), |tx| Ok(Known::load(tx)?)).await?;
    validate::check(&merged, &known).map_err(SyncError::InvalidRows)?;
    let files = cancellable(
        stop,
        receive_needed_files(session, ctx, &merged.attachments),
    )
    .await?;
    if stop.is_requested() {
        return Err(SyncError::Cancelled);
    }
    stage(Stage::Finishing);

    let peer = peer_device_id.clone();
    let state = db::with_conn(ctx.db.clone(), db_sync::device_state).await?;
    check_own_numbers(&merged, &state)?;
    let mut digest_uids = sent_uids;
    digest_uids.extend(merged.uids());
    let mut changed = transaction(ctx.db.clone(), move |tx| {
        if tx.device_state()?.counter != counter {
            return Err(SyncError::Changed);
        }
        let mut writer = Writer::new(tx)?;
        let uids = merged.uids();
        let (local, _) = convert::read(tx, Filter::Uids(&uids))?;
        apply(&mut writer, &merged, &local)?;
        let changed = writer.finish()?;
        tx.clear_changed()?;
        let written = tx.device_state()?.counter;
        tx.record_cursor(
            &peer,
            Cursor {
                sent_seq: written,
                received_seq: merged_counter,
            },
        )?;
        if digest_of(tx, digest_uids)? != digest {
            return Err(SyncError::Mismatch);
        }
        Ok((changed, written))
    })
    .await?;
    changed.0.files = files;
    session
        .send(&Message::Done {
            counter: changed.1,
            device_id: current_device_id(ctx).await?,
        })
        .await?;
    Ok(Outcome {
        changed: changed.0,
        conflicts: 0,
    })
}

async fn current_device_id(ctx: &Context) -> Result<String, SyncError> {
    Ok(db::with_conn(ctx.db.clone(), db_sync::device_state)
        .await?
        .device_id)
}

/// Aの結果を書く。Aの結果はBの行を版で含むか同じなので、決まらない行があれば、どこかが
/// 食い違っている。
fn apply(
    writer: &mut Writer<'_, '_>,
    incoming: &Transport,
    local: &Transport,
) -> Result<(), SyncError> {
    fn rows<R: merge::Versioned>(
        incoming: &[R],
        local: &[R],
        mut write: impl FnMut(&R) -> Result<(), CoreError>,
    ) -> Result<(), SyncError> {
        let local: HashMap<&str, &R> = local.iter().map(|r| (r.uid(), r)).collect();
        for r in incoming {
            match local.get(r.uid()) {
                None => write(r)?,
                Some(l) => match merge::merge_pair(*l, r) {
                    Some(m) if m != **l => write(&m)?,
                    Some(_) => {}
                    None => return Err(SyncError::Mismatch),
                },
            }
        }
        Ok(())
    }
    rows(&incoming.tasks, &local.tasks, |r| writer.task(r))?;
    rows(&incoming.steps, &local.steps, |r| writer.step(r))?;
    rows(&incoming.memories, &local.memories, |r| writer.memory(r))?;
    rows(&incoming.messages, &local.messages, |r| writer.message(r))?;
    for r in &incoming.attachments {
        writer.attachment(r)?;
    }
    Ok(())
}

/// Aの求め(単位の行・実体)に応え、Aの結果を受け取る。結果の行と、Aが書き終えた時点の
/// カウンター、照合の値、Aの端末ID(巻き戻りで作り直していれば新しい値)、Aへ送った行の`uid`を返す。
async fn serve_listener(
    session: &mut Session,
    ctx: &Context,
    delta: Transport,
    counter: i64,
    stage: &(dyn Fn(Stage) + Sync),
) -> Result<(Transport, i64, String, String, Vec<String>), SyncError> {
    let mut sent_uids = delta.uids();
    let mut sent_attachments = delta.attachments;
    let mut merged = Transport::default();
    let mut received_bytes = 0;
    loop {
        let (message, len) = session.recv_with_len().await?;
        received_bytes += len;
        if received_bytes > MAX_ROWS_BYTES {
            return Err(SyncError::Malformed("too many rows".to_string()));
        }
        match message {
            Message::NeedUnits { tasks, general } => {
                let units = transaction(ctx.db.clone(), move |tx| {
                    // 差分を読んだあとに別の書き込みがあれば、Aが見る単位の行と差分が食い違う。
                    if tx.device_state()?.counter != counter {
                        return Err(SyncError::Changed);
                    }
                    read_own(
                        tx,
                        Filter::Units {
                            tasks: &tasks,
                            general,
                        },
                    )
                })
                .await?;
                check_own_files(ctx, &units.attachments).await?;
                send_rows(session, &units).await?;
                session.send(&Message::UnitsEnd).await?;
                sent_uids.extend(units.uids());
                sent_attachments.extend(units.attachments);
            }
            Message::NeedFiles { hashes } => {
                serve_files(session, ctx, &sent_attachments, hashes).await?
            }
            Message::Choosing => stage(Stage::PeerChoosing),
            Message::Tasks { rows } => merged.tasks.extend(rows),
            Message::Steps { rows } => merged.steps.extend(rows),
            Message::Memories { rows } => merged.memories.extend(rows),
            Message::Messages { rows } => merged.messages.extend(rows),
            Message::Attachments { rows } => merged.attachments.extend(rows),
            Message::Merged {
                counter,
                digest,
                device_id,
            } => {
                if !validate::is_hex(&device_id, 32) {
                    return Err(SyncError::Malformed("device ID".to_string()));
                }
                return Ok((merged, counter, digest, device_id, sent_uids));
            }
            other => return Err(unexpected(&other)),
        }
    }
}

// ---- 待ち受ける側(A)

/// Aが結合に使う、自分の側の読み取り(1つのトランザクションで、版を振ってから読む)。
struct LocalView {
    counter: i64,
    device_id: String,
    /// 前回Bへ渡した位置より後に変えた行(送る側として確かめた行)。
    changed: Transport,
    /// 受け取った行と同じ`uid`の行と、そろった単位のすべての行。
    local: Transport,
}

async fn listener(
    session: &mut Session,
    ctx: &Context,
    agreed: &Agreed,
    stop: &StopSignal,
    stage: &(dyn Fn(Stage) + Sync),
    resolver: &dyn Resolver,
) -> Result<Outcome, SyncError> {
    let delta = cancellable(stop, receive_rows(session, End::Rows)).await?;
    let mut fetched = Transport::default();
    let mut complete = Units::default();
    let mut choices: HashMap<String, Pick> = HashMap::new();
    let mut have_files: HashSet<String> = HashSet::new();
    let mut files_received = 0;
    let mut finishing = false;
    let send_from = agreed.send_from;

    let (mut changed, merged_counter, back, digest) = loop {
        let incoming = combine(&delta, &fetched, &complete);
        // 5: 印のある行に版を振り、受け取った行と突き合わせる自分の行を読む。
        let view = {
            let uids = incoming.uids();
            let complete = complete.clone();
            transaction(ctx.db.clone(), move |tx| {
                tx.stamp()?;
                let state = tx.device_state()?;
                let changed = read_own(tx, Filter::Since(send_from))?;
                let (mut local, _) = convert::read(tx, Filter::Uids(&uids))?;
                let tasks: Vec<String> = complete.tasks.iter().cloned().collect();
                let (units, _) = convert::read(
                    tx,
                    Filter::Units {
                        tasks: &tasks,
                        general: complete.general,
                    },
                )?;
                extend_unique(&mut local, units);
                let known = Known::load(tx)?;
                validate::check(&incoming, &known).map_err(SyncError::InvalidRows)?;
                check_own_numbers(&incoming, &state)?;
                Ok(LocalView {
                    counter: state.counter,
                    device_id: state.device_id,
                    changed,
                    local,
                })
            })
            .await?
        };
        if !finishing {
            cancellable(stop, check_own_files(ctx, &view.changed.attachments)).await?;
        }
        let incoming = combine(&delta, &fetched, &complete);
        let plan = merge::plan(&Inputs {
            incoming: &incoming,
            local: &view.local,
            complete: &complete,
            changed_here: &Units::of(&view.changed),
        });
        if !plan.invalid.is_empty() {
            return Err(SyncError::InvalidRows(plan.invalid));
        }
        if !plan.needs.is_empty() {
            let needs = plan.needs.clone();
            let rows = cancellable(stop, fetch_units(session, &needs)).await?;
            extend_unique(&mut fetched, rows);
            complete.extend(&needs);
            continue;
        }

        // 足す添付の実体のうち、無いものをBに求める(行を書く前に置く。5節)。
        let wanted: Vec<AttachmentRow> = incoming
            .attachments
            .iter()
            .filter(|a| !view.local.attachments.iter().any(|l| l.uid == a.uid))
            .filter(|a| {
                a.file_hash
                    .as_ref()
                    .is_some_and(|h| !have_files.contains(h))
            })
            .cloned()
            .collect();
        if !wanted.is_empty() {
            files_received +=
                cancellable(stop, receive_needed_files(session, ctx, &wanted)).await?;
            have_files.extend(wanted.iter().filter_map(|a| a.file_hash.clone()));
        }

        // 選んでいない競合を利用者に選ばせる。
        let pending: Vec<&Conflict> = plan
            .conflicts
            .iter()
            .filter(|c| !choices.contains_key(&c.unit.id()))
            .collect();
        if !pending.is_empty() {
            let views: Vec<ConflictView> = pending.iter().map(|c| conflict::view(c)).collect();
            let ids: HashSet<String> = views.iter().map(|v| v.id.clone()).collect();
            session.send(&Message::Choosing).await?;
            // 選んでいる間に相手がやめたら、選ばせたまま確定しない。
            let chosen = cancellable(stop, async {
                tokio::select! {
                    chosen = resolver.choose(views) => chosen.ok_or(SyncError::Cancelled),
                    message = session.recv::<Message>() => Err(match message {
                        Ok(message) => unexpected(&message),
                        Err(e) => e,
                    }),
                }
            })
            .await?;
            let chosen: HashMap<String, Pick> = chosen
                .into_iter()
                .filter(|c| ids.contains(&c.id))
                .map(|c| (c.id, c.side.into()))
                .collect();
            if chosen.len() != ids.len() {
                return Err(SyncError::Core(CoreError::Internal(
                    "not every conflict was chosen".to_string(),
                )));
            }
            choices.extend(chosen);
        }

        // 6: 確定する。版を振ってから別の書き込みがあれば、振るところからやり直す。
        if !finishing {
            if stop.is_requested() {
                return Err(SyncError::Cancelled);
            }
            stage(Stage::Finishing);
            finishing = true;
        }
        let choices = choices.clone();
        let committed = transaction(ctx.db.clone(), move |tx| {
            if tx.device_state()?.counter != view.counter {
                return Ok(None);
            }
            commit(tx, &plan, &choices, &view.device_id, &incoming, send_from).map(Some)
        })
        .await?;
        if let Some(committed) = committed {
            break committed;
        }
    };

    // 7: Bに結果と照合の値を返し、Bが求める実体を送る。
    send_rows(session, &back).await?;
    session
        .send(&Message::Merged {
            counter: merged_counter,
            digest,
            device_id: current_device_id(ctx).await?,
        })
        .await?;
    let (peer_written, peer_device_id) = loop {
        match session.recv().await? {
            Message::NeedFiles { hashes } => {
                serve_files(session, ctx, &back.attachments, hashes).await?
            }
            Message::Done { counter, device_id } if validate::is_hex(&device_id, 32) => {
                break (counter, device_id)
            }
            other => return Err(unexpected(&other)),
        }
    };
    db::with_conn(ctx.db.clone(), move |conn| {
        db_sync::record_cursor(
            conn,
            &peer_device_id,
            Cursor {
                sent_seq: merged_counter,
                received_seq: peer_written,
            },
        )
    })
    .await?;
    changed.files = files_received;
    Ok(Outcome {
        changed,
        conflicts: choices.len(),
    })
}

/// 受け取った行。そろった単位の行は、単位ごと求めた行に置き換える(同じ行と、Bの会話の並び)。
fn combine(delta: &Transport, fetched: &Transport, complete: &Units) -> Transport {
    let in_complete_chat = |task: &Option<String>| match task {
        Some(uid) => complete.tasks.contains(uid),
        None => complete.general,
    };
    let mut out = Transport {
        tasks: delta
            .tasks
            .iter()
            .filter(|r| !complete.tasks.contains(&r.uid))
            .cloned()
            .collect(),
        steps: delta
            .steps
            .iter()
            .filter(|r| !complete.tasks.contains(&r.task))
            .cloned()
            .collect(),
        memories: delta.memories.clone(),
        messages: delta
            .messages
            .iter()
            .filter(|r| !in_complete_chat(&r.task))
            .cloned()
            .collect(),
        attachments: delta.attachments.clone(),
    };
    extend_unique(&mut out, fetched.clone());
    out
}

/// `extra`のうち、まだ無い`uid`の行を後ろに足す。
fn extend_unique(t: &mut Transport, extra: Transport) {
    fn add<R>(rows: &mut Vec<R>, extra: Vec<R>, uid: impl Fn(&R) -> &str) {
        let seen: HashSet<String> = rows.iter().map(|r| uid(r).to_string()).collect();
        rows.extend(extra.into_iter().filter(|r| !seen.contains(uid(r))));
    }
    add(&mut t.tasks, extra.tasks, |r| &r.uid);
    add(&mut t.steps, extra.steps, |r| &r.uid);
    add(&mut t.memories, extra.memories, |r| &r.uid);
    add(&mut t.messages, extra.messages, |r| &r.uid);
    add(&mut t.attachments, extra.attachments, |r| &r.uid);
}

/// 結合の結果を書き、Bへ返す行と照合の値を作る(4節の6と7)。書いた行の数、書き終えた時点の
/// カウンター、Bへ返す行、照合の値を返す。
fn commit(
    tx: &Tx<'_>,
    plan: &merge::Plan,
    choices: &HashMap<String, Pick>,
    me: &str,
    incoming: &Transport,
    send_from: i64,
) -> Result<(Counts, i64, Transport, String), SyncError> {
    let number = tx.next_number()?;
    let now = db::now_iso8601();
    let mut rows = plan.rows.clone();
    for conflict in &plan.conflicts {
        let pick = choices
            .get(&conflict.unit.id())
            .copied()
            .ok_or_else(|| CoreError::Internal("a conflict was not chosen".to_string()))?;
        let resolved = merge::resolve(conflict, pick, me, number, &now);
        rows.tasks.extend(resolved.tasks);
        rows.steps.extend(resolved.steps);
        rows.memories.extend(resolved.memories);
        rows.messages.extend(resolved.messages);
        rows.attachments.extend(resolved.attachments);
    }
    let mut writer = Writer::new(tx)?;
    for r in &rows.tasks {
        writer.task(r)?;
    }
    for r in &rows.steps {
        writer.step(r)?;
    }
    for r in &rows.memories {
        writer.memory(r)?;
    }
    for r in &rows.messages {
        writer.message(r)?;
    }
    for r in &rows.attachments {
        writer.attachment(r)?;
    }
    for record in &plan.records {
        writer.task_state_record(&record.task, &record.uid, &record.created_at)?;
    }
    let mut changed = writer.finish()?;

    // 同じ本文のメモリを畳む。編集として扱い、版を進める(8節)。
    let alive = tx.alive_memories()?;
    let duplicates: HashSet<&str> =
        merge::duplicate_memories(alive.iter().map(|m| (m.uid.as_str(), m.content.as_str())))
            .into_iter()
            .collect();
    for memory in alive.iter().filter(|m| duplicates.contains(m.uid.as_str())) {
        let mut version: BTreeMap<String, i64> = serde_json::from_str(&memory.version)
            .map_err(|e| CoreError::Internal(format!("unreadable version: {e}")))?;
        version.insert(me.to_string(), number);
        tx.update_memory(&db_sync::MemoryRow {
            deleted_at: Some(now.clone()),
            version: convert::version_json(&version),
            ..memory.clone()
        })?;
        changed.memories += 1;
    }
    tx.clear_changed()?;
    let counter = tx.device_state()?.counter;

    // Bへ返すのは、前回Bへ渡した位置より後に変わった行のうち、Bが送ってきたものと違う行。
    let (written, invalid) = convert::read(tx, Filter::Since(send_from))?;
    if !invalid.is_empty() {
        return Err(SyncError::InvalidRows(invalid));
    }
    let back = without_echo(written, incoming);
    validate::check(&back, &Known::load(tx)?).map_err(SyncError::InvalidRows)?;
    let mut uids = incoming.uids();
    uids.extend(back.uids());
    let digest = digest_of(tx, uids)?;
    Ok((changed, counter, back, digest))
}

/// `incoming`と同じ行を除く(試行は端末ごとに見え方が違いうるので比べない)。
fn without_echo(mut written: Transport, incoming: &Transport) -> Transport {
    fn keep<R: Serialize>(rows: &mut Vec<R>, incoming: &[R], uid: impl Fn(&R) -> &str) {
        let sent: HashMap<&str, Vec<u8>> = incoming
            .iter()
            .map(|r| (uid(r), serde_json::to_vec(r).expect("rows serialize")))
            .collect();
        rows.retain(|r| sent.get(uid(r)) != Some(&serde_json::to_vec(r).expect("rows serialize")));
    }
    let incoming_messages: Vec<_> = incoming
        .messages
        .iter()
        .map(|m| crate::transport::MessageRow {
            attempt: None,
            ..m.clone()
        })
        .collect();
    keep(&mut written.tasks, &incoming.tasks, |r| &r.uid);
    keep(&mut written.steps, &incoming.steps, |r| &r.uid);
    keep(&mut written.memories, &incoming.memories, |r| &r.uid);
    let sent: HashMap<&str, &crate::transport::MessageRow> = incoming_messages
        .iter()
        .map(|m| (m.uid.as_str(), m))
        .collect();
    written.messages.retain(|m| {
        sent.get(m.uid.as_str()).is_none_or(|s| {
            **s != crate::transport::MessageRow {
                attempt: None,
                ..m.clone()
            }
        })
    });
    keep(&mut written.attachments, &incoming.attachments, |r| &r.uid);
    written
}

/// 照合の値(4節)。今回やり取りした行の、確定後の状態を、運ぶ形で`uid`の順に並べたもののハッシュ。
/// 試行は入れない。
fn digest_of(tx: &Tx<'_>, mut uids: Vec<String>) -> Result<String, SyncError> {
    uids.sort_unstable();
    uids.dedup();
    let (mut t, _) = convert::read(tx, Filter::Uids(&uids))?;
    t.tasks.sort_by(|a, b| a.uid.cmp(&b.uid));
    t.steps.sort_by(|a, b| a.uid.cmp(&b.uid));
    t.memories.sort_by(|a, b| a.uid.cmp(&b.uid));
    t.messages.sort_by(|a, b| a.uid.cmp(&b.uid));
    t.attachments.sort_by(|a, b| a.uid.cmp(&b.uid));
    for m in &mut t.messages {
        m.attempt = None;
    }
    fn feed<R: Serialize>(hasher: &mut Sha256, rows: &[R]) {
        for r in rows {
            hasher.update(serde_json::to_vec(r).expect("rows serialize"));
            hasher.update(b"\n");
        }
    }
    let mut hasher = Sha256::new();
    feed(&mut hasher, &t.tasks);
    feed(&mut hasher, &t.steps);
    feed(&mut hasher, &t.memories);
    feed(&mut hasher, &t.messages);
    feed(&mut hasher, &t.attachments);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Bに単位の行を求めて受け取る。
async fn fetch_units(session: &mut Session, units: &Units) -> Result<Transport, SyncError> {
    session
        .send(&Message::NeedUnits {
            tasks: units.tasks.iter().cloned().collect(),
            general: units.general,
        })
        .await?;
    receive_rows(session, End::Units).await
}

/// 受け取る行の終わりの印。
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Rows,
    Units,
}

async fn receive_rows(session: &mut Session, end: End) -> Result<Transport, SyncError> {
    let mut t = Transport::default();
    let mut received_bytes = 0;
    loop {
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
            Message::RowsEnd if end == End::Rows => return Ok(t),
            Message::UnitsEnd if end == End::Units => return Ok(t),
            other => return Err(unexpected(&other)),
        }
    }
}

async fn send_rows(session: &mut Session, t: &Transport) -> Result<(), SyncError> {
    send_batches(session, &t.tasks, |rows| Message::Tasks { rows }).await?;
    send_batches(session, &t.steps, |rows| Message::Steps { rows }).await?;
    send_batches(session, &t.memories, |rows| Message::Memories { rows }).await?;
    send_batches(session, &t.messages, |rows| Message::Messages { rows }).await?;
    send_batches(session, &t.attachments, |rows| Message::Attachments {
        rows,
    })
    .await
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

async fn read_file(store: &AttachmentStore, hash: &str) -> Result<Vec<u8>, SyncError> {
    let store = store.clone();
    let hash = hash.to_string();
    Ok(blocking::run(move || store.read(&hash)).await?)
}

/// 送る前に、行が指す自分の実体を確かめる(送る側で外れた行を示すため。`docs/sync/index.md` 5節)。
async fn check_own_files(ctx: &Context, rows: &[AttachmentRow]) -> Result<(), SyncError> {
    let mut invalid = Vec::new();
    for (hash, rows) in files::rows_by_file(rows) {
        match existing_file(ctx, hash).await? {
            Some(bytes) => invalid.extend(files::check(hash, &bytes, &rows)),
            None => invalid.extend(rows.iter().map(|r| InvalidRow {
                table: "attachments",
                id: None,
                uid: r.uid.clone(),
                reason: "the file is missing".to_string(),
            })),
        }
    }
    if invalid.is_empty() {
        Ok(())
    } else {
        Err(SyncError::InvalidRows(invalid))
    }
}

async fn existing_file(ctx: &Context, hash: &str) -> Result<Option<Vec<u8>>, SyncError> {
    let store = ctx.attachments.clone();
    let wanted = hash.to_string();
    Ok(blocking::run(move || {
        if store.contains(&wanted)? {
            store.read(&wanted).map(Some)
        } else {
            Ok(None)
        }
    })
    .await?)
}

/// 相手の求めに応えて実体を送る。送った行が指す実体だけを送る。
async fn serve_files(
    session: &mut Session,
    ctx: &Context,
    sent: &[AttachmentRow],
    hashes: Vec<String>,
) -> Result<(), SyncError> {
    let sent = files::rows_by_file(sent);
    for hash in &hashes {
        if !sent.contains_key(hash.as_str()) {
            return Err(SyncError::Malformed(
                "asked for a file that was not sent".to_string(),
            ));
        }
        let bytes = read_file(&ctx.attachments, hash).await?;
        send_file(session, hash, &bytes).await?;
    }
    session.send(&Message::FilesEnd).await
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

/// 足す添付の行が指す実体のうち、無いものを求めて受け取り、確かめて置く。既に持っている実体も、
/// 行と合うかを確かめる(運んだときだけ外れる行を作らない)。受け取った実体の数を返す。
async fn receive_needed_files(
    session: &mut Session,
    ctx: &Context,
    rows: &[AttachmentRow],
) -> Result<usize, SyncError> {
    let by_file = files::rows_by_file(rows);
    let mut needed = Vec::new();
    let mut invalid = Vec::new();
    for (hash, rows) in &by_file {
        match existing_file(ctx, hash).await? {
            Some(bytes) => invalid.extend(files::check(hash, &bytes, rows)),
            None => needed.push(hash.to_string()),
        }
    }
    if !invalid.is_empty() {
        return Err(SyncError::InvalidRows(invalid));
    }
    needed.sort();
    session
        .send(&Message::NeedFiles {
            hashes: needed.clone(),
        })
        .await?;
    receive_files(session, ctx, &by_file, needed).await
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
