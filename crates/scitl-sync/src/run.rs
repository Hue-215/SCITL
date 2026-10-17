//! 画面から始める同期(`docs/sync/index.md`「画面とCLI」)。画面は始めることと「やめる」、競合ごとに選んだ側を
//! 伝え、途中経過を受け取るだけで、待ち受け・接続・結合の順序はここが持つ。始める前に、ネイティブの
//! ダイアログで利用者に確かめる(同期を始めるコマンドは、乗っ取られた画面も呼べるため)。

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Mutex;

use futures::channel::oneshot;
use futures::future::BoxFuture;
use serde::Serialize;

use scitl_core::db::messages::Chat;
use scitl_core::i18n::{self, Language};
use scitl_core::in_flight::{InFlightSet, Occupied, StopSignal};
use scitl_core::qr_scan::ScanError;
use scitl_core::settings::QueryPause;
use scitl_core::{blocking, db, text};

use crate::conflict::{ConflictChoice, ConflictView};
use crate::invitation::{self, QrModules};
use crate::transfer::{cancellable, Confirmer, Resolver, Stage, WriteAnswer};
use crate::transport::Counts;
use crate::write_plan::WritePlan;
use crate::{
    confirm_device_id, connect, local_addresses, parse_invitation, parse_peer_address, sync,
    Context, Invitation, Limits, Listener, LocalVersions, Outcome, PairingCode, Session, SyncError,
};

/// 始める前の確認のダイアログに出す文面(表示言語)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDialog {
    pub title: String,
    pub message: String,
    pub confirm_label: String,
    pub cancel_label: String,
}

/// 始める前の確認のダイアログの答え。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogAnswer {
    Confirmed,
    Declined,
    /// 出せなかった(別のダイアログが出ている等)。
    NotShown,
}

/// 画面へ知らせる途中経過と、その時点で止められるか。画面は`stoppable`が偽の間は「戻る」を出さない。
/// 数字を含むので`Debug`を持たない(ログに出さない)。
#[derive(Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub struct SyncEvent {
    pub stoppable: bool,
    #[serde(flatten)]
    pub progress: SyncProgress,
}

