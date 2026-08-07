//! The scriptable-IPC server: a Unix-socket endpoint that lets any client drive
//! everything the control UI can, and read back engine state, over the
//! newline-delimited JSON protocol defined in [`vidiotic_wire`].
//!
//! # Shape
//!
//! One accept thread owns the [`UnixListener`]. Each connection gets a reader
//! thread (socket → engine) and a writer thread (engine → socket); the two
//! never touch the engine directly. All coupling is through channels:
//!
//! - a single **bounded** ingress channel carries [`IngressMsg`]s from every
//!   reader (and the accept thread) to the engine. Bounded so a flooding client
//!   applies backpressure — a full channel blocks its reader, which stops
//!   reading its socket — rather than growing engine memory without limit.
//! - one **bounded** outbox channel per connection carries reply lines to that
//!   connection's writer. If a writer can't keep up and its outbox fills, the
//!   engine drops the connection instead of ever blocking the render loop.
//!
//! The engine side is [`IpcEngine`], owned by `App`. It never does socket I/O:
//! each tick it drains ingress into a bounded work queue ([`IpcEngine::pump`]),
//! the tick applies a capped batch of requests, and after the frame's
//! `UiMirror` is rebuilt it answers the queries it parked — so a client that
//! sends a command then a query reads its own write.
//!
//! This module holds the transport, the `WireCommand → Command` translation
//! ([`to_command`]), the mirror → reply builders ([`build_reply`]), and
//! pre-dispatch validation ([`reject_reason`]). The tick integration (drain,
//! park, answer) lives in `app.rs` because it needs `App`'s private state.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};

use vidiotic_wire::command::{
    WireCadence, WireCamDelay, WireChainSlot, WireCueParam, WireCueParamKind, WireSlotRef,
    WireSyncKind, WireTimeSig, WireToggleF64, WireToggleI32, WireToggleU32,
};
use vidiotic_wire::envelope::{Greeting, Reply, Request};
use vidiotic_wire::isf::{WireIsfInput, WireIsfInputKind, WireIsfValue, WireParam};
use vidiotic_wire::query::WireQuery;
use vidiotic_wire::reply::{
    WireAudio, WireBankView, WireCameraEntry, WireClipBankView, WireClipEntry, WireClipRole,
    WireCueView, WireCues, WireLevels, WirePool, WireReply, WireShaderPoolView, WireShaders,
    WireStatus, WireTransport,
};

use crate::bank::{CamDelay, Toggle};
use crate::commands::{
    Cadence, ChainSlot, ClipEntry, ClipRole, Command, CueParam, CueParamKind, CueView, SlotRef,
    SyncKind, TimeSig, UiMirror,
};
use crate::isf::{IsfInput, IsfInputKind, IsfValue};

/// Re-exported so `crate::ipc::SOCK_ENV` still reads as "the engine's socket
/// variable" at the listener side; the definition lives with the protocol in
/// `vidiotic-wire`, where clients reach it without depending on this crate.
pub use vidiotic_wire::SOCK_ENV;

/// Longest request line accepted, in bytes. A line without a newline inside
/// this bound is a protocol violation and drops the connection — it caps a
/// reader thread's per-line allocation against a client that never sends `\n`.
const MAX_LINE_BYTES: u64 = 1 << 20; // 1 MiB

/// Depth of the shared ingress channel. When full, reader threads block on
/// send — the natural backpressure that keeps a chatty client from growing
/// engine memory.
const INGRESS_CAP: usize = 512;

/// Depth of a per-connection outbox. Overflow means the client isn't draining
/// its socket; the engine drops the connection rather than block.
const OUTBOX_CAP: usize = 256;

/// Ceiling on requests applied per tick, so one client's burst can't monopolize
/// a frame. Excess stays queued for the next tick.
pub const PER_TICK_CAP: usize = 64;

/// A connection's identity, assigned by the accept thread.
pub type ConnId = u64;

/// A message from the socket threads to the engine, all over one bounded
/// channel so the engine drains a single queue.
enum IngressMsg {
    /// A new connection, carrying the sender half of its outbox so the engine
    /// can address replies to it.
    Hello { conn: ConnId, outbox: Sender<String> },
    /// A parsed request line from a connection.
    Line { conn: ConnId, req: Request },
    /// A connection's reader ended (EOF, I/O error, or protocol violation).
    Bye { conn: ConnId },
}

/// The listening endpoint. Binding and the accept loop live here; dropping the
/// returned [`IpcEngine`] unlinks the socket file.
pub struct IpcServer;

impl IpcServer {
    /// Bind a Unix socket at `path` and spawn the accept loop. `epoch` is the
    /// live session generation, read for each connection's greeting. When
    /// `latest` is given, a symlink there is pointed at `path` (atomically, via
    /// a temp name + rename) so a client can always find the newest instance.
    ///
    /// A stale socket file at `path` (from a prior crash) is unlinked first.
    ///
    /// # Errors
    ///
    /// Returns the bind error if the socket can't be created (e.g. the parent
    /// directory is missing or unwritable).
    pub fn spawn(
        path: PathBuf,
        epoch: Arc<AtomicU64>,
        latest: Option<PathBuf>,
    ) -> std::io::Result<IpcEngine> {
        // Unlink a stale socket before binding — a leftover file from a crash
        // would otherwise make bind fail with EADDRINUSE.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        if let Some(latest) = &latest {
            update_latest_symlink(latest, &path);
        }
        let (ingress_tx, ingress_rx) = bounded(INGRESS_CAP);
        thread::Builder::new()
            .name("ipc-accept".into())
            .spawn(move || accept_loop(&listener, &ingress_tx, &epoch))?;
        log::info!("ipc: listening on {}", path.display());
        Ok(IpcEngine {
            ingress: ingress_rx,
            outboxes: HashMap::new(),
            pending: VecDeque::new(),
            parked: Vec::new(),
            socket_path: path,
            latest,
        })
    }
}

