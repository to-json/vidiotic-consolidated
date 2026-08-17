//! Application shell: two windows (fullscreen output + egui control) on one
//! shared Device/Queue, an engine tick that drains UI commands, runs the
//! sequencer, manages per-clip decoders, and feeds audio/beat uniforms to the
//! output shader.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::monitor::MonitorHandle;
use winit::window::{Fullscreen, Window, WindowId};

use crate::analysis::AudioFrame;
use crate::audio::{self, AudioCapture};
use crate::bank::{Bank, CueId};
use crate::clippool::{self, Clip, ClipBank, Thumbnail};
use crate::clock::{InternalClock, LinkClock};
use crate::commands::{Cadence, ChainSlot, ClipId, Command, SlotRef, SyncKind, TimeSig, UiMirror};
use crate::control_input::ControlInput;
use crate::gfx::Graphics;
use crate::grammar;
use crate::render::{Globals, Renderer};
use crate::sequencer::Sequencer;
use crate::shader::lang_of;
use crate::shaderwatch::ShaderWatcher;
use crate::ui::EguiCtl;
use crate::video::capture;
use crate::video::frame::{DecodedFrame, PixelData};
use vidiotic_ctl::event::EventValue;
use vidiotic_play::engine::Engine;
use vidiotic_wire::envelope::{Reply, ReplyResult, ReqBody};

mod cameras;
mod clips;
mod dispatch;
mod ipc;
mod keys;
mod mirror;
mod project;
mod shaders;
mod sources;
mod transport;
mod windows;

use sources::{Captures, NativeSources};

const SHADER_DEBOUNCE: Duration = Duration::from_millis(75);

/// Launch a sibling front end, detached. [`phosphor::bundle::helper`] resolves
/// both layouts the family ships in: nested helper `.app`s under
/// `Contents/Library` when bundled, plain siblings in the cargo target dir
/// otherwise, falling back to the bare name on `$PATH`.
///
/// When IPC is live, `socket` is passed down as `$VIDIOTIC_SOCK` — that is the
/// whole handshake. The child knows both that an engine launched it and which
/// socket to reach it on, so it can talk back (prep hands the edited project
/// over with a `LoadProject` when it's done).
///
/// The nested executable is launched directly rather than through `open(1)`
/// precisely so this environment is inherited; it still picks up its own
/// bundle identity — icon, name, menu bar — from where the binary sits.
fn spawn_helper(app: &str, bin: &str, args: &[&Path], socket: Option<&Path>) {
    let path = phosphor::bundle::helper(app, bin);
    let mut cmd = std::process::Command::new(&path);
    cmd.args(args);
    if let Some(socket) = socket {
        cmd.env(crate::ipc::SOCK_ENV, socket);
    }
    match cmd.spawn() {
        Ok(_) => log::info!("launched {}", path.display()),
        Err(e) => log::error!("failed to launch {}: {e}", path.display()),
    }
}

/// Launch `vidiotic-prep` on `path` to retrim a project.
fn spawn_project_editor(path: &Path, socket: Option<&Path>) {
    spawn_helper("Vidiotic Prep", "vidiotic-prep", &[path], socket);
}

/// Launch `vidiotic-ctl`, the MIDI/key/gamepad mapper. It edits the map files
/// on disk, so it needs no arguments — but it does get the socket, which is
/// how a future live-reload of the map would reach the engine.
fn spawn_control_mapper(socket: Option<&Path>) {
    spawn_helper("Vidiotic Ctl", "vidiotic-ctl", &[], socket);
}

