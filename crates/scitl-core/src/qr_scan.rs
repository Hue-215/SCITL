//! カメラでQRを読む(Androidだけ。`docs/sync/index.md` 11節)。カメラはOSの機能なので、画面
//! (WebView)には読ませない。読み取りの画面は`gen/android`のKotlin(`ScanActivity`)にあり、ここからは
//! 開くこと・結果を引き取ること・閉じることだけを頼む。読んだ中身は確かめずに返す。

use std::time::Duration;

/// 読み取りを頼むKotlinのクラスの名前(パッケージは[`crate::APP_IDENTIFIER`])。
pub const SCANNER_CLASS: &str = "QrScanner";

/// Kotlin側が返す結果の、頭の1文字(種類)。Kotlin側の定数と揃える(scitl-tauriのテストが照合する)。
pub const SCANNED: &str = "S";
pub const CANCELLED: &str = "C";
pub const DENIED: &str = "D";
pub const FAILED: &str = "F";

/// 結果を引き取りに行く間隔。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// 読み取りの画面を開いたままにできる長さ。過ぎたら画面を閉じ、読めなかったことにする(画面が
/// 結果を置かずに消えたときに、待ち続けないため)。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
const GIVE_UP_AFTER: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// 利用者がカメラの使用を許可しなかった。
    #[error("camera permission was denied")]
    CameraDenied,
    /// カメラで読めない(カメラが無い・開けない、この端末では読まない等)。
    #[error("could not scan a QR code: {0}")]
    Unavailable(String),
}

/// カメラでQRを読み、中身を返す。利用者が読むのをやめたら`None`。返す前に捨てると(止める指示等)、
/// 読み取りの画面を閉じる。
#[cfg(target_os = "android")]
pub async fn scan() -> Result<Option<String>, ScanError> {
    android::scan().await
}

/// カメラでQRを読み、中身を返す。デスクトップでは読まない(アドレスと数字を打つ)。
#[cfg(not(target_os = "android"))]
pub async fn scan() -> Result<Option<String>, ScanError> {
    Err(ScanError::Unavailable(
        "QR scanning is only available on Android".to_string(),
    ))
}

/// この端末でQRを読めるか(Androidで、カメラがある)。読めない端末では、画面は読むボタンを出さない。
#[cfg(target_os = "android")]
pub fn available() -> bool {
    android::available()
}

/// この端末でQRを読めるか。デスクトップでは読まない。
#[cfg(not(target_os = "android"))]
pub fn available() -> bool {
    false
}

/// Kotlin側の結果を読む。`None`は、まだ読んでいない。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn outcome(raw: &str) -> Result<Option<String>, ScanError> {
    if let Some(text) = raw.strip_prefix(SCANNED) {
        Ok(Some(text.to_string()))
    } else if raw == CANCELLED {
        Ok(None)
    } else if raw == DENIED {
        Err(ScanError::CameraDenied)
    } else if let Some(message) = raw.strip_prefix(FAILED) {
        Err(ScanError::Unavailable(message.to_string()))
    } else {
        Err(ScanError::Unavailable(format!(
            "unexpected result from the scanner: {raw:?}"
        )))
    }
}

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{JString, JValue};
    use jni::{jni_sig, jni_str};
    use tokio::time::{sleep, Instant};

    use super::{outcome, ScanError, GIVE_UP_AFTER, POLL_INTERVAL, SCANNER_CLASS};
    use crate::android::{app_class, with_application};

    pub(super) fn available() -> bool {
        let available = with_application(|env, context| {
            let class = app_class(env, context, SCANNER_CLASS)?;
            env.call_static_method(
                &class,
                jni_str!("available"),
                jni_sig!("(Landroid/content/Context;)Z"),
                &[JValue::from(context)],
            )?
            .z()
        });
        available.unwrap_or_else(|e| {
            crate::diagnostics::report(format_args!("could not check for a camera: {e}"));
            false
        })
    }

    /// 捨てたときに読み取りを取りやめ、画面が開いていれば閉じる。結果が出たあとは何もしない。
    struct Open(i64);

    impl Drop for Open {
        fn drop(&mut self) {
            let id = self.0;
            let cancelled = with_application(|env, context| {
                let class = app_class(env, context, SCANNER_CLASS)?;
                env.call_static_method(
                    &class,
                    jni_str!("cancel"),
                    jni_sig!("(J)V"),
                    &[JValue::Long(id)],
                )?;
                Ok(())
            });
            if let Err(e) = cancelled {
                crate::diagnostics::report(format_args!("could not close the scanner: {e}"));
            }
        }
    }

    pub(super) async fn scan() -> Result<Option<String>, ScanError> {
        let id = with_application(|env, context| {
            let class = app_class(env, context, SCANNER_CLASS)?;
            env.call_static_method(
                &class,
                jni_str!("start"),
                jni_sig!("(Landroid/content/Context;)J"),
                &[JValue::from(context)],
            )?
            .j()
        })
        .map_err(ScanError::Unavailable)?;
        let open = Open(id);
        let deadline = Instant::now() + GIVE_UP_AFTER;
        while Instant::now() < deadline {
            sleep(POLL_INTERVAL).await;
            if let Some(raw) = poll(open.0).map_err(ScanError::Unavailable)? {
                return outcome(&raw);
            }
        }
        Err(ScanError::Unavailable(
            "gave up waiting for the scanner".to_string(),
        ))
    }

    fn poll(id: i64) -> Result<Option<String>, String> {
        with_application(|env, context| {
            let class = app_class(env, context, SCANNER_CLASS)?;
            let result = env
                .call_static_method(
                    &class,
                    jni_str!("poll"),
                    jni_sig!("(J)Ljava/lang/String;"),
                    &[JValue::Long(id)],
                )?
                .l()?;
            if result.is_null() {
                return Ok(None);
            }
            let result = env.cast_local::<JString>(result)?;
            Ok(Some(result.try_to_string(env)?))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_are_read_by_their_first_letter() {
        assert_eq!(
            outcome("Sscitl-sync;1;012345").unwrap().as_deref(),
            Some("scitl-sync;1;012345")
        );
        assert_eq!(outcome("S").unwrap().as_deref(), Some(""));
        assert!(outcome("C").unwrap().is_none());
        assert!(matches!(outcome("D"), Err(ScanError::CameraDenied)));
        assert!(
            matches!(outcome("Fno camera"), Err(ScanError::Unavailable(m)) if m == "no camera")
        );
        for raw in ["", "X", "あ", "Cx", "Dx"] {
            assert!(
                matches!(outcome(raw), Err(ScanError::Unavailable(_))),
                "{raw}"
            );
        }
    }
}
