//! 画面から始める同期(`docs/sync/index.md` 11節)。画面は始めることと「やめる」、競合ごとに選んだ側を
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
use scitl_core::in_flight::{InFlightSet, StopSignal};
use scitl_core::qr_scan::ScanError;
use scitl_core::{blocking, db, text};

use crate::conflict::{ConflictChoice, ConflictView};
use crate::invitation::{self, QrModules};
use crate::transfer::{cancellable, Resolver, Stage};
use crate::transport::Counts;
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

/// 画面へ知らせる途中経過。数字を含むので`Debug`を持たない(ログに出さない)。
#[derive(Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SyncEvent {
    /// 待ち受けを始めた。相手の端末で打つアドレス(候補のすべて)と数字と、同じものを読ませるQR。
    Listening {
        addresses: Vec<String>,
        code: String,
        qr: QrModules,
    },
    /// 相手を確かめ終え、同期を始めた。
    Syncing,
    /// 待ち受ける側が、利用者に競合を選ばせている(つなぐ側で知らせる)。
    PeerChoosing,
    /// 競合。単位ごとにどちらを取るかを[`answer`]で返す。
    Conflicts { conflicts: Vec<ConflictView> },
    /// 書き始める段に入った。ここからは止められない。
    Finishing,
}

/// 同期の結果。失敗は種類([`SyncError::kind`])と、英語の診断文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SyncReport {
    Synced {
        /// この端末で書いた行の数。
        changed: Counts,
        /// 選んだ競合の数。
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
    /// (4節)。生成中の会話があれば始めない。
    pub generating: &'a InFlightSet<Chat>,
    pub answers: &'a ConflictAnswers,
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
        (self.events)(SyncEvent::Conflicts { conflicts });
        Box::pin(async move { chosen.await.ok() })
    }
}

/// 待ち受けて、つないできた相手と同期する。
pub async fn listen(
    request: Request<'_>,
    confirm: impl FnOnce(&SyncDialog) -> bool + Send + 'static,
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
    let target = async { Ok(()) };
    run(
        request,
        target,
        |()| dialog,
        confirm,
        |ctx, stop, ()| async move {
            let (device_id, session) = cancellable(&stop, async {
                let device_id = confirm_device_id(ctx.db.clone(), ctx.registry.clone()).await?;
                let listener = Listener::bind(&local_addresses()?, Limits::default()).await?;
                events(SyncEvent::Listening {
                    addresses: listener
                        .addresses()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    code: listener.code().as_str().to_string(),
                    qr: invitation::modules(&invitation::encode(
                        listener.addresses(),
                        listener.code(),
                    )),
                });
                Ok((device_id, listener.accept(local_versions()).await?))
            })
            .await?;
            sync_over(session, ctx, &device_id, &stop, answers, &events).await
        },
    )
    .await
}

/// 待ち受けている相手へつなぎ、同期する。`address`と`code`は利用者が打ったまま受け取る。
pub async fn connect_to(
    request: Request<'_>,
    address: &str,
    code: &str,
    confirm: impl FnOnce(&SyncDialog) -> bool + Send + 'static,
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
    confirm: impl FnOnce(&SyncDialog) -> bool + Send + 'static,
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
    confirm: impl FnOnce(&SyncDialog) -> bool + Send + 'static,
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
            sync_over(session, ctx, &device_id, &stop, answers, &events).await
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
    events: &(impl Fn(SyncEvent) + Sync),
) -> Result<Outcome, SyncError> {
    events(SyncEvent::Syncing);
    let stage = |stage| {
        events(match stage {
            Stage::PeerChoosing => SyncEvent::PeerChoosing,
            Stage::Finishing => SyncEvent::Finishing,
        })
    };
    let ask = Ask { answers, events };
    let outcome = sync(&mut session, ctx, device_id, stop, stage, &ask).await;
    // 止めたあとに届いた選択を受け取らない。
    answers.0.lock().expect("answers mutex poisoned").take();
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
    confirm: impl FnOnce(&SyncDialog) -> bool + Send + 'static,
    sync: F,
) -> SyncReport
where
    F: FnOnce(&'c Context, StopSignal, T) -> Fut,
    Fut: std::future::Future<Output = Result<Outcome, SyncError>>,
{
    let Some(all) = request.generating.try_begin_all() else {
        return Err(SyncError::Busy).into();
    };
    let target = match cancellable(all.stop_signal(), target).await {
        Ok(target) => target,
        Err(e) => return Err(e).into(),
    };
    let dialog = dialog(&target);
    match blocking::run(move || Ok(confirm(&dialog))).await {
        Ok(true) => {}
        Ok(false) => return SyncReport::Cancelled,
        Err(e) => return Err(SyncError::Core(e)).into(),
    }
    if all.stop_signal().is_requested() {
        return SyncReport::Cancelled;
    }
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
        fn is_registered(&self, _: &str) -> Result<bool, CoreError> {
            Ok(true)
        }

        fn register(&self, _: &str) -> Result<(), CoreError> {
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
            SyncError::Busy,
            SyncError::Cancelled,
        ];
        for e in &all {
            match e {
                SyncError::AddressNotAllowed(_)
                | SyncError::NoLocalAddress
                | SyncError::Bind(_)
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
                | SyncError::Busy
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
            let key = format!("settings.sync.error.{}", e.kind());
            for lang in Language::ALL {
                assert_ne!(i18n::text(lang, &key), key, "{key} in {lang:?}");
            }
        }
    }

    fn no_dialog(_: &SyncDialog) -> bool {
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
                answers: &ConflictAnswers::default(),
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
            answers: &ConflictAnswers::default(),
            lang: Language::DEFAULT,
        };
        let report = listen(request, no_dialog, |_| {}).await;
        assert!(
            matches!(&report, SyncReport::Failed { kind, .. } if kind == "busy"),
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
            answers: &ConflictAnswers::default(),
            lang: Language::DEFAULT,
        };
        let scan = async { panic!("the scanner must not be opened") };
        let report = connect_scanned(request, scan, no_dialog, |_| {}).await;
        assert!(
            matches!(&report, SyncReport::Failed { kind, .. } if kind == "busy"),
            "{report:?}"
        );
    }

    /// 読んでいる間の「やめる」で、読み取りを捨てて取りやめにする(捨てると読み取りの画面が閉じる)。
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
            answers: &ConflictAnswers::default(),
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
            answers: &ConflictAnswers::default(),
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
            answers: &ConflictAnswers::default(),
            lang: Language::DEFAULT,
        };
        let report = listen(request, |_| false, |e| events.lock().unwrap().push(e)).await;
        assert_eq!(report, SyncReport::Cancelled);
        assert!(events.lock().unwrap().is_empty());
        assert!(generating.try_begin(Chat::General).is_some());
    }
}
