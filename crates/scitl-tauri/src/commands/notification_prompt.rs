use super::CommandResult;

/// 通知の許可を求める前の説明を出すか([`scitl_core::permissions::notification_prompt_due`])。
/// 画面が起動時に問い合わせる。JNIと`SharedPreferences`の読み出しで起動を止めないよう、
/// メインスレッドでは呼ばない。
#[tauri::command]
pub async fn notification_prompt_due() -> CommandResult<bool> {
    Ok(
        scitl_core::blocking::run(|| Ok(scitl_core::permissions::notification_prompt_due()))
            .await?,
    )
}

/// 説明への答え([`scitl_core::permissions::answer_notification_prompt`])。
#[tauri::command]
pub fn answer_notification_prompt(allow: bool) {
    scitl_core::permissions::answer_notification_prompt(allow);
}