impl From<SyncProgress> for SyncEvent {
    fn from(progress: SyncProgress) -> Self {
        Self {
            stoppable: progress.stoppable(),
            progress,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncProgress {
    /// 待ち受けを始めた。相手の端末で打つアドレス(候補のすべて)と、読み上げやすく区切った数字と、
    /// 同じものを読ませるQR。
    Listening {
        addresses: Vec<String>,
        code: String,
        qr: QrModules,
    },
    /// 相手を確かめ終え、同期を始めた。
    Syncing,
    /// 待ち受ける側が、利用者に競合を選ばせている(つなぐ側で知らせる)。
    PeerChoosing,
    /// 待ち受ける側が、書き込む前に確認を見せている(つなぐ側で知らせる)。
    PeerConfirming,
    /// 競合。単位ごとにどちらを取るかを[`answer`]で返す。
    Conflicts { conflicts: Vec<ConflictView> },
    /// 書き込む前の確認。件数を見せて、書き込むか戻るかを[`approve_write`]で返す。
    Confirm { plan: WritePlan },
    /// 書き始める段に入った。ここからは止められない。
    Finishing,
}

impl SyncProgress {
    /// 止める指示を受け付けるか。書き始めたあとに止めると、書き終えたのに止めたように見えるので、
    /// 止めない(`transfer::sync`)。確認中の「戻る」は、書き込む前の確認の答えになり、止めない。
    fn stoppable(&self) -> bool {
        match self {
            Self::Listening { .. }
            | Self::Syncing
            | Self::PeerChoosing
            | Self::PeerConfirming
            | Self::Conflicts { .. }
            | Self::Confirm { .. } => true,
            Self::Finishing => false,
        }
    }
}

/// 同期の結果。失敗は種類([`SyncError::kind`])と、英語の診断文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SyncReport {
    Synced {
        changed: Counts,
        conflicts: usize,
    },
    /// 確認のダイアログで取りやめたか、「やめる」で止めた。
    Cancelled,
    Failed {
        kind: String,
        detail: String,
    },
}

impl From<Result<Outcome, SyncError>> for SyncReport {
    fn from(result: Result<Outcome, SyncError>) -> Self {
        match result {
            Ok(Outcome { changed, conflicts }) => Self::Synced { changed, conflicts },
            Err(SyncError::Cancelled) => Self::Cancelled,
            Err(e) => Self::Failed {
                kind: e.kind().to_string(),
                detail: e.to_string(),
            },
        }
    }
}

/// 同期を始めるのに要るもの。
pub struct Request<'a> {
    pub ctx: &'a Context,
    /// 応答を生成中の会話。同期の間は全体を押さえ、送信・編集・タスクの操作を受け付けない
    /// (「同期1回の流れ」)。生成中の会話があれば始めない。
    pub generating: &'a InFlightSet<Chat>,
    /// 設定からの推論サーバー・MCPサーバーへの問い合わせ。同期の間は止める(乗っ取られた画面に、
    /// 数字をモデル名等に入れてLANの上へ送らせない。「画面とCLI」)。
    pub queries: &'a QueryPause,
    pub answers: &'a ConflictAnswers,
    /// 書き込む前の確認の答え(`ConfirmAsk`)。
    pub write_answers: &'a WriteAnswers,
    pub lang: Language,
}

/// 画面が選んだ側を待つ受け口。同期1回につき、競合を見せている間だけ埋まる。
#[derive(Default)]
pub struct ConflictAnswers(Mutex<Option<Waiting>>);

struct Waiting {
    ids: HashSet<String>,
    reply: oneshot::Sender<Vec<ConflictChoice>>,
}

/// 見せている競合の、選んだ側を渡す。見せている競合のすべてにちょうど1つずつ選んでいなければ、
/// 受け取らずに`false`を返す(待ち続ける)。
pub fn answer(answers: &ConflictAnswers, choices: Vec<ConflictChoice>) -> bool {
    let mut slot = answers.0.lock().expect("answers mutex poisoned");
    let chosen: HashSet<&str> = choices.iter().map(|c| c.id.as_str()).collect();
    let complete = slot.as_ref().is_some_and(|w| {
        chosen.len() == choices.len()
            && chosen.len() == w.ids.len()
            && w.ids.iter().all(|id| chosen.contains(id.as_str()))
    });
    if !complete {
        return false;
    }
    let waiting = slot.take().expect("checked above");
    waiting.reply.send(choices).is_ok()
}

/// 競合を画面に見せ、選んだ側を待つ。
struct Ask<'a, E> {
    answers: &'a ConflictAnswers,
    events: &'a E,
}

impl<E: Fn(SyncEvent) + Sync> Resolver for Ask<'_, E> {
    fn choose(&self, conflicts: Vec<ConflictView>) -> BoxFuture<'_, Option<Vec<ConflictChoice>>> {
        let (reply, chosen) = oneshot::channel();
        *self.answers.0.lock().expect("answers mutex poisoned") = Some(Waiting {
            ids: conflicts.iter().map(|c| c.id.clone()).collect(),
            reply,
        });
        (self.events)(SyncProgress::Conflicts { conflicts }.into());
        Box::pin(async move { chosen.await.ok() })
    }
}

/// 書き込む前の確認の答えの受け口。同期1回につき、確認の画面を見せている間だけ埋まる。
#[derive(Default)]
pub struct WriteAnswers(Mutex<Option<oneshot::Sender<bool>>>);

/// 確認の画面の答えを渡す(`write`が真なら書き込む)。見せている確認が無ければ`false`。
pub fn approve_write(answers: &WriteAnswers, write: bool) -> bool {
    let sender = answers.0.lock().expect("write answers poisoned").take();
    match sender {
        Some(sender) => sender.send(write).is_ok(),
        None => false,
    }
}

