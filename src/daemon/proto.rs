//! Frames between the window and the daemon over a unix socket:
//! `[u32 length][u8 kind][payload]`. Kind 0 carries a JSON [`Msg`]; kinds 1
//! and 2 carry terminal bytes as `[u16 id length][id][bytes]`, so output
//! needs no encoding.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Bumped when the frames change; a daemon speaking another one is replaced.
pub const PROTOCOL: u32 = 2;

pub const KIND_MSG: u8 = 0;
/// Client: input for a session. Daemon: a session's output.
pub const KIND_BYTES: u8 = 1;
/// Daemon: earlier output of a session, replayed on attach.
pub const KIND_REPLAY: u8 = 2;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Spec {
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Typed into the shell once it starts, such as the agent's command.
    pub input: Option<String>,
    /// A program on pipes instead of a shell on a pty: the host of an ACP
    /// agent. Its output is not replayed; it tells a window that attaches
    /// what it needs itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub program: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u32,
    pub cell_h: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Fg {
    pub pid: u32,
    pub name: String,
    pub argv: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub shell: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Hook {
    pub task: String,
    pub event: String,
    pub message: Option<String>,
    pub kind: Option<String>,
    /// The file a tool call is about to change.
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub said: Option<String>,
    /// The agent's session id, when it says (newer `ideae --hook`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// A worktree the agent is about to make (`git worktree add`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Which of the task's agents, when not its own (`q-<n>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// A tool call, as `tool:print` ([`crate::agent::HookEvent::call`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// When the daemon heard it, Unix seconds: a window that hears it again
    /// on connect tells whether it came after the task was last looked at.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: u64,
}

impl Hook {
    /// What the latest report is kept by: its task, and its agent there.
    pub fn key(&self) -> String {
        match &self.agent {
            Some(agent) => format!("{}#{agent}", self.task),
            None => self.task.clone(),
        }
    }
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "t")]
pub enum Msg {
    // window → daemon
    Hello { protocol: u32, version: String },
    /// Attach to session `id`, starting it from `spec` when it is not running.
    Open { id: String, spec: Spec, size: Size },
    Resize { id: String, size: Size },
    Kill { id: String },
    /// Ends every session not listed: what the window does not show any more.
    Prune { keep: Vec<String> },
    /// Ends every session and the daemon.
    Shutdown,
    /// Hands every session to a daemon started from `exe`, the build the
    /// window runs, and exits; the programs go on.
    Upgrade { exe: PathBuf },
    // `ideae --hook` → daemon → windows
    Hook(Hook),
    // daemon → window
    Welcome { protocol: u32, version: String, sessions: Vec<String>, hooks: Vec<Hook> },
    /// The answer to an `Upgrade` that did not happen: this daemon goes on
    /// serving its sessions.
    Stays,
    Exited { id: String, code: Option<i32>, signal: Option<String> },
    Foreground { id: String, fg: Option<Fg> },
}

pub fn write_msg(w: &mut impl Write, msg: &Msg) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame(w, KIND_MSG, &body)
}

pub fn write_bytes(w: &mut impl Write, kind: u8, id: &str, bytes: &[u8]) -> io::Result<()> {
    let mut body = Vec::with_capacity(2 + id.len() + bytes.len());
    body.extend_from_slice(&(id.len() as u16).to_be_bytes());
    body.extend_from_slice(id.as_bytes());
    body.extend_from_slice(bytes);
    write_frame(w, kind, &body)
}

pub fn encode_bytes(kind: u8, id: &str, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = write_bytes(&mut out, kind, id, bytes);
    out
}

pub fn encode_msg(msg: &Msg) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = write_msg(&mut out, msg);
    out
}

fn write_frame(w: &mut impl Write, kind: u8, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len() + 1).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(5 + body.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.push(kind);
    frame.extend_from_slice(body);
    w.write_all(&frame)
}

pub enum Frame {
    Msg(Msg),
    Bytes { kind: u8, id: String, bytes: Vec<u8> },
}

pub fn read_frame(r: &mut impl Read) -> io::Result<Frame> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > 64 << 20 {
        return Err(io::Error::other("bad frame length"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let kind = body[0];
    let body = &body[1..];
    if kind == KIND_MSG {
        return serde_json::from_slice(body).map(Frame::Msg).map_err(io::Error::other);
    }
    if body.len() < 2 {
        return Err(io::Error::other("short frame"));
    }
    let id_len = u16::from_be_bytes([body[0], body[1]]) as usize;
    let id = body.get(2..2 + id_len).ok_or_else(|| io::Error::other("short frame"))?;
    Ok(Frame::Bytes {
        kind,
        id: String::from_utf8_lossy(id).into_owned(),
        bytes: body[2 + id_len..].to_vec(),
    })
}

/// One daemon per state: a test run with its own IDEAE_STATE must not
/// attach to (or prune) the terminals of the app in use.
/// What an old daemon hands to its successor about one session; the pty's
/// master descriptor travels beside it, in the same order.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Handover {
    pub id: String,
    pub pid: u32,
    pub size: Size,
    pub fg: Option<Fg>,
    pub replay: Vec<u8>,
    /// A pipe session: two descriptors travel for it, its input then its output.
    #[serde(default)]
    pub pipe: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Handovers {
    pub sessions: Vec<Handover>,
    pub hooks: Vec<Hook>,
}

pub fn socket_path() -> PathBuf {
    let dir = crate::state::runtime_dir();
    match std::env::var_os("IDEAE_STATE") {
        Some(state) => {
            use std::hash::{Hash as _, Hasher as _};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            state.hash(&mut h);
            dir.join(format!("daemon-{:016x}.sock", h.finish()))
        }
        None => dir.join("daemon.sock"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Frame, KIND_BYTES, Msg, encode_bytes, encode_msg, read_frame};

    #[test]
    fn frames_round_trip() {
        let mut wire = encode_msg(&Msg::Kill { id: "a#agent".into() });
        wire.extend(encode_bytes(KIND_BYTES, "a#agent", b"\x1b[1mhi"));
        let mut r = std::io::Cursor::new(wire);
        assert!(matches!(read_frame(&mut r).unwrap(), Frame::Msg(Msg::Kill { id }) if id == "a#agent"));
        match read_frame(&mut r).unwrap() {
            Frame::Bytes { kind, id, bytes } => assert_eq!((kind, id.as_str(), bytes.as_slice()), (KIND_BYTES, "a#agent", &b"\x1b[1mhi"[..])),
            _ => panic!("bytes expected"),
        }
    }
}
