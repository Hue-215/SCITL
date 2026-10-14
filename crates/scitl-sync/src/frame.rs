//! 1本のTLS接続の上でやり取りする、長さを前に付けたメッセージ(`docs/sync/index.md` 3節)。
//! 先頭4バイト(ビッグエンディアン)が中身の長さ。相手を確かめる前の中身はバイト列そのまま、
//! 確かめたあとはJSON。

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::SyncError;

/// 1つ読む。長さが`max`を超えれば中身を読まずに断る。`timeout`は読み終えるまで。
pub(crate) async fn read<S: AsyncRead + Unpin>(
    stream: &mut S,
    max: usize,
    timeout: Duration,
) -> Result<Vec<u8>, SyncError> {
    tokio::time::timeout(timeout, async {
        let mut len = [0u8; 4];
        read_exact(stream, &mut len).await?;
        let len = u32::from_be_bytes(len) as usize;
        if len > max {
            return Err(SyncError::FrameTooLarge { len, max });
        }
        let mut body = vec![0u8; len];
        read_exact(stream, &mut body).await?;
        Ok(body)
    })
    .await
    .map_err(|_| SyncError::TimedOut)?
}

async fn read_exact<S: AsyncRead + Unpin>(stream: &mut S, buf: &mut [u8]) -> Result<(), SyncError> {
    match stream.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(SyncError::PeerClosed),
        Err(e) => Err(e.into()),
    }
}

pub(crate) async fn write<S: AsyncWrite + Unpin>(
    stream: &mut S,
    body: &[u8],
    timeout: Duration,
) -> Result<(), SyncError> {
    let len = u32::try_from(body.len()).map_err(|_| SyncError::FrameTooLarge {
        len: body.len(),
        max: u32::MAX as usize,
    })?;
    // 長さと中身を1回で書く(TLSのレコードを2つに分けない)。
    let mut framed = Vec::with_capacity(4 + body.len());
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(body);
    tokio::time::timeout(timeout, async {
        stream.write_all(&framed).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| SyncError::TimedOut)?
    .map_err(SyncError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn round_trip() {
        let (mut a, mut b) = tokio::io::duplex(64);
        write(&mut a, b"hello", T).await.unwrap();
        assert_eq!(read(&mut b, 5, T).await.unwrap(), b"hello");
    }

    #[tokio::test]
    async fn an_oversized_length_is_refused_before_reading_the_body() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
        assert!(matches!(
            read(&mut b, 16, T).await,
            Err(SyncError::FrameTooLarge { len, max: 16 }) if len == u32::MAX as usize
        ));
    }

    #[tokio::test]
    async fn a_stalled_peer_times_out() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&[0, 0]).await.unwrap();
        assert!(matches!(
            read(&mut b, 16, Duration::from_millis(50)).await,
            Err(SyncError::TimedOut)
        ));
    }

    #[tokio::test]
    async fn a_closed_peer_is_told_apart() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        assert!(matches!(
            read(&mut b, 16, T).await,
            Err(SyncError::PeerClosed)
        ));
    }
}
