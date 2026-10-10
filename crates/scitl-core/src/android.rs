//! Androidで、JNIの参照を受け取る部品(証明書の検証の[`crate::net::android`]、秘密情報の保存先の
//! [`crate::secrets::android`])と、JNIでアプリのKotlinを呼ぶ部品([`crate::foreground_service`]・
//! [`crate::reply_notification`])が共通に使う処理。

use jni::objects::{JClass, JObject, JValue};
use jni::{jni_sig, jni_str, Env, JavaVM};

/// ActivityからApplicationのContextを引く。Activityは作り直されることがあるので、部品にはプロセスと
/// 同じ寿命のApplicationのContextを持たせる。
pub(crate) fn application_context<'local>(
    env: &mut Env<'local>,
    activity: &JObject,
) -> jni::errors::Result<JObject<'local>> {
    env.call_method(
        activity,
        jni_str!("getApplicationContext"),
        jni_sig!("()Landroid/content/Context;"),
        &[],
    )?
    .l()
}

/// ApplicationのContextを渡して`call`を呼ぶ。どのスレッドから呼んでもよい。Javaの例外は捕まえて
/// 消し、`Err`にする。
///
/// Contextは[`crate::secrets::android::init`]が`ndk-context`へ渡したもので、渡す前は`Err`
/// (渡す前に`ndk_context::android_context`を呼ぶとpanicする。済んだ印は秘密情報の保存先が持っている。
/// `network-secrets.md`「Androidの保存先」)。
pub(crate) fn with_application<T>(
    call: impl FnOnce(&mut Env, &JObject) -> jni::errors::Result<T>,
) -> Result<T, String> {
    if !crate::secrets::android::initialized() {
        return Err("no application context yet".to_string());
    }
    let android = ndk_context::android_context();
    // SAFETY: `secrets::android::init`が渡した、このプロセスのJavaVM。
    let vm = unsafe { JavaVM::from_raw(android.vm().cast()) };
    vm.attach_current_thread(|env| -> jni::errors::Result<T> {
        // SAFETY: `secrets::android::init`が渡した、消さないグローバル参照。`JObject`はDropで
        // 参照を消さない。
        let context = unsafe { JObject::from_raw(env, android.context().cast()) };
        call(env, &context)
    })
    .map_err(|e| e.to_string())
}

/// アプリ(`gen/android`)のクラスを引く。Rustのスレッドからの`FindClass`はアプリのクラスを
/// 見つけられないので、Contextのクラスローダーに頼む。`name`はパッケージを除いた名前。
pub(crate) fn app_class<'local>(
    env: &mut Env<'local>,
    context: &JObject,
    name: &str,
) -> jni::errors::Result<JClass<'local>> {
    let loader = env
        .call_method(
            context,
            jni_str!("getClassLoader"),
            jni_sig!("()Ljava/lang/ClassLoader;"),
            &[],
        )?
        .l()?;
    let name = env.new_string(format!("{}.{name}", crate::APP_IDENTIFIER))?;
    let class = env
        .call_method(
            &loader,
            jni_str!("loadClass"),
            jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
            &[JValue::from(&name)],
        )?
        .l()?;
    env.cast_local::<JClass>(class)
}