/// 書き込む前に、件数を画面に出し、答えを待つ。
struct ConfirmAsk<'a, E> {
    answers: &'a WriteAnswers,
    events: &'a E,
}

impl<E: Fn(SyncEvent) + Sync> Confirmer for ConfirmAsk<'_, E> {
    fn confirm(&self, plan: WritePlan) -> BoxFuture<'_, Option<WriteAnswer>> {
        let (reply, answer) = oneshot::channel();
        *self.answers.0.lock().expect("write answers mutex poisoned") = Some(reply);
        (self.events)(SyncProgress::Confirm { plan }.into());
        Box::pin(async move {
            answer.await.ok().map(|write| {
                if write {
                    WriteAnswer::Write
                } else {
                    WriteAnswer::Back
                }
            })
        })
    }
}

/// 待ち受けて、つないできた相手と同期する。`port`は[`Listener::bind`]の`fixed`。
pub async fn listen(
    request: Request<'_>,
    port: Option<u16>,
    confirm: impl FnOnce(&SyncDialog) -> DialogAnswer + Send + 'static,
    events: impl Fn(SyncEvent) + Sync,
) -> SyncReport {
    let dialog = SyncDialog {
        title: i18n::text(request.lang, "sync_dialog.listen_title").to_string(),
        message: i18n::format(
            request.lang,
            "sync_dialog.listen_message",
            &[(
                "minutes",
                &(Limits::default().listen_window.as_secs() / 60).to_string(),
            )],
        ),
        confirm_label: i18n::text(request.lang, "sync_dialog.listen_confirm").to_string(),
        cancel_label: i18n::text(request.lang, "common.cancel").to_string(),
    };
    let answers = request.answers;
    let write_answers = request.write_answers;
    let target = async { Ok(()) };
    run(
        request,
        target,
        |()| dialog,
        confirm,
        |ctx, stop, ()| async move {
            let (device_id, session) = cancellable(&stop, async {
                let device_id = confirm_device_id(ctx.db.clone(), ctx.registry.clone()).await?;
                let listener = Listener::bind(&local_addresses()?, port, Limits::default()).await?;
                events(
                    SyncProgress::Listening {
                        addresses: listener
                            .addresses()
                            .iter()
                            .map(ToString::to_string)
                            .collect(),
                        code: listener.code().spaced(),
                        qr: invitation::modules(&invitation::encode(
                            listener.addresses(),
                            listener.code(),
                        )),
                    }
                    .into(),
                );
                Ok((device_id, listener.accept(local_versions()).await?))
            })
            .await?;
            sync_over(
                session,
                ctx,
                &device_id,
                &stop,
                answers,
                write_answers,
                &events,
            )
            .await
        },
    )
    .await
}

/// 待ち受けている相手へつなぎ、同期する。`address`と`code`は利用者が打ったまま受け取る。
pub async fn connect_to(
    request: Request<'_>,
    address: &str,
    code: &str,
    confirm: impl FnOnce(&SyncDialog) -> DialogAnswer + Send + 'static,
    events: impl Fn(SyncEvent) + Sync,
) -> SyncReport {
    match parse_peer_address(address).and_then(|a| Ok((a, PairingCode::parse(code)?))) {
        Ok((address, code)) => {
            connect_with(
                request,
                async { Ok((vec![address], code)) },
                confirm,
                events,
            )
            .await
        }
        Err(e) => Err(e).into(),
    }
}