/// Whether a command edits state a `.viproj` persists (cues, banks, the
/// pool) — the trigger for soliciting a save path from an unsaved session.
/// Transport tweaks (tempo, cadences, live-bank routing) deliberately don't
/// count: dialing in a groove isn't yet a project worth naming.
fn mutates_project(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::AddCue(_)
            | Command::RemoveCue(_)
            | Command::SetCueIn(..)
            | Command::SetCueOut(..)
            | Command::SetCueInToPlayhead(_)
            | Command::SetCueOutToPlayhead(_)
            | Command::SetCuePreserve(..)
            | Command::SetCueChain(..)
            | Command::SetChainParam { .. }
            | Command::LoadIsf(_)
            | Command::SetCueParam(..)
            | Command::NudgeCueParam(..)
            | Command::MoveCue(..)
            | Command::SetClipBpm(..)
            | Command::AddBank
            | Command::CloneBank
            | Command::SetClipDir(_)
            | Command::AddClipDirAsBank(_)
            | Command::AddCameraCue(_)
            | Command::RelinkCamera { .. }
    )
}

/// Everything assembled at startup (CLI args, clip pool, audio plumbing) that
/// `App::new` takes ownership of.
pub struct Boot {
    pub shader_path: PathBuf,
    pub windowed: bool,
    pub monitor: Option<usize>,
    pub bpm: f64,
    pub time_sig: TimeSig,
    pub phrase_cadence: Cadence,
    pub initial_sync: SyncKind,
    pub clips: Vec<Clip>,
    /// Named groupings over `clips`; at least one bank should cover the pool or
    /// the grid shows nothing. The non-project path passes a single bank.
    pub clip_banks: Vec<ClipBank>,
    /// Cue banks to seed the sequencer with; empty ⇒ one default bank "A".
    pub cue_banks: Vec<Bank>,
    pub auto_active: Vec<ClipId>,
    /// Probe metadata (`fps`/`frames`/`duration_sec`/`source`) for loaded clips,
    /// keyed by id. The runtime `Clip` drops these, so they are retained here to
    /// round-trip through a save. Empty for the non-project path.
    pub clip_meta: HashMap<ClipId, crate::project::ClipMeta>,
    /// The `.viproj` this session was loaded from, if any — the default target
    /// for an in-place save. `None` when started from `--clip`/`--clip-dir`.
    pub project_path: Option<PathBuf>,
    /// The session generation, shared with the IPC server so its greeting reads
    /// the live value. Created in `main` so the server (spawned before `App`)
    /// and the engine share one counter.
    pub epoch: Arc<std::sync::atomic::AtomicU64>,
    /// The engine-side IPC handle, present when the socket server is running.
    pub ipc: Option<crate::ipc::IpcEngine>,
    /// Session playback defaults (project load overrides the CLI defaults).
    pub preserve_playhead: bool,
    pub loop_cadence: Option<Cadence>,
    pub advanced: bool,
    pub thumb_rx: Option<Receiver<Thumbnail>>,
    pub audio_out: triple_buffer::Output<AudioFrame>,
    pub audio_capture: AudioCapture,
    pub audio_err_rx: Receiver<cpal::Error>,
    pub audio_ctl_tx: Sender<crate::analysis::AudioCtl>,
    pub host: cpal::Host,
    pub audio_devices: Vec<Arc<str>>,
    pub cmd_tx: Sender<Command>,
    pub cmd_rx: Receiver<Command>,
    /// This session's `.viproj`-embedded control-mapping layer (empty for
    /// the non-project `--clip`/`--clip-dir` path).
    pub controls: vidiotic_ctl::ControlMap,
}

/// The native shell: two windows on one shared Device/Queue, the OS-facing
/// services (audio, capture, IPC, the filesystem), and an [`Engine`] that owns
/// the session itself.
///
/// The division is the point. Everything here names something a browser does
/// not have; everything in `engine` is code `/play` runs unchanged. When a
/// method below reads `self.engine.banks`, that is not a shortcut past an
/// abstraction — the engine's state is deliberately public to its two shells,
/// and the mirror builder is the reason (see the engine's module docs).
pub struct App {
    // GPU / windows
    graphics: Option<Graphics>,
    renderer: Option<Renderer>,
    egui: Option<EguiCtl>,

    /// The session: clock, sequencer, pool, cue banks, grammar, undo.
    engine: Engine,

