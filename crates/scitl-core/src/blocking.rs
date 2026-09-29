//! 非同期層から同期のブロッキング処理(DB・資格情報ストア・ファイルI/O)を呼ぶ入口。
//! ランタイムのワーカーを止めないよう`spawn_blocking`へ逃がす。

use crate::error::{CoreError, Result};

pub async fn run<F, T>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CoreError::Internal(format!("blocking task panicked: {e}")))?
}
