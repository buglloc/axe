//! AXE relay protocol and reconnecting client, shared by the `axe-relay`
//! server and `axe sshd`. Server-only code lives in the binary behind the
//! default `server` feature.

pub mod client;
pub mod protocol;

pub use protocol::Transport;