    // Retained load-time clip probe metadata + the source `.viproj`, so a save can
    // round-trip data the runtime `Clip` doesn't hold and default to writing back
    // where the session was loaded from.
    clip_meta: HashMap<ClipId, crate::project::ClipMeta>,
    // Session generation, bumped whenever `ClipId`/`CueId`s are invalidated
    // wholesale (project load, clip-dir replace). IPC clients stamp requests
    // with the epoch they last saw; a stale stamp means their ids may now point
    // at different content, so the server rejects the request. Shared so the
    // socket server's greeting can read the current value off-thread.
    epoch: Arc<std::sync::atomic::AtomicU64>,
    // The engine-side IPC handle: drained each tick, handed the fresh mirror to
    // answer parked queries. `None` when the socket server isn't running.
    ipc: Option<crate::ipc::IpcEngine>,
    project_path: Option<PathBuf>,
    // An unsaved session solicits a save path on its first structural edit,
    // once — declining the dialog must not nag on every subsequent edit.
    save_path_prompted: bool,
    thumb_rx: Option<Receiver<Thumbnail>>,
    // Camera capture: one persistent service per on-air device, deliberately
    // outside cue-lifetime bookkeeping (`retain_decoders` never touches it).
    // Shared with the source opener, which is what taps it when a cue arms.
    // `camera_devices` is the last enumeration (startup + manual refresh).
    captures: Captures,
    camera_devices: Vec<capture::DeviceInfo>,
    // Wall-clock of the previous engine tick, for delay-slew integration.
    last_tick: Instant,

    // audio
    audio_out: triple_buffer::Output<AudioFrame>,
    audio_capture: AudioCapture,
    audio_err_rx: Receiver<cpal::Error>,
    audio_ctl_tx: Sender<crate::analysis::AudioCtl>,
    host: cpal::Host,
    audio_devices: Vec<Arc<str>>,

    // shader
    shader_path: PathBuf,
    watcher: Option<ShaderWatcher>,
    dirty_at: Option<Instant>,
    // count of shaders pinned into the pool, for naming ("<stem> #N")
    shader_pin_count: u32,

    // ui plumbing
    cmd_tx: Sender<Command>,
    cmd_rx: Receiver<Command>,
    mirror: UiMirror,
    control_input: ControlInput,

    // window/input
    windowed: bool,
    monitor: Option<usize>,
    fullscreen_applied: bool,
    start: Instant,
    modifiers: Modifiers,
    bpm_entry: Option<String>,
    should_quit: bool,
    output_id: Option<WindowId>,
    control_id: Option<WindowId>,
    // While occluded (screen locked/asleep, window covered/minimized), the
    // compositor never hands back a drawable, so `get_current_texture()`
    // returns instantly instead of blocking on vsync. Poll-driving redraw
    // requests in that state spins the render loop at native CPU speed and
    // leaks GPU-side surface resources. Skip drawing entirely while occluded
    // instead.
    output_occluded: bool,
    control_occluded: bool,
}

impl App {
    pub fn new(boot: Boot) -> Self {
        let control_input = ControlInput::new(boot.controls);
        let captures: Captures = Rc::new(RefCell::new(capture::CaptureRegistry::default()));
        let engine = Engine::new(vidiotic_play::engine::Boot {
            bpm: boot.bpm,
            time_sig: boot.time_sig,
            phrase_cadence: boot.phrase_cadence,
            loop_cadence: boot.loop_cadence,
            clips: boot.clips,
            clip_banks: boot.clip_banks,
            cue_banks: boot.cue_banks,
            auto_active: boot.auto_active,
            preserve_playhead: boot.preserve_playhead,
            advanced: boot.advanced,
            opener: Box::new(NativeSources {
                captures: captures.clone(),
            }),
        });
        let mut app = Self {
            graphics: None,
            renderer: None,
            egui: None,
            engine,
            clip_meta: boot.clip_meta,
            epoch: boot.epoch,
            ipc: boot.ipc,
            project_path: boot.project_path,
            save_path_prompted: false,
            thumb_rx: boot.thumb_rx,
            captures,
            camera_devices: capture::enumerate(),
            last_tick: Instant::now(),
            audio_out: boot.audio_out,
            audio_capture: boot.audio_capture,
            audio_err_rx: boot.audio_err_rx,
            audio_ctl_tx: boot.audio_ctl_tx,
            host: boot.host,
            audio_devices: boot.audio_devices,
            shader_path: boot.shader_path,
            watcher: None,
            dirty_at: None,
            shader_pin_count: 0,
            cmd_tx: boot.cmd_tx,
            cmd_rx: boot.cmd_rx,
            mirror: UiMirror::default(),
            control_input,
            windowed: boot.windowed,
            monitor: boot.monitor,
            fullscreen_applied: false,
            start: Instant::now(),
            modifiers: Modifiers::default(),
            bpm_entry: None,
            should_quit: false,
            output_id: None,
            control_id: None,
            output_occluded: false,
            control_occluded: false,
        };
        if boot.initial_sync == SyncKind::Link {
            app.set_sync_source(SyncKind::Link);
        }
        app
    }

