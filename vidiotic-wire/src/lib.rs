//! `vidiotic-wire`: the scriptable-IPC protocol for `vidiotic`.
//!
//! A dependency-free (nanoserde-only) leaf crate defining the JSON-lines wire
//! vocabulary — [`WireCommand`], [`WireQuery`], their reply views, and the
//! request/reply envelope ([`Request`] / [`Reply`], see [`envelope`] for the
//! documented JSON shapes). `vidiotic` depends on this crate and owns the
//! `WireCommand -> Command` translation; sibling tools (`vidiotic-ctl`,
//! `vidiotic-prep`) can depend on it as clients without pulling in `vidiotic`.
//!
//! This crate must never depend on `vidiotic` — it is the leaf that breaks the
//! dependency cycle (`vidiotic -> vidiotic-ctl` already forbids ctl importing
//! `vidiotic`, and ctl is a target client of this protocol).
//!
//! Every serialized type is owned and monomorphic: `String` where the engine
//! has `Arc<str>`/`PathBuf`, `u64` where it has `usize`, concrete
//! [`WireToggleI32`]/[`WireToggleF64`]/[`WireToggleU32`] where it has the
//! generic `Toggle<T>`.

pub mod command;
#[cfg(feature = "client")]
pub mod client;
pub mod envelope;
pub mod isf;
pub mod query;
pub mod reply;

/// The protocol version this crate speaks; carried in the greeting and in
/// [`WireStatus`]. Bump on any wire-visible breaking change.
pub const WIRE_VERSION: u32 = 1;

/// Environment variable naming the engine socket, set on processes the engine
/// launches itself (`vidiotic-prep`). Its presence is a child's signal that it
/// was spawned by a live engine and may talk back to it.
///
/// It sits with the protocol rather than with the engine's listener because
/// finding the socket is the first step of speaking this protocol — a client
/// that reads it needs nothing else from `vidiotic`.
pub const SOCK_ENV: &str = "VIDIOTIC_SOCK";

#[cfg(feature = "client")]
pub use client::{ClientError, WireClient};
pub use command::{
    WireCadence, WireCamDelay, WireChainSlot, WireCommand, WireCueParam, WireCueParamKind,
    WireSlotRef, WireSyncKind, WireTimeSig, WireToggleF64, WireToggleI32, WireToggleU32,
};
pub use envelope::{Greeting, GreetingInfo, ReplyResult, Reply, ReqBody, Request};
pub use isf::{WireIsfInput, WireIsfInputKind, WireIsfValue, WireParam};
pub use query::WireQuery;
pub use reply::{
    WireAudio, WireBankView, WireCameraEntry, WireClipBankView, WireClipEntry, WireClipRole,
    WireCueView, WireCues, WireLevels, WirePool, WireReply, WireShaderPoolView, WireShaders,
    WireStatus, WireTransport,
};