/// The engine-side handle to the IPC server. Owned by `App`; drives no I/O of
/// its own — the tick pumps it and hands it the fresh mirror to answer queries.
pub struct IpcEngine {
    ingress: Receiver<IngressMsg>,
    /// Per-connection reply sinks, keyed by [`ConnId`]. Removing an entry tears
    /// the connection down (its writer's channel disconnects).
    outboxes: HashMap<ConnId, Sender<String>>,
    /// Bounded backlog of requests not yet applied. Capped at [`INGRESS_CAP`];
    /// [`Self::pump`] stops pulling when full so ingress backpressure holds.
    pending: VecDeque<(ConnId, Request)>,
    /// Queries deferred to end-of-tick so they read the freshly built mirror.
    parked: Vec<(ConnId, u64, WireQuery)>,
    socket_path: PathBuf,
    /// The `vidiotic-latest.sock` convenience symlink, if this instance owns
    /// one. Removed on drop only while it still points at our socket.
    latest: Option<PathBuf>,
}

impl IpcEngine {
    /// The socket this instance is listening on. Handed to child processes
    /// (see `spawn_project_editor`) so they can drive the engine that launched
    /// them rather than guessing at `vidiotic-latest.sock`.
    #[must_use]
    pub fn socket_path(&self) -> &std::path::Path {
        &self.socket_path
    }

    /// Move newly-arrived ingress messages into the work queues. Non-blocking.
    /// Stops pulling `Line`s once `pending` is full, leaving them in the bounded
    /// ingress channel so readers stay blocked (backpressure).
    pub fn pump(&mut self) {
        while self.pending.len() < INGRESS_CAP {
            match self.ingress.try_recv() {
                Ok(IngressMsg::Hello { conn, outbox }) => {
                    self.outboxes.insert(conn, outbox);
                }
                Ok(IngressMsg::Bye { conn }) => {
                    self.outboxes.remove(&conn);
                }
                Ok(IngressMsg::Line { conn, req }) => {
                    self.pending.push_back((conn, req));
                }
                Err(_) => break,
            }
        }
    }

    /// Take up to `cap` queued requests for this tick to apply.
    pub fn take_requests(&mut self, cap: usize) -> Vec<(ConnId, Request)> {
        let n = cap.min(self.pending.len());
        self.pending.drain(..n).collect()
    }

    /// Defer a query to be answered after the mirror is rebuilt this tick.
    pub fn park(&mut self, conn: ConnId, id: u64, query: WireQuery) {
        self.parked.push((conn, id, query));
    }

    /// Take all parked queries (called after `build_mirror`).
    pub fn take_parked(&mut self) -> Vec<(ConnId, u64, WireQuery)> {
        std::mem::take(&mut self.parked)
    }

    /// Send one reply to a connection. A full or disconnected outbox drops the
    /// connection — the engine never blocks on a slow client.
    pub fn send(&mut self, conn: ConnId, reply: &Reply) {
        if let Some(tx) = self.outboxes.get(&conn) {
            if tx.try_send(reply.to_json_line()).is_err() {
                log::debug!("ipc: dropping conn {conn} (outbox full or closed)");
                self.outboxes.remove(&conn);
            }
        }
    }
}

impl Drop for IpcEngine {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        // Only reclaim the shared symlink if it still points at us — another
        // instance may have taken it over since we started.
        if let Some(latest) = &self.latest {
            if std::fs::read_link(latest).ok().as_deref() == Some(self.socket_path.as_path()) {
                let _ = std::fs::remove_file(latest);
            }
        }
    }
}