    /// Apply a command that originated from a UI surface (control window, keys,
    /// grammar, async pickers). An unsaved session's first structural edit
    /// solicits a save path here — so in-place saves and the project editor have
    /// a destination from the start — then defers to [`Self::dispatch`].
    ///
    /// Commands injected over IPC bypass this wrapper and call `dispatch`
    /// directly: they carry explicit paths and must never pop a file dialog on
    /// the performer's screen with nobody driving.
    fn dispatch_ui(&mut self, cmd: Command) {
        if self.project_path.is_none() && !self.save_path_prompted && mutates_project(&cmd) {
            self.save_path_prompted = true;
            crate::ui::pick_file(self.cmd_tx.clone(), crate::ui::PickKind::SaveProject(None));
        }
        self.dispatch(cmd);
    }

    /// The command choke point: undo bookkeeping, then the engine, then this
    /// shell for whatever the engine handed back.
    ///
    /// Undo lives here rather than in the engine because only a shell knows
    /// where a command came from — a person's edit is undoable, an IPC
    /// injection is applied the same way but through the same stack, and both
    /// go through exactly this function.
    fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::Undo => self.engine.undo_document(),
            Command::Redo => self.engine.redo_document(),
            other => {
                // Snapshot before an undoable edit; drop history after a
                // document boundary (project load / clip-dir replace).
                self.engine.record_undo(&other);
                let boundary = crate::undo::is_history_boundary(&other);
                if let Some(rest) = self.engine.apply_command(other) {
                    self.apply_shell_command(rest);
                }
                if boundary {
                    self.engine.undo.reset();
                }
            }
        }
    }

    /// The GPU half of a tick: upload the current frame, point the renderer at
    /// the playing cue's chain, and refresh the uniforms.
    ///
    /// Split out of [`Self::update`] because it is the one part of that function
    /// with a single subject — the three steps only ever run together, share the
    /// `graphics`/`renderer` pair, and share one reason for being skipped.
    ///
    /// That reason: **occlusion**. When the output window is occluded (screen
    /// locked or asleep, covered, minimized) nothing will ever present these
    /// writes, and `write_texture`/`write_buffer` leak GPU-side staging memory
    /// until a frame is submitted to reclaim it. On a Poll loop that is every
    /// tick, forever.
    fn render_tick(
        &mut self,
        tick: vidiotic_play::engine::Tick,
        snap: &crate::clock::ClockSnapshot,
        audio: &AudioFrame,
    ) {
        // 6b runs even while occluded: it is pure CPU state on the renderer, and
        // skipping it would leave the wrong chain selected when the window comes
        // back.
        if let Some(r) = self.renderer.as_mut() {
            r.set_active_chain(tick.chain);
        }
        if self.output_occluded {
            return;
        }
        let (Some(g), Some(r)) = (self.graphics.as_ref(), self.renderer.as_mut()) else {
            return;
        };

        // 6. Upload. A source-less cue (camera off-air, failed spawn) blanks the
        // output once rather than leaving the previous cue's frame up.
        if tick.blank {
            let black = DecodedFrame {
                pixels: PixelData::Rgba {
                    data: vec![0; 16],
                    stride: 8,
                },
                w: 2,
                h: 2,
                pts_sec: 0.0,
            };
            r.upload_frame(&g.device, &g.queue, &black);
        }
        if let Some(frame) = &tick.frame {
            r.upload_frame(&g.device, &g.queue, frame);
        }

        // 7. Uniforms: the globals buffer and the audio texture.
        let phrase = self.engine.sequencer.phrase_len();
        let mut globals = Globals {
            resolution: [g.output.config.width as f32, g.output.config.height as f32],
            mouse: [0.0, 0.0],
            time: self.start.elapsed().as_secs_f32(),
            lvl: audio.level,
            beat: snap.beat.rem_euclid(16384.0) as f32,
            bar_phase: (snap.phase / snap.quantum) as f32,
            phrase_phase: (snap.beat.rem_euclid(phrase) / phrase) as f32,
            bpm: snap.bpm as f32,
            video_mode: self.engine.video_mode,
            _pad0: 0.0,
            freqs: [[0.0; 4]; 6],
        };
        globals.set_bands(&audio.bands);
        r.update_globals(&g.queue, &globals);
        r.upload_audio(&g.queue, &audio.audio_tex);
    }

    fn update(&mut self, event_loop: &ActiveEventLoop) {
        // 0. MIDI/gamepad input. The grammar claims token presses first;
        // everything else resolves through the mapper. Either way the
        // resulting commands land in cmd_rx, drained by step 1 below in this
        // same tick.
        for ev in self.control_input.collect() {
            if self.engine.grammar_on
                && ev.value == EventValue::Pressed
                && grammar::token_of_source(&ev.source)
                    .is_some_and(|input| self.engine.grammar_step(input))
            {
                continue;
            }
            self.control_input.resolve(&ev, &self.cmd_tx);
        }

        // 1. Commands (from UI + async pickers + keys), then whatever the
        // grammar raised while resolving them. Verbs go through the same
        // dispatch as a click, which is the point of resolving them into
        // commands rather than mutations.
        let cmds: Vec<Command> = self.cmd_rx.try_iter().collect();
        for c in cmds {
            self.dispatch_ui(c);
        }
        while let Some(c) = self.engine.next_pending() {
            self.dispatch_ui(c);
        }

        // 1b. IPC requests: commands apply now (bypassing the UI save-picker
        // solicit); queries are parked and answered in step 8b once the mirror
        // reflects this tick's commands, giving each connection read-your-writes.
        self.drain_ipc();

        // 2. Thumbnails.
        if let Some(rx) = &self.thumb_rx {
            let thumbs: Vec<Thumbnail> = rx.try_iter().collect();
            if let Some(egui) = self.egui.as_mut() {
                for t in thumbs {
                    egui.set_thumbnail(t.id, t.w, t.h, &t.rgba);
                }
            }
        }

        // 3. Audio errors.
        while let Ok(e) = self.audio_err_rx.try_recv() {
            log::warn!("audio stream error: {e}");
            self.mirror.audio_error = Some(e.to_string());
        }

        // 4. Shader hot-reload (debounced).
        if self.watcher.as_ref().is_some_and(|w| w.dirty()) {
            self.dirty_at = Some(Instant::now());
        }
        if self
            .dirty_at
            .is_some_and(|t| t.elapsed() >= SHADER_DEBOUNCE)
        {
            self.dirty_at = None;
            self.load_shader();
        }

        // 5. The engine: clock, sequencer, the musical re-loop, and the current
        // source's newest frame. Everything portable about a tick happens in
        // there; what comes back is the GPU half, which is this shell's.
        let tick = self.engine.tick(Instant::now());
        let snap = tick.snap;

        // 5c. Camera cues: pick up taps that couldn't attach earlier and move
        // each effective delay toward its target (slew, or snap on the grid).
        self.resolve_camera_delays(tick.boundary_crossed);

        // 6/6b/7. Everything GPU about this tick, in one place — see
        // [`Self::render_tick`].
        let audio: AudioFrame = *self.audio_out.read();
        self.render_tick(tick, &snap, &audio);

        // 8. Publish the mirror for the control window.
        self.build_mirror(&snap, &audio);

        // 8b. Answer IPC queries parked in step 1b, now that the mirror is
        // fresh — so a client's `get` after a `cmd` sees the command's effect.
        self.answer_ipc_queries();

        // 9. Redraw scheduling. Skip windows that are occluded (screen locked/
        // asleep, covered, minimized): the compositor has no drawable to hand
        // back, so `get_current_texture()` returns instantly instead of
        // blocking on vsync, and polling it in that state spins the loop at
        // full CPU speed leaking GPU-side surface resources.
        if let Some(g) = self.graphics.as_ref() {
            if !self.output_occluded {
                g.output.window.request_redraw();
            }
            // control repaints ~each tick while shown (cheap; it clears on occlusion)
            if !self.control_occluded {
                g.control.window.request_redraw();
            }
        }

        if self.should_quit {
            event_loop.exit();
        }

        // Nothing paces `ControlFlow::Poll` while both windows are occluded
        // (no vsync wait, no redraw request) — without this the loop free-spins
        // at raw CPU speed. A short sleep is a cheap, robust backstop regardless
        // of what future per-tick work might get added.
        if self.output_occluded && self.control_occluded {
            std::thread::sleep(Duration::from_millis(16));
        }
    }
}

