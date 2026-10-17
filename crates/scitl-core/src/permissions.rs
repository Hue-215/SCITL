//! Androidで、カメラと通知の許可をOSに求める前に、画面で説明を挟む(Issue #615。
//! `architecture/concurrency.md`「応答が終わったことの通知」・`../sync/index.md`「画面とCLI」)。
//!
//! 許可の状態はKotlin(`Permissions`)しか知らないので、JNIで引く。説明を出すかはここが決め、画面は
//! 結果に応じて出すだけにする。OSのダイアログは説明を飛ばされても残るので、乗っ取った画面が
//! 許可を付けることはできない。

/// 許可の状態を答えるKotlinのクラスの名前(パッケージは[`crate::APP_IDENTIFIER`])。
pub const PERMISSIONS_CLASS: &str = "Permissions";

/// Kotlin側が返すカメラの許可の状態。Kotlin側の定数と揃える(scitl-tauriのテストが照合する)。
pub const GRANTED: i32 = 0;
pub const ASK: i32 = 1;
pub const BLOCKED: i32 = 2;

/// 同期のQRを読む前の、カメラの許可の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
#[serde(rename_all = "snake_case")]
pub enum CameraPermission {
    Granted,
    /// 説明してから読み取りの画面を開く(OSのダイアログは読み取りの画面が出す)。
    Ask,
    /// OSがもうダイアログを出さない。設定アプリで許可してもらう。
    Blocked,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn camera_permission_from(code: i32) -> CameraPermission {
    match code {
        GRANTED => CameraPermission::Granted,
        BLOCKED => CameraPermission::Blocked,
        _ => CameraPermission::Ask,
    }
}

#[cfg(target_os = "android")]
pub use android::{answer_notification_prompt, camera, notification_prompt_due, open_settings};

/// デスクトップではQRを読まない(読むボタンを出さない)ので、説明も挟まない。
#[cfg(not(target_os = "android"))]
pub fn camera() -> CameraPermission {
    CameraPermission::Granted
}

#[cfg(not(target_os = "android"))]
pub fn open_settings() {}

/// デスクトップでは通知を出さない。
#[cfg(not(target_os = "android"))]
pub fn notification_prompt_due() -> bool {
    false
}

#[cfg(not(target_os = "android"))]
pub fn answer_notification_prompt(_allow: bool) {}

#[cfg(target_os = "android")]
mod android {
    use jni::objects::JValue;
    use jni::{jni_sig, jni_str};

    use super::{camera_permission_from, CameraPermission, PERMISSIONS_CLASS};
    use crate::android::{app_class, with_application};

    /// 引けなければ説明を出す側に倒す(OSのダイアログは読み取りの画面が出す)。
    pub fn camera() -> CameraPermission {
        let code = with_application(|env, context| {
            let class = app_class(env, context, PERMISSIONS_CLASS)?;
            env.call_static_method(
                &class,
                jni_str!("camera"),
                jni_sig!("(Landroid/content/Context;)I"),
                &[JValue::from(context)],
            )?
            .i()
        });
        match code {
            Ok(code) => camera_permission_from(code),
            Err(e) => {
                crate::diagnostics::report(format_args!(
                    "could not check the camera permission: {e}"
                ));
                CameraPermission::Ask
            }
        }
    }

    /// OSがもうダイアログを出さないときだけ、このアプリの設定画面を開く。
    pub fn open_settings() {
        if camera() != CameraPermission::Blocked {
            return;
        }
        let opened = with_application(|env, context| {
            let class = app_class(env, context, PERMISSIONS_CLASS)?;
            env.call_static_method(
                &class,
                jni_str!("openSettings"),
                jni_sig!("(Landroid/content/Context;)V"),
                &[JValue::from(context)],
            )?;
            Ok(())
        });
        if let Err(e) = opened {
            crate::diagnostics::report(format_args!("could not open the app settings: {e}"));
        }
    }

    /// 通知の許可を求める説明を出すか。Android 13以上で、許可が無く、まだ求めていないとき。
    pub fn notification_prompt_due() -> bool {
        let due = with_application(|env, context| {
            let class = app_class(env, context, PERMISSIONS_CLASS)?;
            env.call_static_method(
                &class,
                jni_str!("notificationPromptDue"),
                jni_sig!("(Landroid/content/Context;)Z"),
                &[JValue::from(context)],
            )?
            .z()
        });
        due.unwrap_or_else(|e| {
            crate::diagnostics::report(format_args!(
                "could not check the notification permission: {e}"
            ));
            false
        })
    }

    /// 説明への答え。どちらでも以後は説明を出さず、`allow`ならOSのダイアログを出す。もう求めたあとは
    /// 何もしない。
    pub fn answer_notification_prompt(allow: bool) {
        let answered = with_application(|env, context| {
            let class = app_class(env, context, PERMISSIONS_CLASS)?;
            env.call_static_method(
                &class,
                jni_str!("answerNotificationPrompt"),
                jni_sig!("(Landroid/content/Context;Z)V"),
                &[JValue::from(context), JValue::Bool(allow)],
            )?;
            Ok(())
        });
        if let Err(e) = answered {
            crate::diagnostics::report(format_args!(
                "could not ask for the notification permission: {e}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_codes_fall_back_to_asking() {
        assert_eq!(camera_permission_from(GRANTED), CameraPermission::Granted);
        assert_eq!(camera_permission_from(ASK), CameraPermission::Ask);
        assert_eq!(camera_permission_from(BLOCKED), CameraPermission::Blocked);
        for code in [-1, 3, i32::MAX] {
            assert_eq!(
                camera_permission_from(code),
                CameraPermission::Ask,
                "{code}"
            );
        }
    }
}
