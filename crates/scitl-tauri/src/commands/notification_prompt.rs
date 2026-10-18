/// 通知の許可を求める前の説明を出すか([`scitl_core::permissions::notification_prompt_due`])。
/// 画面が起動時に問い合わせる。
#[tauri::command]
pub fn notification_prompt_due() -> bool {
    scitl_core::permissions::notification_prompt_due()
}

/// 説明への答え([`scitl_core::permissions::answer_notification_prompt`])。
#[tauri::command]
pub fn answer_notification_prompt(allow: bool) {
    scitl_core::permissions::answer_notification_prompt(allow);
}
