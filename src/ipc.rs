//! Client <-> server wire protocol over a Windows named pipe.
//!
//! Framing: `u32` little-endian payload length followed by a bincode payload.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_FRAME: u32 = 64 * 1024 * 1024;

/// Name of the per-user named pipe the server listens on. Both the user name
/// and the socket name are reduced to `[A-Za-z0-9_.-]` so a `-L` value can
/// never escape the `wmux-<user>-` namespace (pipe names accept `\`).
pub fn pipe_name(socket_name: &str) -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    let clean = |s: &str| -> String {
        s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' { c } else { '_' }).collect()
    };
    let (user, socket) = (clean(&user), clean(socket_name));
    let socket = if socket.is_empty() { "default".to_string() } else { socket };
    format!(r"\\.\pipe\wmux-{user}-{socket}")
}

/// Raw Windows `KEY_EVENT_RECORD`, forwarded verbatim so the server can hand it
/// to ConPTY in win32-input-mode without loss of fidelity.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyRecord {
    pub down: bool,
    pub repeat: u16,
    pub vk: u16,
    pub sc: u16,
    pub ch: u16,
    pub ctrl: u32,
}

/// Raw Windows `MOUSE_EVENT_RECORD`.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseRecord {
    pub x: i16,
    pub y: i16,
    pub buttons: u32,
    pub ctrl: u32,
    pub flags: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ClientMsg {
    /// Run a command (the argv of the `wmux` CLI, or a `:` prompt line).
    /// If the command attaches, the server answers `Attached` and the stream
    /// switches into interactive mode.
    Command {
        version: u32,
        argv: Vec<String>,
        cwd: String,
        cols: u16,
        rows: u16,
        /// Client is running on a real console and may be attached.
        interactive: bool,
        /// Value of `WMUX_PANE` in the client's environment (running inside a pane).
        pane_env: Option<u32>,
    },
    Key(KeyRecord),
    Mouse(MouseRecord),
    Resize {
        cols: u16,
        rows: u16,
    },
    Detach,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ServerMsg {
    /// The client is now attached to `session`; raw terminal output follows.
    Attached {
        session: String,
    },
    /// Raw VT bytes for the client console.
    Output(Vec<u8>),
    /// Plain text for a non-interactive command (list-sessions, ...).
    Text(String),
    /// A non-interactive command finished.
    Done {
        code: i32,
    },
    /// The attached client was detached (prefix-d, session killed, ...).
    Detached {
        reason: String,
    },
    Error(String),
    /// Whether the console should capture mouse events for wmux (`mouse`
    /// option). When false the host terminal keeps its native selection.
    SetMouse(bool),
    /// Bring the client's terminal window to the front (`focus-pane`, the
    /// "Go to pane" button of a notification).
    Raise,
}

pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize,
{
    let payload = bincode::serialize(msg).context("serialize frame")?;
    let len = u32::try_from(payload.len()).context("frame too large")?;
    let mut buf = Vec::with_capacity(4 + payload.len());
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(&payload);
    w.write_all(&buf).await.context("write frame")?;
    Ok(())
}

pub async fn read_frame<R, T>(r: &mut R) -> Result<Option<T>>
where
    R: AsyncReadExt + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut hdr = [0u8; 4];
    match r.read_exact(&mut hdr).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) if e.raw_os_error() == Some(109) => return Ok(None), // ERROR_BROKEN_PIPE
        Err(e) => return Err(e).context("read frame header"),
    }
    let len = u32::from_le_bytes(hdr);
    if len > MAX_FRAME {
        bail!("frame too large: {len}");
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await.context("read frame body")?;
    let msg = bincode::deserialize(&payload).context("deserialize frame")?;
    Ok(Some(msg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let msg = ClientMsg::Key(KeyRecord { down: true, repeat: 1, vk: 0x42, sc: 48, ch: 2, ctrl: 8 });
        write_frame(&mut a, &msg).await.unwrap();
        let got: ClientMsg = read_frame(&mut b).await.unwrap().unwrap();
        match got {
            ClientMsg::Key(k) => assert_eq!(k.vk, 0x42),
            _ => panic!("wrong message"),
        }
        drop(a);
        let eof: Option<ClientMsg> = read_frame(&mut b).await.unwrap();
        assert!(eof.is_none());
    }

    #[test]
    fn pipe_name_is_sanitized() {
        let n = pipe_name("default");
        assert!(n.starts_with(r"\\.\pipe\wmux-"));
        assert!(n.ends_with("-default"));
        assert!(pipe_name(r"..\evil").ends_with("-.._evil"));
        assert!(pipe_name("").ends_with("-default"));
        assert!(pipe_name("a b/c").ends_with("-a_b_c"));
    }
}