/// 待ち受ける側のQRを`scan`で読み、その候補へ順につないで同期する。`scan`は、利用者が読むのを
/// やめたら`None`を返す。読むのは全体を押さえてからで、止める指示が来たら`scan`を捨てる。
pub async fn connect_scanned(
    request: Request<'_>,
    scan: impl std::future::Future<Output = Result<Option<String>, ScanError>>,
    confirm: impl FnOnce(&SyncDialog) -> DialogAnswer + Send + 'static,
    events: impl Fn(SyncEvent) + Sync,
) -> SyncReport {
    let target = async {
        let text = scan.await?.ok_or(SyncError::Cancelled)?;
        let Invitation { addresses, code } = parse_invitation(&text)?;
        Ok((addresses, code))
    };
    connect_with(request, target, confirm, events).await
}

async fn connect_with(
    request: Request<'_>,
    target: impl std::future::Future<Output = Result<(Vec<SocketAddr>, PairingCode), SyncError>>,
    confirm: impl FnOnce(&SyncDialog) -> DialogAnswer + Send + 'static,
    events: impl Fn(SyncEvent) + Sync,
) -> SyncReport {
    let lang = request.lang;
    let dialog = move |(addresses, _): &(Vec<SocketAddr>, PairingCode)| {
        let shown: Vec<String> = addresses
            .iter()
            .map(|a| text::reveal_invisible_line(&a.to_string()))
            .collect();
        SyncDialog {
            title: i18n::text(lang, "sync_dialog.connect_title").to_string(),
            message: i18n::format(
                lang,
                "sync_dialog.connect_message",
                &[("address", &shown.join(", "))],
            ),
            confirm_label: i18n::text(lang, "sync_dialog.connect_confirm").to_string(),
            cancel_label: i18n::text(lang, "common.cancel").to_string(),
        }
    };
    let answers = request.answers;
    let write_answers = request.write_answers;
    run(
        request,
        target,
        dialog,
        confirm,
        |ctx, stop, (addresses, code)| async move {
            let (device_id, session) = cancellable(&stop, async {
                let device_id = confirm_device_id(ctx.db.clone(), ctx.registry.clone()).await?;
                let session =
                    connect(&addresses, &code, local_versions(), Limits::default()).await?;
                Ok((device_id, session))
            })
            .await?;
            sync_over(
                session,
                ctx,
                &device_id,
                &stop,
                answers,
                write_answers,
                &events,
            )
            .await
        },
    )
    .await
}

async fn sync_over(
    mut session: Session,
    ctx: &Context,
    device_id: &str,
    stop: &StopSignal,
    answers: &ConflictAnswers,
    write_answers: &WriteAnswers,
    events: &(impl Fn(SyncEvent) + Sync),
) -> Result<Outcome, SyncError> {
    events(SyncProgress::Syncing.into());
    let stage = |stage| {
        events(
            match stage {
                Stage::PeerChoosing => SyncProgress::PeerChoosing,
                Stage::PeerConfirming => SyncProgress::PeerConfirming,
                Stage::Finishing => SyncProgress::Finishing,
            }
            .into(),
        )
    };
    let ask = Ask { answers, events };
    let confirm = ConfirmAsk {
        answers: write_answers,
        events,
    };
    let outcome = sync(&mut session, ctx, device_id, stop, stage, &ask, &confirm).await;
    // 止めたあとに届いた選択・答えを受け取らない。
    answers.0.lock().expect("answers mutex poisoned").take();
    write_answers
        .0
        .lock()
        .expect("write answers mutex poisoned")
        .take();
    session.close().await;
    outcome
}

/// 進んでいる同期に止める指示を出す。同期していなければ`false`。
pub fn cancel(generating: &InFlightSet<Chat>) -> bool {
    generating.request_stop_all()
}

fn local_versions() -> LocalVersions {
    LocalVersions {
        db_version: db::schema_version(),
    }
}