/// Point `latest` at `target` atomically: create the symlink under a unique
/// temp name, then rename it over `latest` (rename is atomic, so a concurrent
/// reader sees either the old or the new target, never a missing one).
fn update_latest_symlink(latest: &std::path::Path, target: &std::path::Path) {
    let tmp = latest.with_file_name(format!("vidiotic-latest.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let linked = std::os::unix::fs::symlink(target, &tmp).and_then(|()| std::fs::rename(&tmp, latest));
    if linked.is_err() {
        let _ = std::fs::remove_file(&tmp);
        log::debug!("ipc: could not update {}", latest.display());
    }
}

/// The accept loop: greet each connection, spawn its reader and writer, and
/// register it with the engine.
fn accept_loop(listener: &UnixListener, ingress: &Sender<IngressMsg>, epoch: &Arc<AtomicU64>) {
    let mut next: ConnId = 0;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let conn = next;
        next += 1;
        let (out_tx, out_rx) = bounded::<String>(OUTBOX_CAP);
        // Queue the greeting before the engine can enqueue any reply, so it is
        // always the connection's first line.
        let _ = out_tx.send(Greeting::new(epoch.load(Ordering::Relaxed)).to_json_line());
        let Ok(write_half) = stream.try_clone() else { continue };
        let _ = thread::Builder::new()
            .name(format!("ipc-write-{conn}"))
            .spawn(move || writer_loop(write_half, &out_rx));
        if ingress.send(IngressMsg::Hello { conn, outbox: out_tx }).is_err() {
            break; // engine gone
        }
        let ingress = ingress.clone();
        let _ = thread::Builder::new()
            .name(format!("ipc-read-{conn}"))
            .spawn(move || reader_loop(stream, conn, &ingress));
    }
}

/// Read request lines from a connection and forward parsed ones to the engine.
/// A malformed-but-bounded line is logged and skipped (one typo in an `nc`
/// session doesn't kill the connection); an oversize line or I/O error tears it
/// down. On exit, notify the engine so it can forget the connection.
fn reader_loop(stream: UnixStream, conn: ConnId, ingress: &Sender<IngressMsg>) {
    let mut reader = BufReader::new(stream);
    loop {
        let mut buf = Vec::new();
        let read = (&mut reader).take(MAX_LINE_BYTES).read_until(b'\n', &mut buf);
        match read {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                log::debug!("ipc conn {conn}: read error: {e}");
                break;
            }
        }
        if buf.last() != Some(&b'\n') {
            // Hit the byte cap without a line terminator: a client that won't
            // frame its input. Drop it rather than buffer unboundedly.
            log::warn!("ipc conn {conn}: line exceeded {MAX_LINE_BYTES} bytes; dropping");
            break;
        }
        let Ok(text) = std::str::from_utf8(&buf) else {
            log::debug!("ipc conn {conn}: non-utf8 line skipped");
            continue;
        };
        let line = text.trim();
        if line.is_empty() {
            continue;
        }
        match Request::from_json_line(line) {
            Ok(req) => {
                if ingress.send(IngressMsg::Line { conn, req }).is_err() {
                    break; // engine gone
                }
            }
            Err(e) => log::debug!("ipc conn {conn}: bad request skipped: {e}"),
        }
    }
    let _ = ingress.send(IngressMsg::Bye { conn });
}