/// A clip bank's display name from its source directory (the folder's own name).
fn dir_bank_name(dir: &std::path::Path) -> std::sync::Arc<str> {
    dir.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("clips")
        .into()
}

fn pick_monitor_from_window(window: &Window, index: Option<usize>) -> Option<MonitorHandle> {
    let monitors: Vec<MonitorHandle> = window.available_monitors().collect();
    let primary = window.primary_monitor();
    match index {
        Some(i) => monitors.get(i).cloned(),
        None => monitors
            .iter()
            .find(|m| primary.as_ref() != Some(*m))
            .cloned()
            .or(primary),
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics.is_some() {
            return;
        }
        let make = |title: &str, w: f64, h: f64, min_w: f64, min_h: f64| {
            event_loop.create_window(
                Window::default_attributes()
                    .with_title(title)
                    .with_inner_size(winit::dpi::LogicalSize::new(w, h))
                    .with_min_inner_size(winit::dpi::LogicalSize::new(min_w, min_h))
                    // See phosphor::theme::apply's doc for why this matters.
                    // eframe apps get the equivalent via a viewport command
                    // phosphor sends at theme-apply time, but this control
                    // window's egui integration never forwards viewport
                    // commands (see EguiCtl::render), so it's set here directly
                    // instead, at window creation.
                    .with_transparent(true),
            )
        };
        // The control layout is designed to stack down to ~420 px wide;
        // below that, rows would clip rather than wrap.
        let (output_win, control_win) = if let (Ok(o), Ok(c)) = (
            make("vidiotic output", 1280.0, 720.0, 160.0, 90.0),
            make("vidiotic control", 1000.0, 720.0, 420.0, 480.0),
        ) {
            (Arc::new(o), Arc::new(c))
        } else {
            log::error!("failed to create windows");
            event_loop.exit();
            return;
        };
        self.output_id = Some(output_win.id());
        self.control_id = Some(control_win.id());
        let graphics = match Graphics::new(output_win, control_win) {
            Ok(g) => g,
            Err(e) => {
                log::error!("gpu init: {e:#}");
                event_loop.exit();
                return;
            }
        };
        let renderer = Renderer::new(&graphics.device, graphics.output.config.format);
        let egui = EguiCtl::new(
            &graphics.control.window,
            &graphics.device,
            graphics.control.config.format,
        );
        self.graphics = Some(graphics);
        self.renderer = Some(renderer);
        self.egui = Some(egui);
        self.watcher = ShaderWatcher::new(&self.shader_path).ok();
        self.load_shader();
        self.load_referenced_isf();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let is_control = self.control_id == Some(id);
        let is_output = self.output_id == Some(id);

        if is_control {
            if let (Some(g), Some(egui)) = (self.graphics.as_ref(), self.egui.as_mut()) {
                if !matches!(event, WindowEvent::RedrawRequested) {
                    let win = g.control.window.clone();
                    let (consumed, _repaint) = egui.on_window_event(&win, &event);
                    if consumed {
                        return;
                    }
                }
            }
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(g) = self.graphics.as_mut() {
                    if is_output {
                        g.output.resize(&g.device, size.width, size.height);
                    } else if is_control {
                        g.control.resize(&g.device, size.width, size.height);
                    }
                }
            }
            // Keyboard shortcuts are honored from either window. Control-window
            // key events only reach here when egui didn't consume them above
            // (i.e. no text field is focused), so typing still wins.
            WindowEvent::ModifiersChanged(m) if is_output || is_control => self.modifiers = m,
            WindowEvent::KeyboardInput { event, .. } if is_output || is_control => {
                self.handle_key(&event);
            }
            WindowEvent::Occluded(occluded) if is_output => self.output_occluded = occluded,
            WindowEvent::Occluded(occluded) if is_control => self.control_occluded = occluded,
            WindowEvent::RedrawRequested if is_output => self.render_output(),
            WindowEvent::RedrawRequested if is_control => self.render_control(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.update(event_loop);
    }
}

/// Run the player until quit: builds the `App` and drives the winit event loop.
///
/// # Errors
/// Propagates failure to create or run the winit event loop.
pub fn run(boot: Boot) -> anyhow::Result<()> {
    let event_loop = winit::event_loop::EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App::new(boot);
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The line [`mutates_project`] draws, from both sides. It decides whether
    /// closing an unsaved session asks where to save, so a command on the wrong
    /// side either loses work silently or nags after every tempo nudge.
    ///
    /// Representative rather than exhaustive: a `match` over the whole `Command`
    /// enum here would be a second copy of the function, which proves only that
    /// it was copied correctly.
    #[test]
    fn mutates_project_covers_the_document_and_not_the_transport() {
        // Anything a `.viproj` stores.
        for cmd in [
            Command::AddCue(0),
            Command::RemoveCue(0),
            Command::SetCueIn(0, 1.0),
            Command::SetCueOut(0, Some(2.0)),
            Command::SetCuePreserve(0, Some(true)),
            Command::MoveCue(0, 1),
            Command::AddBank,
            Command::CloneBank,
            Command::SetClipBpm(0, Some(120.0)),
            Command::SetClipDir(PathBuf::from("/clips")),
            Command::AddClipDirAsBank(PathBuf::from("/clips")),
            Command::LoadIsf(PathBuf::from("/x.fs")),
            Command::AddCameraCue("uid".into()),
        ] {
            assert!(mutates_project(&cmd), "{cmd:?} edits the document");
        }

        // Dialing in a groove is not yet a project worth naming, and neither is
        // routing which bank is live or what the output window is doing.
        for cmd in [
            Command::SetBpm(128.0),
            Command::TapTempo,
            Command::SoftReset,
            Command::CycleLiveBank(1),
            Command::ToggleFullscreen,
            Command::SaveProject,
            Command::Undo,
            Command::Redo,
        ] {
            assert!(!mutates_project(&cmd), "{cmd:?} is not a document edit");
        }
    }

    #[test]
    fn dir_bank_name_is_the_folders_own_name() {
        assert_eq!(
            &*dir_bank_name(std::path::Path::new("/a/b/friday")),
            "friday"
        );
        // A trailing slash still names the folder, not the empty string.
        assert_eq!(
            &*dir_bank_name(std::path::Path::new("/a/b/friday/")),
            "friday"
        );
        // Nothing to name: the root, and the empty path.
        assert_eq!(&*dir_bank_name(std::path::Path::new("/")), "clips");
        assert_eq!(&*dir_bank_name(std::path::Path::new("")), "clips");
    }
}
