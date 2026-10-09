//! Androidでの証明書の検証の初期化(`architecture/network-secrets.md`「Androidの信頼ルート」)。
//!
//! reqwestの`rustls` featureが使う`rustls-platform-verifier`は、Androidでは検証のたびにJNIで
//! Androidの証明書の検証(Kotlinの部品。`gen/android`のGradleの設定で同梱する)を呼ぶ。そのために
//! 要るJVM・Context・ClassLoaderを、ここで先に渡しておく。渡さないままHTTPSで接続すると、
//! クライアントの組み立ては通り、検証の時点でpanicする。そうならないよう、渡すまでは
//! [`super::hardened_client`]がHTTPSの通信先を断る。

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use jni::objects::JObject;
use jni::{jni_sig, jni_str, JavaVM};

static INITIALIZED: AtomicBool = AtomicBool::new(false);

/// [`init`]を済ませたか。
pub fn initialized() -> bool {
    INITIALIZED.load(Ordering::Acquire)
}

/// 証明書の検証に要るJNIの参照を`rustls-platform-verifier`へ渡す。2回目からは何もしない。
///
/// # Safety
///
/// `java_vm`はこのプロセスのJavaVMでなければならない。`activity`は、呼び出したスレッドで有効な
/// Activityの参照(JNIの`jobject`。ローカル参照でもグローバル参照でもよい)か、nullでなければならない。
pub unsafe fn init(java_vm: *mut c_void, activity: *mut c_void) -> Result<(), String> {
    if activity.is_null() {
        return Err("could not initialize the certificate verifier: no activity".to_string());
    }
    // SAFETY: 呼び出し側が保証する。
    let vm = unsafe { JavaVM::from_raw(java_vm.cast()) };
    vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        // SAFETY: 呼び出し側が保証する。`JObject`はDropで参照を消さないので、持ち主は呼び出し側のまま。
        let activity = unsafe { JObject::from_raw(env, activity.cast()) };
        // Activityは作り直されることがあるので、プロセスと同じ寿命のApplicationのContextを持たせる。
        let context = env
            .call_method(
                &activity,
                jni_str!("getApplicationContext"),
                jni_sig!("()Landroid/content/Context;"),
                &[],
            )?
            .l()?;
        rustls_platform_verifier::android::init_with_env(env, context)
    })
    .map_err(|e| format!("could not initialize the certificate verifier: {e}"))?;
    INITIALIZED.store(true, Ordering::Release);
    Ok(())
}