/// 全体を押さえ、`target`で相手を決め(QRを読む等。止める指示が来たら捨てる)、確かめてから`sync`を
/// 走らせる。`sync`には止める指示を渡す。止められるのは、自分が書き始める前まで(`transfer::sync`)。
async fn run<'c, T, F, Fut>(
    request: Request<'c>,
    target: impl std::future::Future<Output = Result<T, SyncError>>,
    dialog: impl FnOnce(&T) -> SyncDialog,
    confirm: impl FnOnce(&SyncDialog) -> DialogAnswer + Send + 'static,
    sync: F,
) -> SyncReport
where
    F: FnOnce(&'c Context, StopSignal, T) -> Fut,
    Fut: std::future::Future<Output = Result<Outcome, SyncError>>,
{
    let all = match request.generating.try_begin_all() {
        Ok(all) => all,
        Err(Occupied::Keys) => return Err(SyncError::Generating).into(),
        Err(Occupied::All) => return Err(SyncError::AlreadySyncing).into(),
    };
    let _paused = request.queries.pause();
    let target = match cancellable(all.stop_signal(), target).await {
        Ok(target) => target,
        Err(e) => return Err(e).into(),
    };
    let dialog = dialog(&target);
    match blocking::run(move || Ok(confirm(&dialog))).await {
        Ok(DialogAnswer::Confirmed) => {}
        Ok(DialogAnswer::Declined) => return SyncReport::Cancelled,
        Ok(DialogAnswer::NotShown) => return Err(SyncError::DialogUnavailable).into(),
        Err(e) => return Err(SyncError::Core(e)).into(),
    }
    if all.stop_signal().is_requested() {
        return SyncReport::Cancelled;
    }
    // 承認してから終えるまで(待ち受けを含む)。Androidでは、この間フォアグラウンドサービスにして、
    // 裏へ回ったり画面が消えたりしても接続を切られないようにする。承認の直後は前に出ているので始められる。
    let _long_running = all.long_running();
    sync(request.ctx, all.stop_signal().clone(), target)
        .await
        .into()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use scitl_core::attachments::AttachmentStore;
    use scitl_core::CoreError;

    use super::*;
    use crate::transport::AbortReason;
    use crate::DeviceRegistry;

    struct Registry;

    impl DeviceRegistry for Registry {
        fn is_current(&self, _: &str, _: &str) -> Result<bool, CoreError> {
            Ok(true)
        }

        fn register(&self, _: &str, _: &str) -> Result<(), CoreError> {
            Ok(())
        }
    }

    fn context(dir: &tempfile::TempDir) -> Context {
        Context {
            db: Arc::new(Mutex::new(db::open_in_memory().unwrap())),
            attachments: AttachmentStore::new(dir.path().join("b"), dir.path().join("r")),
            registry: Arc::new(Registry),
        }
    }

    /// 全種類。網羅的な`match`を置き、種類を足したらここがコンパイルエラーになるようにする。
    fn every_error() -> Vec<SyncError> {
        let io = || std::io::Error::other("x");
        let all = vec![
            SyncError::AddressNotAllowed(String::new()),
            SyncError::NoLocalAddress,
            SyncError::Bind(io()),
            SyncError::PortInUse(0),
            SyncError::Connect(io()),
            SyncError::Io(io()),
            SyncError::Tls(rustls::Error::DecryptError),
            SyncError::TimedOut,
            SyncError::WindowExpired,
            SyncError::CodeMismatch,
            SyncError::PeerClosed,
            SyncError::FrameTooLarge { len: 0, max: 0 },
            SyncError::Malformed(String::new()),
            SyncError::ProtocolMismatch { ours: 1, theirs: 2 },
            SyncError::DbVersionMismatch { ours: 1, theirs: 2 },
            SyncError::DeviceCheck(CoreError::Cancelled),
            SyncError::SameDevice,
            SyncError::InvalidRows(Vec::new()),
            SyncError::Changed,
            SyncError::Mismatch,
            SyncError::PeerRefused(AbortReason::InvalidRows),
            SyncError::PeerRefused(AbortReason::Changed),
            SyncError::PeerRefused(AbortReason::Mismatch),
            SyncError::PeerRefused(AbortReason::Cancelled),
            SyncError::PeerRefused(AbortReason::Failed),
            SyncError::Core(CoreError::Cancelled),
            SyncError::InvalidCode,
            SyncError::InvalidInvitation,
            SyncError::Scan(ScanError::CameraDenied),
            SyncError::Scan(ScanError::Unavailable(String::new())),
            SyncError::Generating,
            SyncError::AlreadySyncing,
            SyncError::DialogUnavailable,
            SyncError::Cancelled,
        ];
        for e in &all {
            match e {
                SyncError::AddressNotAllowed(_)
                | SyncError::NoLocalAddress
                | SyncError::Bind(_)
                | SyncError::PortInUse(_)
                | SyncError::Connect(_)
                | SyncError::Io(_)
                | SyncError::Tls(_)
                | SyncError::TimedOut
                | SyncError::WindowExpired
                | SyncError::CodeMismatch
                | SyncError::PeerClosed
                | SyncError::FrameTooLarge { .. }
                | SyncError::Malformed(_)
                | SyncError::ProtocolMismatch { .. }
                | SyncError::DbVersionMismatch { .. }
                | SyncError::DeviceCheck(_)
                | SyncError::SameDevice
                | SyncError::InvalidRows(_)
                | SyncError::Changed
                | SyncError::Mismatch
                | SyncError::PeerRefused(_)
                | SyncError::Core(_)
                | SyncError::InvalidCode
                | SyncError::InvalidInvitation
                | SyncError::Scan(_)
                | SyncError::Generating
                | SyncError::AlreadySyncing
                | SyncError::DialogUnavailable
                | SyncError::Cancelled => {}
            }
        }
        all
    }

    #[test]
    fn every_failure_has_a_message_in_every_language() {
        // 止めたことは失敗ではなく、取りやめとして返す。
        for e in every_error()
            .into_iter()
            .filter(|e| !matches!(e, SyncError::Cancelled))
        {
            let key = format!("{}{}", crate::error::MESSAGE_KEY_PREFIX, e.kind());
            for lang in Language::ALL {
                assert_ne!(i18n::text(lang, &key), key, "{key} in {lang:?}");
            }
        }
    }

    fn no_dialog(_: &SyncDialog) -> DialogAnswer {
        panic!("the dialog must not be shown")
    }

    #[tokio::test]
    async fn a_mistyped_address_or_code_is_refused_before_asking() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        for (address, code, kind) in [
            ("example.com:1", "123456", "invalid_address"),
            ("192.168.1.5:50123", "12345", "invalid_code"),
        ] {
            let request = Request {
                ctx: &ctx,
                generating: &generating,
                queries: &QueryPause::default(),
                answers: &ConflictAnswers::default(),
                write_answers: &WriteAnswers::default(),
                lang: Language::DEFAULT,
            };
            let report = connect_to(request, address, code, no_dialog, |_| {}).await;
            assert!(
                matches!(&report, SyncReport::Failed { kind: k, .. } if k == kind),
                "{report:?}"
            );
        }
    }

    #[tokio::test]
    async fn syncing_does_not_start_while_a_response_is_generated() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        let _busy = generating.try_begin(Chat::General).unwrap();
        let request = Request {
            ctx: &ctx,
            generating: &generating,
            queries: &QueryPause::default(),
            answers: &ConflictAnswers::default(),
            write_answers: &WriteAnswers::default(),
            lang: Language::DEFAULT,
        };
        let report = listen(request, None, no_dialog, |_| {}).await;
        assert!(
            matches!(&report, SyncReport::Failed { kind, .. } if kind == "generating"),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn the_camera_is_not_opened_while_a_response_is_generated() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        let _busy = generating.try_begin(Chat::General).unwrap();
        let request = Request {
            ctx: &ctx,
            generating: &generating,
            queries: &QueryPause::default(),
            answers: &ConflictAnswers::default(),
            write_answers: &WriteAnswers::default(),
            lang: Language::DEFAULT,
        };
        let scan = async { panic!("the scanner must not be opened") };
        let report = connect_scanned(request, scan, no_dialog, |_| {}).await;
        assert!(
            matches!(&report, SyncReport::Failed { kind, .. } if kind == "generating"),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn stopping_while_scanning_drops_the_scan() {
        struct Dropped<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for Dropped<'_> {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        let dropped = std::sync::atomic::AtomicBool::new(false);
        let request = Request {
            ctx: &ctx,
            generating: &generating,
            queries: &QueryPause::default(),
            answers: &ConflictAnswers::default(),
            write_answers: &WriteAnswers::default(),
            lang: Language::DEFAULT,
        };
        let scan = async {
            let _guard = Dropped(&dropped);
            std::future::pending::<Result<Option<String>, ScanError>>().await
        };
        let stop = async {
            while !cancel(&generating) {
                tokio::task::yield_now().await;
            }
        };
        let (report, ()) = tokio::join!(connect_scanned(request, scan, no_dialog, |_| {}), stop);
        assert_eq!(report, SyncReport::Cancelled);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert!(generating.try_begin(Chat::General).is_some());
    }

    #[tokio::test]
    async fn a_qr_code_that_is_not_ours_is_refused_before_asking() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        let request = Request {
            ctx: &ctx,
            generating: &generating,
            queries: &QueryPause::default(),
            answers: &ConflictAnswers::default(),
            write_answers: &WriteAnswers::default(),
            lang: Language::DEFAULT,
        };
        let scan = async { Ok(Some("scitl-sync;1;012345;example.com:443".to_string())) };
        let report = connect_scanned(request, scan, no_dialog, |_| {}).await;
        // 読んだ中身を詳細に載せない(画面へ返さない)。
        assert!(
            matches!(&report, SyncReport::Failed { kind, detail }
                if kind == "invalid_qr" && !detail.contains("example.com")),
            "{report:?}"
        );
    }

    #[tokio::test]
    async fn declining_the_dialog_starts_nothing_and_lets_chats_run_again() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        let events = Mutex::new(Vec::new());
        let request = Request {
            ctx: &ctx,
            generating: &generating,
            queries: &QueryPause::default(),
            answers: &ConflictAnswers::default(),
            write_answers: &WriteAnswers::default(),
            lang: Language::DEFAULT,
        };
        let report = listen(
            request,
            None,
            |_| DialogAnswer::Declined,
            |e| events.lock().unwrap().push(e),
        )
        .await;
        assert_eq!(report, SyncReport::Cancelled);
        assert!(events.lock().unwrap().is_empty());
        assert!(generating.try_begin(Chat::General).is_some());
    }

    /// 長く掛かる部分(Androidではフォアグラウンドサービス)には、承認してから入り、終えたら出る。
    /// 取りやめたときは入らない(サービスは前に出ている間にしか始められないので、承認の前には入らない)。
    #[tokio::test]
    async fn the_sync_runs_long_only_after_it_is_confirmed() {
        use scitl_core::in_flight::LongRunningKind;
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        for (confirmed, expected) in [
            (false, vec![]),
            (true, vec![Some(LongRunningKind::All), None]),
        ] {
            let reported = Arc::new(Mutex::new(Vec::new()));
            let generating = InFlightSet::watching_long_running({
                let reported = reported.clone();
                move |kind| reported.lock().unwrap().push(kind)
            });
            let request = Request {
                ctx: &ctx,
                generating: &generating,
                queries: &QueryPause::default(),
                answers: &ConflictAnswers::default(),
                write_answers: &WriteAnswers::default(),
                lang: Language::DEFAULT,
            };
            // 承認したら、つながらない宛先へつなごうとして失敗する。
            let report = connect_to(
                request,
                "127.0.0.1:1",
                "123456",
                move |_| {
                    if confirmed {
                        DialogAnswer::Confirmed
                    } else {
                        DialogAnswer::Declined
                    }
                },
                |_| {},
            )
            .await;
            if confirmed {
                assert!(matches!(report, SyncReport::Failed { .. }), "{report:?}");
            } else {
                assert_eq!(report, SyncReport::Cancelled);
            }
            assert_eq!(
                *reported.lock().unwrap(),
                expected,
                "confirmed: {confirmed}"
            );
        }
    }

    #[tokio::test]
    async fn queries_are_paused_while_the_sync_holds_everything() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        let queries: &'static QueryPause = Box::leak(Box::default());
        let request = Request {
            ctx: &ctx,
            generating: &generating,
            queries,
            answers: &ConflictAnswers::default(),
            write_answers: &WriteAnswers::default(),
            lang: Language::DEFAULT,
        };
        let paused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = std::sync::Arc::clone(&paused);
        let report = listen(
            request,
            None,
            move |_| {
                seen.store(queries.is_paused(), std::sync::atomic::Ordering::SeqCst);
                DialogAnswer::Declined
            },
            |_| {},
        )
        .await;
        assert_eq!(report, SyncReport::Cancelled);
        assert!(paused.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!queries.is_paused());
    }

    /// 出せなかったことは、取りやめと分けて失敗で返す。
    #[tokio::test]
    async fn a_dialog_that_cannot_be_shown_fails_and_lets_chats_run_again() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        let request = Request {
            ctx: &ctx,
            generating: &generating,
            queries: &QueryPause::default(),
            answers: &ConflictAnswers::default(),
            write_answers: &WriteAnswers::default(),
            lang: Language::DEFAULT,
        };
        let report = listen(request, None, |_| DialogAnswer::NotShown, |_| {}).await;
        assert!(
            matches!(&report, SyncReport::Failed { kind, .. } if kind == "dialog_unavailable"),
            "{report:?}"
        );
        assert!(generating.try_begin(Chat::General).is_some());
    }

    #[tokio::test]
    async fn a_second_sync_does_not_start_while_one_is_running() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(&dir);
        let generating = InFlightSet::new();
        let _syncing = generating.try_begin_all().unwrap();
        let request = Request {
            ctx: &ctx,
            generating: &generating,
            queries: &QueryPause::default(),
            answers: &ConflictAnswers::default(),
            write_answers: &WriteAnswers::default(),
            lang: Language::DEFAULT,
        };
        let report = listen(request, None, no_dialog, |_| {}).await;
        assert!(
            matches!(&report, SyncReport::Failed { kind, .. } if kind == "already_syncing"),
            "{report:?}"
        );
    }

    #[test]
    fn only_the_writing_stage_cannot_be_stopped() {
        let event = |progress: SyncProgress| SyncEvent::from(progress).stoppable;
        assert!(event(SyncProgress::Syncing));
        assert!(event(SyncProgress::PeerChoosing));
        assert!(event(SyncProgress::Conflicts {
            conflicts: Vec::new()
        }));
        assert!(!event(SyncProgress::Finishing));
    }

    /// 画面と写し合う値を、ts-rsの生成物と同じ置き場所(`frontend/src/bindings/`)へ書き出す
    /// (`scitl_core`の`i18n::tests::export_shared_constants`と同じ扱い)。
    #[test]
    fn export_sync_constants() {
        let dir = std::env::var("TS_RS_EXPORT_DIR").expect("set in .cargo/config.toml");
        let body = format!(
            "// This file was generated by a test in scitl-sync (`run::tests::export_sync_constants`). Do not edit this file manually.\n\
             \n\
             /** 同期の失敗の文言のキーの前置き(`error::MESSAGE_KEY_PREFIX`)。 */\n\
             export const SYNC_ERROR_KEY_PREFIX = {};\n",
            serde_json::to_string(crate::error::MESSAGE_KEY_PREFIX).unwrap(),
        );
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(std::path::Path::new(&dir).join("SyncConstants.ts"), body).unwrap();
    }
}
