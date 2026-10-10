//! Androidで、応答の生成中だけプロセスをフォアグラウンドサービスにする
//! (`architecture/concurrency.md`「Androidで裏へ回ったとき」)。
//!
//! サービスの本体は`gen/android`のKotlin(`GeneratingService`)にあり、ここからはIntentで始める・
//! 止めるだけを頼む。裏でActivityが無くなっていても止められるよう、WebViewを通さず、
//! [`crate::secrets::android`]が`ndk-context`へ渡したApplicationのContextから頼む。

/// サービスのクラスの名前(パッケージは[`crate::APP_IDENTIFIER`])。マニフェストの宣言と揃える。
/// マニフェストはRustの定数を参照できないので、scitl-tauriのテストが照合する。
pub const SERVICE_CLASS: &str = "GeneratingService";

/// 通知の文面をサービスへ渡すIntentのextraの名前。Kotlin側の定数と揃える(照合は[`SERVICE_CLASS`]と同じ)。
pub const EXTRA_TITLE: &str = "title";
pub const EXTRA_CHANNEL: &str = "channel";

/// 通知の文面。
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
struct Notice {
    title: &'static str,
    /// OSの設定に出る、通知のチャンネルの名前。
    channel: &'static str,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn notice(lang: crate::i18n::Language) -> Notice {
    Notice {
        title: crate::i18n::text(lang, "generating_notice.title"),
        channel: crate::i18n::text(lang, "generating_notice.channel"),
    }
}

#[cfg(target_os = "android")]
pub use android::set_running;

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str, Env, JavaVM};

    use super::{notice, EXTRA_CHANNEL, EXTRA_TITLE, SERVICE_CLASS};
    use crate::i18n::Language;

    /// サービスを始める(`running`が`true`)・止める。通知の文面は`lang`で引いて渡す。
    ///
    /// 頼めなくても応答の生成は続けるので、失敗は診断に出すだけにする(裏へ回ると止まりうる、
    /// サービスが無いときと同じ状態になる)。アプリが前に出ていないときに始めようとすると、
    /// OSが断る。
    pub fn set_running(running: bool, lang: Language) {
        if let Err(e) = request(running, lang) {
            let action = if running { "start" } else { "stop" };
            crate::diagnostics::report(format_args!(
                "could not {action} the foreground service: {e}"
            ));
        }
    }

    fn request(running: bool, lang: Language) -> Result<(), String> {
        // 渡す前に`ndk_context::android_context`を呼ぶとpanicする。済んだ印は秘密情報の保存先が
        // 持っている(`network-secrets.md`「Androidの保存先」)。
        if !crate::secrets::android::initialized() {
            return Err("no application context yet".to_string());
        }
        let android = ndk_context::android_context();
        // SAFETY: `secrets::android::init`が渡した、このプロセスのJavaVM。
        let vm = unsafe { JavaVM::from_raw(android.vm().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            // SAFETY: `secrets::android::init`が渡した、消さないグローバル参照。`JObject`はDropで
            // 参照を消さない。
            let context = unsafe { JObject::from_raw(env, android.context().cast()) };
            let intent = service_intent(env, &context)?;
            if running {
                let notice = notice(lang);
                put_extra(env, &intent, EXTRA_TITLE, notice.title)?;
                put_extra(env, &intent, EXTRA_CHANNEL, notice.channel)?;
                // 前に出ている間に始めるので`startForegroundService`は使わない。あちらは、サービスが
                // 通知を出す前に止めるとアプリごと落とされる。
                env.call_method(
                    &context,
                    jni_str!("startService"),
                    jni_sig!("(Landroid/content/Intent;)Landroid/content/ComponentName;"),
                    &[JValue::from(&intent)],
                )?;
            } else {
                env.call_method(
                    &context,
                    jni_str!("stopService"),
                    jni_sig!("(Landroid/content/Intent;)Z"),
                    &[JValue::from(&intent)],
                )?;
            }
            Ok(())
        })
        .map_err(|e| e.to_string())
    }

    /// サービスを名前で指すIntent。
    fn service_intent<'local>(
        env: &mut Env<'local>,
        context: &JObject,
    ) -> jni::errors::Result<JObject<'local>> {
        let class = env.new_string(format!("{}.{SERVICE_CLASS}", crate::APP_IDENTIFIER))?;
        let intent = env.new_object(jni_str!("android/content/Intent"), jni_sig!("()V"), &[])?;
        env.call_method(
            &intent,
            jni_str!("setClassName"),
            jni_sig!("(Landroid/content/Context;Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::from(context), JValue::from(&class)],
        )?;
        Ok(intent)
    }

    fn put_extra(
        env: &mut Env,
        intent: &JObject,
        name: &str,
        value: &str,
    ) -> jni::errors::Result<()> {
        let name = env.new_string(name)?;
        let value = env.new_string(value)?;
        env.call_method(
            intent,
            jni_str!("putExtra"),
            jni_sig!("(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;"),
            &[JValue::from(&name), JValue::from(&value)],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Language;

    /// 文面を引くキーはAndroidでしか使わないので、どの言語でも引けることをここで確かめる
    /// (引けないとキーそのものが通知に出る)。
    #[test]
    fn the_notice_has_text_in_every_language() {
        for lang in Language::ALL {
            let notice = notice(lang);
            assert!(!notice.title.starts_with("generating_notice."), "{lang:?}");
            assert!(
                !notice.channel.starts_with("generating_notice."),
                "{lang:?}"
            );
        }
    }
}
