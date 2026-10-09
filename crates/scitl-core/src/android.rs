//! Androidで、JNIの参照を受け取る部品(証明書の検証の[`crate::net::android`]、秘密情報の保存先の
//! [`crate::secrets::android`])が共通に使う処理。

use jni::objects::JObject;
use jni::{jni_sig, jni_str, Env};

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
