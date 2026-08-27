//! TCP 线帧协议。长度前缀 (4B big-endian) + JSON payload。PRD §16 内网 TCP。

use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{ClusterError, ClusterResult};

/// 帧类型: hello 握手 / sync 请求 / sync 响应 / ping 心跳 / pong。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Hello,
    SyncRequest,
    SyncResponse,
    Ping,
    Pong,
}

/// 握手包: follower 告知本地 last_seq + 共享 secret token (H3 鉴权)。
/// token 经 env FUSION_MEMORY_CLUSTER_TOKEN 下发, leader 校验不一致 → 拒连接。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    pub follower_last_seq: i64,
    #[serde(default)]
    pub token: String,
}

/// follower → leader 拉增量: 自 since_seq 之后。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SyncRequest {
    pub since_seq: i64,
    pub limit: usize,
}

/// leader → follower: wop 条目 + 当前 leader 最大 seq。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SyncResponse {
    pub entries: Vec<fm_persist::WopEntry>,
    pub leader_last_seq: i64,
}

/// 统一帧封装 (kind + json payload)。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Frame {
    pub kind: FrameKind,
    pub payload: String,
}

impl Frame {
    pub fn new(kind: FrameKind, payload: impl serde::Serialize) -> ClusterResult<Self> {
        Ok(Self {
            kind,
            payload: serde_json::to_string(&payload)?,
        })
    }

    pub fn decode_payload<T: serde::de::DeserializeOwned>(&self) -> ClusterResult<T> {
        Ok(serde_json::from_str(&self.payload)?)
    }
}

const LEN_BYTES: usize = 4;
/// 帧长度上限 (16MB)。防 H3: 4B 长度前缀无上限 → len=0xFFFFFFFF 触发 4GB 分配 OOM。
const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// 写一帧: 4B 长度 + JSON。
pub async fn write_frame(stream: &mut TcpStream, frame: &Frame) -> ClusterResult<()> {
    let buf = serde_json::to_vec(frame)?;
    let len = (buf.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&buf).await?;
    stream.flush().await?;
    Ok(())
}

/// 读一帧: 先 4B 长度，再读 payload。EOF → None。len 超 MAX_FRAME_LEN → 错误 (防 OOM)。
pub async fn read_frame(stream: &mut TcpStream) -> ClusterResult<Option<Frame>> {
    let mut len_buf = [0u8; LEN_BYTES];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None);
    }
    if len > MAX_FRAME_LEN {
        return Err(ClusterError::Transport(format!(
            "frame len {len} exceeds max {MAX_FRAME_LEN} (possible DoS)"
        )));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let frame: Frame = serde_json::from_slice(&buf)?;
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let resp = SyncResponse {
            entries: vec![fm_persist::WopEntry {
                seq: 1,
                op: "commit".into(),
                payload: "{}".into(),
                at: 100,
            }],
            leader_last_seq: 1,
        };
        let frame = Frame::new(FrameKind::SyncResponse, resp).unwrap();
        assert_eq!(frame.kind, FrameKind::SyncResponse);
        let back: SyncResponse = frame.decode_payload().unwrap();
        assert_eq!(back.leader_last_seq, 1);
        assert_eq!(back.entries.len(), 1);
    }

    #[tokio::test]
    async fn write_read_frame_over_tcp() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let frame = read_frame(&mut sock).await.unwrap().unwrap();
            write_frame(&mut sock, &Frame::new(FrameKind::Pong, "ok").unwrap())
                .await
                .unwrap();
            frame
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        write_frame(&mut client, &Frame::new(FrameKind::Ping, "hi").unwrap())
            .await
            .unwrap();
        let pong = read_frame(&mut client).await.unwrap().unwrap();
        let got = server.await.unwrap();
        assert_eq!(got.kind, FrameKind::Ping);
        assert_eq!(pong.kind, FrameKind::Pong);
    }

    #[tokio::test]
    async fn read_frame_zero_len_returns_none() {
        // 覆盖 len==0 → None 分支。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // 写 4B 长度 = 0
            sock.write_all(&[0u8, 0, 0, 0]).await.unwrap();
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let f = read_frame(&mut client).await.unwrap();
        assert!(f.is_none());
    }

    #[tokio::test]
    async fn read_frame_truncated_payload_errors() {
        // 覆盖 read_exact payload 读不全 → 错误 (非 EOF len 前缀)。
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // 声明 len=10 但只写 3B → read_exact payload 失败
            sock.write_all(&(10u32).to_be_bytes()).await.unwrap();
            sock.write_all(b"abc").await.unwrap();
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let res = read_frame(&mut client).await;
        assert!(res.is_err());
    }
}