/// Write reply lines to a connection until its outbox closes (the engine
/// dropped it, or the reader ended) or the socket errors.
fn writer_loop(mut stream: UnixStream, outbox: &Receiver<String>) {
    for line in outbox.iter() {
        if stream.write_all(line.as_bytes()).and_then(|()| stream.write_all(b"\n")).is_err() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Command translation: WireCommand -> Command
// ---------------------------------------------------------------------------

/// Cadence bar counts are multiplied by bar length (`commands.rs`), so an
/// unclamped huge value overflows `u32`. No musical phrase is this long; clamp
/// hostile input here rather than panic in the resolver.
const MAX_CADENCE_BARS: u32 = 4096;

fn to_cadence(c: WireCadence) -> Cadence {
    match c {
        WireCadence::Note(t) => Cadence::Note(t),
        WireCadence::Bars(n) => Cadence::Bars(n.min(MAX_CADENCE_BARS)),
    }
}

fn to_slot(s: WireSlotRef) -> SlotRef {
    match s {
        WireSlotRef::Live => SlotRef::Live,
        WireSlotRef::Builtin(name) => SlotRef::Builtin(Arc::from(name)),
        WireSlotRef::Pinned(id) => SlotRef::Pinned(id),
        WireSlotRef::Isf(path) => SlotRef::Isf(Arc::from(path)),
    }
}

fn to_isf(v: WireIsfValue) -> IsfValue {
    match v {
        WireIsfValue::Float(x) => IsfValue::Float(x),
        WireIsfValue::Bool(b) => IsfValue::Bool(b),
        WireIsfValue::Long(n) => IsfValue::Long(n),
        WireIsfValue::Color(c) => IsfValue::Color(c),
        WireIsfValue::Point2D(p) => IsfValue::Point2D(p),
    }
}

fn to_chain(slot: WireChainSlot) -> ChainSlot {
    ChainSlot {
        shader: to_slot(slot.shader),
        params: slot
            .params
            .into_iter()
            .map(|WireParam { name, value }| (Arc::from(name), to_isf(value)))
            .collect(),
    }
}

fn to_cue_param(p: WireCueParam) -> CueParam {
    match p {
        WireCueParam::Dwell(t) => CueParam::Dwell(t),
        WireCueParam::Loop(t) => CueParam::Loop(t),
        WireCueParam::LoopPhase(WireToggleI32 { on, val }) => CueParam::LoopPhase(Toggle { on, val }),
        WireCueParam::StartNudge(WireToggleF64 { on, val }) => {
            CueParam::StartNudge(Toggle { on, val })
        }
        WireCueParam::TrigDelay(WireToggleU32 { on, val }) => CueParam::TrigDelay(Toggle { on, val }),
        // Source tempo must stay finite and positive; the resolver divides by it.
        WireCueParam::Bpm(b) => CueParam::Bpm(b.filter(|v| v.is_finite() && *v > 0.0)),
        WireCueParam::BpmSync(on) => CueParam::BpmSync(on),
        WireCueParam::SpeedMul(WireToggleF64 { on, val }) => CueParam::SpeedMul(Toggle { on, val }),
        WireCueParam::CamDelay(WireCamDelay { value, beats, quantize }) => {
            CueParam::CamDelay(CamDelay { value, beats, quantize })
        }
    }
}

fn to_cue_param_kind(k: WireCueParamKind) -> CueParamKind {
    match k {
        WireCueParamKind::Dwell => CueParamKind::Dwell,
        WireCueParamKind::Loop => CueParamKind::Loop,
        WireCueParamKind::LoopPhase => CueParamKind::LoopPhase,
        WireCueParamKind::StartNudge => CueParamKind::StartNudge,
        WireCueParamKind::TrigDelay => CueParamKind::TrigDelay,
        WireCueParamKind::Bpm => CueParamKind::Bpm,
        WireCueParamKind::BpmSync => CueParamKind::BpmSync,
        WireCueParamKind::SpeedMul => CueParamKind::SpeedMul,
    }
}

/// Translate a wire command into the engine's `Command`. Total and infallible:
/// paths become `PathBuf`, names become `Arc<str>`, `usize` indices widen from
/// `u64`, and a few payloads are clamped to the engine's safe domain
/// (cadence bar counts, per-cue BPM). Semantic rejection (unknown ids, out-of-
/// range indices) is [`reject_reason`]'s job, done against live state.
#[must_use]
pub fn to_command(w: vidiotic_wire::command::WireCommand) -> Command {
    use vidiotic_wire::command::WireCommand as W;
    match w {
        W::SetBpm(b) => Command::SetBpm(b),
        W::BpmDelta(d) => Command::BpmDelta(d),
        W::NudgeBpm(r) => Command::NudgeBpm(r),
        W::TapDownbeat => Command::TapDownbeat,
        W::TapTempo => Command::TapTempo,
        W::SoftReset => Command::SoftReset,
        W::HardReset => Command::HardReset,
        W::SetSyncSource(k) => Command::SetSyncSource(match k {
            WireSyncKind::Internal => SyncKind::Internal,
            WireSyncKind::Link => SyncKind::Link,
        }),
        W::SetTimeSig(WireTimeSig { num, den }) => Command::SetTimeSig(TimeSig { num, den }),
        W::SetPhraseCadence(c) => Command::SetPhraseCadence(to_cadence(c)),
        W::SetLoopCadence(c) => Command::SetLoopCadence(c.map(to_cadence)),
        W::SetPreservePlayhead(on) => Command::SetPreservePlayhead(on),
        W::ToggleClipActive(id) => Command::ToggleClipActive(id),
        W::SelectClip(id) => Command::SelectClip(id),
        W::SelectClipDelta(d) => Command::SelectClipDelta(d),
        W::SelectClipFirst => Command::SelectClipFirst,
        W::SelectClipLast => Command::SelectClipLast,
        W::AddCue(clip) => Command::AddCue(clip),
        W::RemoveCue(id) => Command::RemoveCue(id),
        W::SelectCue(id) => Command::SelectCue(id),
        W::SelectCueDelta(d) => Command::SelectCueDelta(d),
        W::SelectCueFirst => Command::SelectCueFirst,
        W::SelectCueLast => Command::SelectCueLast,
        W::SetCueIn(id, s) => Command::SetCueIn(id, s),
        W::SetCueOut(id, s) => Command::SetCueOut(id, s),
        W::SetCueInToPlayhead(id) => Command::SetCueInToPlayhead(id),
        W::SetCueOutToPlayhead(id) => Command::SetCueOutToPlayhead(id),
        W::SetCuePreserve(id, p) => Command::SetCuePreserve(id, p),
        W::SetCueChain(id, chain) => {
            Command::SetCueChain(id, chain.into_iter().map(to_chain).collect())
        }
        W::SetChainParam { cue, slot, name, value } => Command::SetChainParam {
            cue,
            slot: slot as usize,
            name: Arc::from(name),
            value: to_isf(value),
        },
        W::LoadIsf(path) => Command::LoadIsf(PathBuf::from(path)),
        W::SetCueParam(id, p) => Command::SetCueParam(id, to_cue_param(p)),
        W::NudgeCueParam(k, n) => Command::NudgeCueParam(to_cue_param_kind(k), n),
        W::MoveCue(id, idx) => Command::MoveCue(id, idx as usize),
        W::SetClipBpm(id, b) => Command::SetClipBpm(id, b),
        W::SetAdvancedMode(on) => Command::SetAdvancedMode(on),
        W::SetGrammarMode(on) => Command::SetGrammarMode(on),
        W::AddBank => Command::AddBank,
        W::CloneBank => Command::CloneBank,
        W::SetLiveBank(i) => Command::SetLiveBank(i as usize),
        W::CycleLiveBank(d) => Command::CycleLiveBank(d),
        W::SetEditBank(i) => Command::SetEditBank(i as usize),
        W::CaptureShader => Command::CaptureShader,
        W::RemoveShader(id) => Command::RemoveShader(id),
        W::SetClipDir(dir) => Command::SetClipDir(PathBuf::from(dir)),
        W::AddClipDirAsBank(dir) => Command::AddClipDirAsBank(PathBuf::from(dir)),
        W::SetActiveClipBank(i) => Command::SetActiveClipBank(i as usize),
        W::RefreshCameras => Command::RefreshCameras,
        W::SetCameraOnAir(uid, on) => Command::SetCameraOnAir(Arc::from(uid), on),
        W::AddCameraCue(uid) => Command::AddCameraCue(Arc::from(uid)),
        W::RelinkCamera { from, to } => {
            Command::RelinkCamera { from: Arc::from(from), to: Arc::from(to) }
        }
        W::SetShaderPath(path) => Command::SetShaderPath(PathBuf::from(path)),
        W::SetAudioDevice(d) => Command::SetAudioDevice(d),
        W::ToggleFullscreen => Command::ToggleFullscreen,
        W::SaveProject => Command::SaveProject,
        W::SaveProjectTo(path) => Command::SaveProjectTo(PathBuf::from(path)),
        W::LoadProject(path) => Command::LoadProject(PathBuf::from(path)),
        W::OpenProjectEditor => Command::OpenProjectEditor,
        W::OpenControlMapper => Command::OpenControlMapper,
        W::Quit => Command::Quit,
    }
}

// ---------------------------------------------------------------------------
// Pre-dispatch validation
// ---------------------------------------------------------------------------

/// Reject a command that would silently no-op or misfire, against the current
/// mirror. Returns `Some(reason)` to answer `err` instead of dispatching.
///
/// This covers the cases the engine handles by ignoring (unknown cue id,
/// out-of-range bank/shader index, pathless save) — the ones that would
/// otherwise ack as if they worked. Clip-id validity is intentionally *not*
/// checked: the mirror only holds the active clip bank, so a valid cross-bank
/// id would be wrongly rejected; those fall through and no-op if truly unknown.
#[must_use]
pub fn reject_reason(cmd: &Command, m: &UiMirror) -> Option<String> {
    let cue_exists = |id| m.cues.iter().any(|c| c.id == id);
    match cmd {
        Command::RemoveCue(id)
        | Command::SetCueIn(id, _)
        | Command::SetCueOut(id, _)
        | Command::SetCueInToPlayhead(id)
        | Command::SetCueOutToPlayhead(id)
        | Command::SetCuePreserve(id, _)
        | Command::SetCueChain(id, _)
        | Command::SetCueParam(id, _) => {
            (!cue_exists(*id)).then(|| format!("unknown cue {id}"))
        }
        Command::SetChainParam { cue, slot, .. } => {
            if !cue_exists(*cue) {
                return Some(format!("unknown cue {cue}"));
            }
            let len = m.cues.iter().find(|c| c.id == *cue).map_or(0, |c| c.chain.len());
            (*slot >= len).then(|| format!("chain slot {slot} out of range (len {len})"))
        }
        Command::MoveCue(id, idx) => {
            if !cue_exists(*id) {
                return Some(format!("unknown cue {id}"));
            }
            (*idx > m.cues.len()).then(|| format!("cue index {idx} out of range (len {})", m.cues.len()))
        }
        Command::SetLiveBank(i) | Command::SetEditBank(i) => {
            (*i >= m.banks.len()).then(|| format!("bank index {i} out of range (len {})", m.banks.len()))
        }
        Command::SetActiveClipBank(i) => (*i >= m.clip_banks.len())
            .then(|| format!("clip-bank index {i} out of range (len {})", m.clip_banks.len())),
        Command::RemoveShader(id) => (!m.shader_pool.iter().any(|s| s.id == *id))
            .then(|| format!("unknown shader {id}")),
        Command::SaveProject | Command::OpenProjectEditor => m
            .project_path
            .is_none()
            .then(|| "no project path; use SaveProjectTo with an explicit path".to_string()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Reply builders: UiMirror -> WireReply
// ---------------------------------------------------------------------------

fn w_role(r: ClipRole) -> WireClipRole {
    match r {
        ClipRole::None => WireClipRole::None,
        ClipRole::Playing => WireClipRole::Playing,
        ClipRole::Armed => WireClipRole::Armed,
    }
}

fn w_sync(k: SyncKind) -> WireSyncKind {
    match k {
        SyncKind::Internal => WireSyncKind::Internal,
        SyncKind::Link => WireSyncKind::Link,
    }
}

fn w_time_sig(t: TimeSig) -> WireTimeSig {
    WireTimeSig { num: t.num, den: t.den }
}

fn w_cadence(c: Cadence) -> WireCadence {
    match c {
        Cadence::Note(t) => WireCadence::Note(t),
        Cadence::Bars(n) => WireCadence::Bars(n),
    }
}

fn w_toggle_i32(t: Toggle<i32>) -> WireToggleI32 {
    WireToggleI32 { on: t.on, val: t.val }
}
fn w_toggle_f64(t: Toggle<f64>) -> WireToggleF64 {
    WireToggleF64 { on: t.on, val: t.val }
}
fn w_toggle_u32(t: Toggle<u32>) -> WireToggleU32 {
    WireToggleU32 { on: t.on, val: t.val }
}

fn w_cam_delay(d: CamDelay) -> WireCamDelay {
    WireCamDelay { value: d.value, beats: d.beats, quantize: d.quantize }
}

fn w_isf_value(v: &IsfValue) -> WireIsfValue {
    match v {
        IsfValue::Float(x) => WireIsfValue::Float(*x),
        IsfValue::Bool(b) => WireIsfValue::Bool(*b),
        IsfValue::Long(n) => WireIsfValue::Long(*n),
        IsfValue::Color(c) => WireIsfValue::Color(*c),
        IsfValue::Point2D(p) => WireIsfValue::Point2D(*p),
    }
}

fn w_chain(slot: &ChainSlot) -> WireChainSlot {
    WireChainSlot {
        shader: match &slot.shader {
            SlotRef::Live => WireSlotRef::Live,
            SlotRef::Builtin(name) => WireSlotRef::Builtin(name.to_string()),
            SlotRef::Pinned(id) => WireSlotRef::Pinned(*id),
            SlotRef::Isf(path) => WireSlotRef::Isf(path.to_string()),
        },
        params: slot
            .params
            .iter()
            .map(|(name, value)| WireParam { name: name.to_string(), value: w_isf_value(value) })
            .collect(),
    }
}

fn w_isf_input_kind(k: &IsfInputKind) -> WireIsfInputKind {
    match k {
        IsfInputKind::Float { min, max, default } => {
            WireIsfInputKind::Float { min: *min, max: *max, default: *default }
        }
        IsfInputKind::Bool { default } => WireIsfInputKind::Bool { default: *default },
        IsfInputKind::Long { values, labels, default } => WireIsfInputKind::Long {
            values: values.clone(),
            labels: labels.clone(),
            default: *default,
        },
        IsfInputKind::Color { default } => WireIsfInputKind::Color { default: *default },
        IsfInputKind::Point2D { min, max, default } => {
            WireIsfInputKind::Point2D { min: *min, max: *max, default: *default }
        }
        IsfInputKind::Event => WireIsfInputKind::Event,
        IsfInputKind::Image => WireIsfInputKind::Image,
        IsfInputKind::Audio => WireIsfInputKind::Audio,
        IsfInputKind::AudioFft => WireIsfInputKind::AudioFft,
    }
}

fn w_isf_input(input: &IsfInput) -> WireIsfInput {
    WireIsfInput {
        name: input.name.clone(),
        label: input.label.clone(),
        kind: w_isf_input_kind(&input.kind),
    }
}

fn w_clip(c: &ClipEntry) -> WireClipEntry {
    WireClipEntry {
        id: c.id,
        name: c.name.to_string(),
        active: c.active,
        role: w_role(c.role),
        has_thumb: c.has_thumb,
        bpm: c.bpm,
        bank: c.bank as u64,
        duration_sec: c.duration_sec,
        fps: c.fps,
    }
}

fn w_cue(c: &CueView) -> WireCueView {
    WireCueView {
        id: c.id,
        clip: c.clip,
        name: c.name.to_string(),
        in_sec: c.in_sec,
        out_sec: c.out_sec,
        preserve: c.preserve,
        chain: c.chain.iter().map(w_chain).collect(),
        role: w_role(c.role),
        has_thumb: c.has_thumb,
        dwell: c.dwell,
        loop_len: c.loop_len,
        loop_phase: w_toggle_i32(c.loop_phase),
        start_nudge: w_toggle_f64(c.start_nudge),
        trig_delay: w_toggle_u32(c.trig_delay),
        bpm: c.bpm,
        clip_bpm: c.clip_bpm,
        bpm_sync_on: c.bpm_sync_on,
        speed_mul: w_toggle_f64(c.speed_mul),
        speed: c.speed,
        camera: c.camera,
        delay: w_cam_delay(c.delay),
        delay_eff: c.delay_eff,
    }
}

/// Build the answer to `query` from the freshly rebuilt `mirror`. `epoch` is
/// the current session generation, echoed into the `Status` payload.
#[must_use]
pub fn build_reply(query: &WireQuery, m: &UiMirror, epoch: u64) -> WireReply {
    match query {
        WireQuery::Status => WireReply::Status(WireStatus {
            project_path: m.project_path.clone(),
            epoch,
            wire_version: vidiotic_wire::WIRE_VERSION,
            advanced: m.advanced,
            grammar_on: m.grammar_on,
        }),
        WireQuery::Transport => WireReply::Transport(WireTransport {
            bpm: m.bpm,
            beat: m.beat,
            phase: m.phase,
            quantum: m.quantum,
            time_sig: w_time_sig(m.time_sig),
            phrase_cadence: w_cadence(m.phrase_cadence),
            loop_cadence: m.loop_cadence.map(w_cadence),
            sync: m.sync.map(w_sync),
            peers: m.peers,
            can_set_tempo: m.can_set_tempo,
            can_set_phase: m.can_set_phase,
        }),
        WireQuery::Pool => WireReply::Pool(WirePool {
            clip_banks: m
                .clip_banks
                .iter()
                .map(|b| WireClipBankView { name: b.name.to_string(), clip_count: b.clip_count as u64 })
                .collect(),
            active_clip_bank: m.active_clip_bank as u64,
            clips: m.clips.iter().map(w_clip).collect(),
            selected_clip: m.selected_clip,
            cameras: m
                .cameras
                .iter()
                .map(|c| WireCameraEntry {
                    uid: c.uid.to_string(),
                    name: c.name.to_string(),
                    on_air: c.on_air,
                    status: c.status.to_string(),
                    missing: c.missing,
                    active: c.active,
                    role: w_role(c.role),
                })
                .collect(),
        }),
        WireQuery::Cues => WireReply::Cues(WireCues {
            banks: m
                .banks
                .iter()
                .map(|b| WireBankView { name: b.name.to_string(), cue_count: b.cue_count as u64 })
                .collect(),
            live_bank: m.live_bank as u64,
            edit_bank: m.edit_bank as u64,
            cues: m.cues.iter().map(w_cue).collect(),
            selected_cue: m.selected_cue,
        }),
        WireQuery::Shaders => WireReply::Shaders(WireShaders {
            shaders: m
                .shader_pool
                .iter()
                .map(|s| WireShaderPoolView {
                    id: s.id,
                    name: s.name.to_string(),
                    builtin: s.builtin,
                    inputs: s.inputs.iter().map(w_isf_input).collect(),
                })
                .collect(),
        }),
        WireQuery::Audio => WireReply::Audio(WireAudio {
            devices: m.audio_devices.iter().map(|d| d.to_string()).collect(),
            current: m.current_device.as_ref().map(|d| d.to_string()),
            error: m.audio_error.clone(),
        }),
        WireQuery::Levels => WireReply::Levels(WireLevels {
            levels: m.levels.to_vec(),
            spectrum_linear: m.spectrum_linear.clone(),
            level: m.level,
        }),
    }
}

/// Compile-forcing guard: a new `UiMirror` field breaks this destructure (no
/// `..`), forcing a decision about whether the wire should surface it. Never
/// called at runtime; exercised by a test so it isn't dead code.
#[cfg(test)]
fn mirror_field_guard(m: &UiMirror) {
    let UiMirror {
        project_path: _,
        bpm: _,
        bpm_entry: _,
        beat: _,
        phase: _,
        quantum: _,
        time_sig: _,
        phrase_cadence: _,
        loop_cadence: _,
        bar_in_phrase: _,
        bars_per_phrase: _,
        phrase_beats: _,
        loop_len: _,
        preserve_playhead: _,
        advanced: _,
        grammar_on: _,
        grammar_modal: _,
        grammar_pane: _,
        sync: _,
        peers: _,
        can_set_tempo: _,
        can_set_phase: _,
        audio_devices: _,
        current_device: _,
        audio_error: _,
        shader_name: _,
        shader_error: _,
        clip_dir: _,
        clip_banks: _,
        active_clip_bank: _,
        clips: _,
        selected_clip: _,
        cameras: _,
        banks: _,
        live_bank: _,
        edit_bank: _,
        cues: _,
        selected_cue: _,
        shader_pool: _,
        playhead_sec: _,
        levels: _,
        spectrum_linear: _,
        level: _,
        fullscreen: _,
    } = m;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use vidiotic_wire::command::WireCommand;
    use vidiotic_wire::envelope::{ReplyResult, ReqBody};

    /// Full transport exercise with no engine app: bind a socket, connect a
    /// client, and drive [`IpcEngine`] by hand (pump → park → answer) against a
    /// default mirror. Covers the accept/reader/writer threads, the ingress
    /// channel, the greeting, and a query round-trip end to end.
    #[test]
    fn socket_round_trip_answers_a_query() {
        let path = std::env::temp_dir().join(format!("vidiotic-test-{}.sock", std::process::id()));
        let epoch = Arc::new(AtomicU64::new(7));
        let mut engine = IpcServer::spawn(path.clone(), epoch.clone(), None).expect("bind");

        let client = UnixStream::connect(&path).expect("connect");
        let mut reader = BufReader::new(client.try_clone().unwrap());

        // First line is always the greeting, carrying the live epoch.
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let greeting = Greeting::from_json_line(line.trim()).unwrap();
        assert_eq!(greeting.vidiotic.epoch, 7);
        assert_eq!(greeting.vidiotic.wire, vidiotic_wire::WIRE_VERSION);

        let req = Request { id: 42, epoch: None, req: ReqBody::Get(WireQuery::Status) };
        writeln!(&mut { client.try_clone().unwrap() }, "{}", req.to_json_line()).unwrap();

        // Drive the engine the way the tick does, until the request lands.
        let mirror = UiMirror::default();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut answered = false;
        while Instant::now() < deadline && !answered {
            engine.pump();
            for (conn, r) in engine.take_requests(PER_TICK_CAP) {
                if let ReqBody::Get(q) = r.req {
                    engine.park(conn, r.id, q);
                }
            }
            let e = epoch.load(Ordering::Relaxed);
            for (conn, id, q) in engine.take_parked() {
                let reply = build_reply(&q, &mirror, e);
                engine.send(conn, &Reply { id, epoch: e, result: ReplyResult::Ok(reply) });
                answered = true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(answered, "engine never received the request");

        line.clear();
        reader.read_line(&mut line).unwrap();
        let reply = Reply::from_json_line(line.trim()).unwrap();
        assert_eq!(reply.id, 42);
        assert_eq!(reply.epoch, 7);
        assert!(matches!(reply.result, ReplyResult::Ok(WireReply::Status(_))), "{reply:?}");
    }

    /// Every `WireCommand` translates to *some* `Command` without panicking,
    /// and the drift tripwire below keeps `to_command` exhaustive.
    #[test]
    fn every_wire_command_translates() {
        // Reuse the wire crate's own exhaustive catalog shape: one of each
        // variant. We construct a representative here per category.
        let samples = [
            WireCommand::SetBpm(120.0),
            WireCommand::TapTempo,
            WireCommand::SetPhraseCadence(WireCadence::Bars(u32::MAX)),
            WireCommand::SetCueChain(1, vec![WireChainSlot { shader: WireSlotRef::Live, params: vec![] }]),
            WireCommand::SetChainParam { cue: 1, slot: 3, name: "x".into(), value: WireIsfValue::Float(0.5) },
            WireCommand::SetCueParam(1, WireCueParam::Bpm(Some(f64::INFINITY))),
            WireCommand::RelinkCamera { from: "a".into(), to: "b".into() },
            WireCommand::Quit,
        ];
        for s in samples {
            let _ = to_command(s);
        }
    }

    #[test]
    fn cadence_bars_are_clamped() {
        match to_command(WireCommand::SetPhraseCadence(WireCadence::Bars(u32::MAX))) {
            Command::SetPhraseCadence(Cadence::Bars(n)) => assert_eq!(n, MAX_CADENCE_BARS),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cue_bpm_infinite_is_cleared() {
        match to_command(WireCommand::SetCueParam(1, WireCueParam::Bpm(Some(f64::INFINITY)))) {
            Command::SetCueParam(_, CueParam::Bpm(b)) => assert_eq!(b, None),
            other => panic!("{other:?}"),
        }
    }

    /// Drift tripwire: this exhaustive match forces a compile error when a new
    /// `Command` variant is added, so the wire surface can't silently omit it.
    /// Every variant is classified `Wired` (has a `WireCommand` mapping) or
    /// `ExcludedInteractive` (a UI-only picker with no wire form).
    #[test]
    fn command_surface_is_classified() {
        enum Class {
            Wired,
            ExcludedInteractive,
        }
        fn classify(cmd: &Command) -> Class {
            match cmd {
                // Undo/redo are reserved editor chords, not a wire surface.
                // The Pick* four are the panels' "ask the visitor for a path"
                // requests: they resolve to SetClipDir / AddClipDirAsBank /
                // SetShaderPath / LoadIsf, and *those* are wired. A script
                // driving this over IPC already knows the path it wants, so
                // sending it a file dialog would be the wrong verb.
                // The BPM digit trio is the keyboard's way of typing into the
                // transport's tempo field; a script has `SetBpm` and no reason
                // to spell a number one keystroke at a time.
                Command::OpenProject
                | Command::SaveProjectAs
                | Command::PickClipDir
                | Command::PickClipBankDir
                | Command::PickShader
                | Command::PickIsf
                | Command::Undo
                | Command::Redo
                | Command::BpmDigit(_)
                | Command::BpmCommit
                | Command::BpmClear => Class::ExcludedInteractive,
                Command::SetBpm(_)
                | Command::BpmDelta(_)
                | Command::NudgeBpm(_)
                | Command::TapDownbeat
                | Command::TapTempo
                | Command::SoftReset
                | Command::HardReset
                | Command::SetSyncSource(_)
                | Command::SetTimeSig(_)
                | Command::SetPhraseCadence(_)
                | Command::SetLoopCadence(_)
                | Command::SetPreservePlayhead(_)
                | Command::ToggleClipActive(_)
                | Command::SelectClip(_)
                | Command::SelectClipDelta(_)
                | Command::SelectClipFirst
                | Command::SelectClipLast
                | Command::AddCue(_)
                | Command::RemoveCue(_)
                | Command::SelectCue(_)
                | Command::SelectCueDelta(_)
                | Command::SelectCueFirst
                | Command::SelectCueLast
                | Command::SetCueIn(_, _)
                | Command::SetCueOut(_, _)
                | Command::SetCueInToPlayhead(_)
                | Command::SetCueOutToPlayhead(_)
                | Command::SetCuePreserve(_, _)
                | Command::SetCueChain(_, _)
                | Command::SetChainParam { .. }
                | Command::LoadIsf(_)
                | Command::SetCueParam(_, _)
                | Command::NudgeCueParam(_, _)
                | Command::MoveCue(_, _)
                | Command::SetClipBpm(_, _)
                | Command::SetAdvancedMode(_)
                | Command::SetGrammarMode(_)
                | Command::AddBank
                | Command::CloneBank
                | Command::SetLiveBank(_)
                | Command::CycleLiveBank(_)
                | Command::SetEditBank(_)
                | Command::CaptureShader
                | Command::RemoveShader(_)
                | Command::SetClipDir(_)
                | Command::AddClipDirAsBank(_)
                | Command::SetActiveClipBank(_)
                | Command::RefreshCameras
                | Command::SetCameraOnAir(_, _)
                | Command::AddCameraCue(_)
                | Command::RelinkCamera { .. }
                | Command::SetShaderPath(_)
                | Command::SetAudioDevice(_)
                | Command::ToggleFullscreen
                | Command::SaveProject
                | Command::SaveProjectTo(_)
                | Command::LoadProject(_)
                | Command::OpenProjectEditor
                | Command::OpenControlMapper
                | Command::Quit => Class::Wired,
            }
        }
        let _ = classify;
    }

    #[test]
    fn mirror_guard_covers_default() {
        mirror_field_guard(&UiMirror::default());
    }
}
