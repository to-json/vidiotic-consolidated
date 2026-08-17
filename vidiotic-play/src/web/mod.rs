//! The `/play` browser shell: two canvases, one device, one submit.
//!
//! This is the wasm counterpart of `vidiotic::app`, and it is now the same
//! shape: an [`Engine`] that owns the session, plus the machine-specific half
//! around it. The machine-specific half here is very small — canvases, a
//! `requestAnimationFrame` loop, and an [`Opener`](crate::engine::Opener) that
//! turns bytes into frames — because everything a vidiotic session *is* was
//! extracted into the engine and both shells now drive one copy of it.
//!
//! What that bought, concretely: cues, cue banks, the sequencer's rotation, the
//! clip pool, document undo, and the whole verb vocabulary arrived here without
//! being written a second time. What is still absent is absent honestly — a
//! command this shell cannot serve comes *back* out of
//! [`Engine::apply_command`] and lands in the status line naming itself, rather
//! than being swallowed by a `_ => {}`.
//!
//! **Decode is pulled by the render loop, not paced by a thread.** See
//! [`sources`]: a frame is decoded only when the timeline moves to a different
//! sample, so a 30 fps clip on a 60 Hz display decodes on half the frames and
//! uploads on none of the rest.
//!
//! # Ownership
//! The shell lives in a `thread_local` rather than being threaded through the
//! JS boundary, because wasm is single-threaded and every entry point here is
//! called from the same task. Each entry borrows it briefly and drops the
//! borrow before yielding — the render callback in particular must not hold it
//! across an `await`, or a `load_clip` landing mid-frame would panic.

// The bake driver moved to `vidiotic_bake::web`, which is where the compressor
// and the muxer already were — `/chop` bakes in a browser too, and the two web
// shells cannot depend on each other. Nothing re-exports it: its `Baker`,
// `is_baked` and `bake_size` are `#[wasm_bindgen]` items in a crate this one
// links, so they land in this bundle's glue exactly as before and `boot.js`
// imports them unchanged.
mod cameras;
mod input;
mod project;
mod sources;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use cameras::camera_device_pairs;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::chain::{ChainSlot, ClipId, SlotRef};
use crate::clip::Clip as Movie;
use crate::clippool::ClipSource;
use crate::commands::Command;
use crate::engine::{Boot, Engine};
use crate::gfx::Graphics;
use crate::grammar::{self, Step, Verb};
use crate::render::{Globals, Renderer};
use crate::video::frame::{DecodedFrame, PixelData};
use sources::{Flag, Library, Loaded, SoftFlag, WebSources};

thread_local! {
    static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) };
}

/// Logs to the browser console. `log::warn!` from deep inside `gfx`/`render`
/// is otherwise silently dropped, which would make the BC negotiation and any
/// shader-compile failure invisible — the two things most worth seeing.
struct ConsoleLog;

impl log::Log for ConsoleLog {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        let msg = JsValue::from_str(&format!("[{}] {}", record.level(), record.args()));
        match record.level() {
            log::Level::Error => web_sys::console::error_1(&msg),
            log::Level::Warn => web_sys::console::warn_1(&msg),
            _ => web_sys::console::log_1(&msg),
        }
    }
    fn flush(&self) {}
}

static LOGGER: ConsoleLog = ConsoleLog;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
}

fn window() -> web_sys::Window {
    web_sys::window().expect("no window")
}

fn now_ms() -> f64 {
    window().performance().expect("no performance clock").now()
}

/// How long the analyser goes unfed before the shell starts feeding it silence.
///
/// Comfortably longer than the tap's own cadence (2048 samples, ~43 ms at
/// 48 kHz) so a live but bursty source is never mistaken for a dead one.
const AUDIO_STARVED_MS: f64 = 250.0;

/// The grammar state `engine_state` reports. Was `panel::GrammarView`; the P0
/// panel it fed is gone, but the smoke test's readout is not.
struct GrammarView<'a> {
    pane: &'a str,
    /// `None` when idle; otherwise the open root's or sticky mode's label.
    pending: Option<&'a str>,
    /// `(key, label)` per reachable conjugation.
    options: Vec<(&'static str, &'static str)>,
    last_verb: Option<&'a str>,
}

/// The browser's half of the player.
struct Shell {
    gfx: Graphics,
    renderer: Renderer,
    egui_ctx: egui::Context,
    egui_rend: egui_wgpu::Renderer,
    input: input::Shared,
    /// Device pixels per CSS pixel for the control canvas.
    dpr: f32,

    /// The session. Identical code to the one `vidiotic` runs.
    engine: Engine,
    /// Clip bytes and probes, by pool id — what this shell's opener resolves
    /// against, since a browser pool entry's "path" is only a display name.
    library: Library,
    /// Decompress HAP blocks on the CPU instead of handing them to the GPU.
    ///
    /// Forced on when the adapter lacks `texture-compression-bc`, because there
    /// the block texture simply cannot be created. Settable independently so the
    /// path can be exercised on a machine that *does* have BC — otherwise the
    /// fallback would only ever run where nobody can test it, which is the same
    /// as not having one (see [`set_soft_decode`]).
    soft: SoftFlag,

    /// Whether the clips are held. Not an engine concept — the sequencer and the
    /// beat grid have no pause, and natively there is no button for one — so it
    /// lives here, shared with the opener so that a cue the rotation arms *after*
    /// the pause comes up held rather than running.
    paused: Flag,
    /// `performance.now()` at the previous frame, for the delta.
    last_ms: f64,
    /// Seconds since boot, for the shader's `time` uniform and egui's clock.
    elapsed: f64,
    /// Smoothed frame rate, purely for the readout.
    fps: f64,

    /// The effect chosen in the panel. Applied to every cue in the edit bank so
    /// it survives a swap, and kept here as the chain to use when no cue is
    /// playing at all — otherwise picking an effect with an empty pool would
    /// appear to do nothing.
    effect: Option<usize>,
    status: String,

    /// The FFT, fed by whatever the page has tapped — a mic, a tab's audio, a
    /// synthetic tone from a test. Identical code to the one the analysis
    /// thread runs natively; only the source of samples differs.
    ///
    /// Always present, because an analyser over silence is exactly what a
    /// player with no audio source should show: `lvl` at zero and a flat
    /// spectrum, rather than a special case threaded through the uniforms.
    analyzer: crate::analysis::Analyzer,
    /// Whether anything has ever been fed. Only for the readout — the shaders
    /// cannot tell the difference between "no source" and "a silent one", and
    /// the visitor very much can.
    audio_live: bool,
    /// `performance.now()` of the last [`push_audio`], so a source that stops
    /// delivering decays instead of latching. See [`AUDIO_STARVED_MS`].
    last_audio_ms: f64,

    /// Capture devices, as the page last enumerated them.
    ///
    /// Held rather than asked for, because enumeration is async in a browser
    /// and a panel is drawn synchronously. `RefreshCameras` asks the page; the
    /// answer arrives later through [`set_cameras`] and lands here.
    camera_devices: Vec<cameras::Device>,
    /// Cameras that are actually on air, by uid. Shared with the opener.
    taps: cameras::Taps,

    /// What a save is called, minus the extension.
    ///
    /// Set by the page when it loads a `.viproj`, so saving a project you
    /// opened gives it back under its own name rather than a generic one. The
    /// default is what a session built by dropping clips deserves: it was never
    /// called anything.
    project_name: String,

    /// The read-only view the panels draw, rebuilt every frame by the engine.
    /// Held rather than made fresh so its Vecs keep their allocations.
    mirror: crate::commands::UiMirror,
    /// The panels emit here; `build_ui` drains it into the engine on the same
    /// frame. Natively this is the channel the winit loop owns.
    cmd_tx: crossbeam_channel::Sender<Command>,
    cmd_rx: crossbeam_channel::Receiver<Command>,
    /// Clip thumbnails, by pool id.
    thumbs: HashMap<ClipId, egui::TextureHandle>,
}

fn generate_thumbnail(probe: &Movie, bytes: &[u8]) -> Option<egui::ColorImage> {
    let track = probe.track();
    if !track.is_hap() {
        return None;
    }
    let sample = track.sample_data(bytes, 0)?;
    let mut main_buf = Vec::new();
    let mut alpha_buf = Vec::new();
    let meta = vidiotic_bake::hap::decode_frame(sample, 1, &mut main_buf, &mut alpha_buf).ok()?;

    let (src_w, src_h) = (track.width as usize, track.height as usize);
    if src_w == 0 || src_h == 0 {
        return None;
    }

    let mut rgba = vec![0u8; src_w * src_h * 4];
    match meta.format {
        vidiotic_bake::hap::HapTextureFormat::Bc1 => {
            texpresso::Format::Bc1.decompress(&main_buf, src_w, src_h, &mut rgba);
        }
        vidiotic_bake::hap::HapTextureFormat::Bc3 | vidiotic_bake::hap::HapTextureFormat::Bc3YCoCg => {
            texpresso::Format::Bc3.decompress(&main_buf, src_w, src_h, &mut rgba);
        }
        _ => return None,
    }

    let thumb_w = 128;
    let thumb_h = 86;
    let mut thumb_rgba = vec![0u8; thumb_w * thumb_h * 4];

    for ty in 0..thumb_h {
        let sy = (ty * src_h) / thumb_h;
        for tx in 0..thumb_w {
            let sx = (tx * src_w) / thumb_w;
            let src_idx = (sy * src_w + sx) * 4;
            let dst_idx = (ty * thumb_w + tx) * 4;
            if src_idx + 3 < rgba.len() && dst_idx + 3 < thumb_rgba.len() {
                thumb_rgba[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
            }
        }
    }

    Some(egui::ColorImage::from_rgba_unmultiplied(
        [thumb_w, thumb_h],
        &thumb_rgba,
    ))
}

#[wasm_bindgen]
pub fn deliver_thumbnail(clip_name: &str, width: u32, height: u32, rgba: &[u8]) -> Result<(), JsValue> {
    if width == 0 || height == 0 || rgba.len() != (width * height * 4) as usize {
        return Err(JsValue::from_str("invalid thumbnail dimensions or buffer length"));
    }
    with_shell(|s| {
        let clip_id = s.mirror.clips.iter().find(|c| c.name.as_ref() == clip_name).map(|c| c.id);
        if let Some(id) = clip_id {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [width as usize, height as usize],
                rgba,
            );
            let handle = s.egui_ctx.load_texture(
                format!("thumb:{id}"),
                img,
                egui::TextureOptions::LINEAR,
            );
            s.thumbs.insert(id, handle);
        }
        Ok(())
    })
}

impl Shell {
    fn frame(&mut self) {
        let ms = now_ms();
        let dt = ((ms - self.last_ms) / 1000.0).clamp(0.0, 0.25);
        self.last_ms = ms;
        self.elapsed += dt;
        if dt > 0.0 {
            // Exponential smoothing; a raw per-frame reciprocal is unreadable.
            self.fps = self.fps.mul_add(0.9, (1.0 / dt) * 0.1);
        }

        self.handle_keys();
        self.drain_pending();

        let tick = self.engine.tick(web_time::Instant::now());

        if tick.blank {
            let black = DecodedFrame {
                pixels: PixelData::Rgba { data: vec![0; 16], stride: 8 },
                w: 2,
                h: 2,
                pts_sec: 0.0,
            };
            self.renderer.upload_frame(&self.gfx.device, &self.gfx.queue, &black);
        }
        if let Some(f) = &tick.frame {
            self.renderer.upload_frame(&self.gfx.device, &self.gfx.queue, f);
        }
        // A cue's own chain wins; the panel's choice stands in when nothing is
        // cued, so the effect list is live on an empty pool too.
        let chain = if tick.chain.is_empty() { self.effect_chain() } else { tick.chain };
        self.renderer.set_active_chain(chain);

        self.paint(&tick.snap);
    }

    /// Feed the engine everything the grammar raised, on the same path a click
    /// would take.
    fn drain_pending(&mut self) {
        while let Some(c) = self.engine.next_pending() {
            self.dispatch(c);
        }
    }

    /// The command choke point, exactly as `vidiotic::app::App::dispatch` is —
    /// undo bookkeeping, the engine, then this shell for the remainder.
    ///
    /// The remainder here is *reported*, not handled. Saving a project, opening
    /// a file dialog, switching an audio device, going fullscreen, talking to a
    /// camera: none of it exists in this build, and a verb that resolves and
    /// then does nothing is indistinguishable from a broken grammar unless
    /// something says which one fired.
    fn dispatch(&mut self, cmd: Command) {
        match cmd {
            Command::Undo => self.engine.undo_document(),
            Command::Redo => self.engine.redo_document(),
            Command::SaveProject | Command::SaveProjectAs => self.save_project(None),
            Command::SaveProjectTo(p) => self.save_project(Some(&p)),
            c @ (Command::RefreshCameras
            | Command::SetCameraOnAir(..)
            | Command::AddCameraCue(_)
            | Command::RelinkCamera { .. }) => self.camera_command(c),
            other => {
                self.engine.record_undo(&other);
                let boundary = crate::undo::is_history_boundary(&other);
                if let Some(rest) = self.engine.apply_command(other) {
                    self.status = format!("/play does not do this yet: {rest:?}");
                    log::info!("{}", self.status);
                }
                if boundary {
                    self.engine.undo.reset();
                }
            }
        }
    }

    fn handle_keys(&mut self) {
        for k in input::take_keys(&self.input) {
            self.handle_key(&k);
        }
    }

    /// One key press. Mirrors `vidiotic::app::keys` in the one thing that
    /// matters — **the grammar gets first refusal** — so a sequence can never be
    /// torn apart by a colliding flat binding. `t` and `b` are both grammar
    /// tokens *and* flat tap keys natively, and that ordering is why they can be.
    fn handle_key(&mut self, k: &input::KeyPress) {
        if k.plain() && !k.repeat {
            if let Some(input) = grammar::token_of_key(&k.canon) {
                match self.engine.grammar.step(grammar::pane_table(self.engine.focused_pane), input)
                {
                    Step::Verb(v) => {
                        self.engine.apply_verb(v);
                        return;
                    }
                    Step::Pending | Step::Cancelled => return,
                    // Cancel while idle: the grammar declines it, so the flat
                    // bindings below get their turn.
                    Step::Rejected => {}
                }
            }
        }
        if !k.plain() {
            return;
        }
        // Flat bindings, and note what is *not* here: `t` and `b`. They are
        // grammar tokens, so the branch above always claims them first — exactly
        // as natively, where the flat tap keys only fire with the grammar off.
        // Tap tempo is `b a f f` (Pane→Clock, Fire→tap); a duplicate flat key
        // would be unreachable and the panel hint would be a lie.
        match k.canon.as_str() {
            "Space" => self.set_playing(self.paused.get()),
            "=" | "+" => self.engine.apply_verb(Verb::BpmDelta(1.0)),
            "-" => self.engine.apply_verb(Verb::BpmDelta(-1.0)),
            "[" => self.engine.apply_verb(Verb::NudgeBpm(-0.001)),
            "]" => self.engine.apply_verb(Verb::NudgeBpm(0.001)),
            // Shift is what tells these apart natively too: canonical key names
            // are lowercase, so `R` and `r` arrive identically and the modifier
            // is the only thing carrying the distinction.
            "r" if k.shift => self.engine.apply_verb(Verb::HardReset),
            "r" => self.engine.apply_verb(Verb::SoftReset),
            _ => {}
        }
    }

    /// Hold or resume playback. The clock keeps running: the beat grid is a
    /// tempo, not a transport, and a held clip on a live grid is exactly what a
    /// freeze looks like on stage.
    fn set_playing(&mut self, on: bool) {
        self.paused.set(!on);
    }

    fn playing(&self) -> bool {
        !self.paused.get()
    }

    fn effect_chain(&self) -> Vec<ChainSlot> {
        match self.effect {
            None => Vec::new(),
            Some(i) => {
                let (name, _) = crate::render::BUILTIN_EFFECTS[i];
                vec![ChainSlot::new(SlotRef::Builtin(name.into()))]
            }
        }
    }

    fn set_effect(&mut self, which: Option<usize>) {
        self.effect = which;
        let chain = self.effect_chain();
        let ids: Vec<crate::bank::CueId> = self.engine.banks[self.engine.edit_bank]
            .cues
            .iter()
            .map(|c| c.id)
            .collect();
        for id in ids {
            self.dispatch(Command::SetCueChain(id, chain.clone()));
        }
        // Apply it to the renderer now rather than waiting for the next tick to
        // read it back off the playing cue. Callers reasonably expect the choice
        // to be in effect when this returns — `centre_pixel` right after a
        // `set_effect` would otherwise read the *previous* chain, which is a
        // whole frame of lag that only shows up when something is measuring.
        self.renderer.set_active_chain(chain);
    }

    /// One frame, both heads, one encoder, one submit — the shape §10a measured
    /// in raw WebGPU, now through real `wgpu`.
    fn paint(&mut self, snap: &crate::clock::ClockSnapshot) {
        let (ow, oh) = (self.gfx.output.config.width, self.gfx.output.config.height);
        // The beat uniforms are live, and derived exactly as `vidiotic::app`
        // derives them — because they are now derived by the same engine. Same
        // `rem_euclid(16384.0)` fold to keep `beat` inside f32's
        // exactly-representable integer range over a long set, same phase
        // normalisation — and, since step 4f, the same `lvl`/`freqs`/audio
        // texture, produced by the same `analysis::Analyzer`.
        // A source that has gone away is silence, and has to be *fed* as
        // silence: a stopped analyser holds its last frame, so `lvl` would
        // latch and every reactive effect would freeze mid-flash. The threshold
        // is well past the tap's own cadence — it posts 2048 samples at a time,
        // about 43 ms at 48 kHz — so a live source never trips it.
        if self.audio_live && now_ms() - self.last_audio_ms > AUDIO_STARVED_MS {
            self.analyzer.feed_silence();
        }
        // Drain whatever the page has fed since the last frame. More than one
        // hop can be waiting — the tap posts in batches and a slow frame lets
        // them pile up — and the shaders want the *newest* analysis, not the
        // oldest, so this runs the queue out rather than taking one.
        while self.analyzer.poll().is_some() {}
        let audio = *self.analyzer.frame();

        let phrase = self.engine.sequencer.phrase_len();
        let mut globals = Globals {
            resolution: [ow as f32, oh as f32],
            mouse: [0.0, 0.0],
            time: self.elapsed as f32,
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
        self.renderer.update_globals(&self.gfx.queue, &globals);
        self.renderer.upload_audio(&self.gfx.queue, &audio.audio_tex);

        let ui = self.build_ui(snap);

        let mut encoder = self
            .gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("play-frame"),
            });

        // --- output head: video through the composite pass ---
        let out_frame = self.gfx.output.acquire(&self.gfx.device);
        if let Some(f) = &out_frame {
            let view = f.texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.renderer.render(
                &self.gfx.device,
                &self.gfx.queue,
                &mut encoder,
                &view,
                ow,
                oh,
            );
        }

        // --- control head: egui ---
        let ctl_frame = self.gfx.control.acquire(&self.gfx.device);
        let mut ui_bufs = Vec::new();
        if let Some(f) = &ctl_frame {
            let view = f.texture.create_view(&wgpu::TextureViewDescriptor::default());
            let sd = egui_wgpu::ScreenDescriptor {
                size_in_pixels: [self.gfx.control.config.width, self.gfx.control.config.height],
                pixels_per_point: ui.pixels_per_point,
            };
            for (id, delta) in &ui.textures_delta.set {
                self.egui_rend
                    .update_texture(&self.gfx.device, &self.gfx.queue, *id, delta);
            }
            ui_bufs = self.egui_rend.update_buffers(
                &self.gfx.device,
                &self.gfx.queue,
                &mut encoder,
                &ui.prims,
                &sd,
            );
            {
                let mut pass = encoder
                    .begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("egui"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(clear_color()),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    })
                    .forget_lifetime();
                self.egui_rend.render(&mut pass, &ui.prims, &sd);
            }
        }

        // The claim being demonstrated: one submit spanning canvases that live
        // in two different documents.
        self.gfx
            .queue
            .submit(ui_bufs.into_iter().chain([encoder.finish()]));
        if let Some(f) = out_frame {
            f.present();
        }
        if let Some(f) = ctl_frame {
            f.present();
        }
        for id in &ui.textures_delta.free {
            self.egui_rend.free_texture(id);
        }
    }

    /// Read the grammar's state for [`engine_state`].
    ///
    /// The panels get this from `UiMirror::grammar_modal` and draw it as the
    /// which-key overlay. This flatter form exists for the smoke test, which
    /// needs the pending root and last verb as plain strings — none of the
    /// grammar has pixels on the output head, so reading it back is the only
    /// way to show it is running.
    fn grammar_view(&self) -> GrammarView<'_> {
        use crate::grammar::{GrammarState, KEY_TOKENS};

        let table = grammar::pane_table(self.engine.focused_pane);
        let (pending, options) = match self.engine.grammar.state {
            GrammarState::Idle => (
                None,
                // Idle, the reachable options are the roots themselves.
                table
                    .roots
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| !r.label.is_empty())
                    .map(|(i, r)| (KEY_TOKENS[i], r.label))
                    .collect(),
            ),
            GrammarState::AwaitingConjugation { root } => {
                let entry = &table.roots[root.index()];
                (
                    Some(entry.label),
                    entry
                        .conjugations
                        .iter()
                        .enumerate()
                        .filter_map(|(i, c)| c.as_ref().map(|c| (KEY_TOKENS[i], c.label)))
                        .collect(),
                )
            }
            GrammarState::Sticky { label, entries, .. } => (
                Some(label),
                entries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, v)| v.map(|_| (KEY_TOKENS[i], "•")))
                    .collect(),
            ),
        };
        GrammarView {
            pane: self.engine.focused_pane.label(),
            pending,
            options,
            last_verb: self.engine.last_verb.as_deref(),
        }
    }

    fn build_ui(&mut self, snap: &crate::clock::ClockSnapshot) -> UiFrame {
        phosphor::theme::sync(&self.egui_ctx);
        let size_px = egui::vec2(
            self.gfx.control.config.width as f32,
            self.gfx.control.config.height as f32,
        );
        let raw = input::take(&self.input, size_px / self.dpr, self.elapsed);

        // The engine publishes everything a session knows; this shell overlays
        // the little it knows that the engine cannot. Natively `App` does the
        // same call and overlays rather more — audio devices, a project path, a
        // window to make fullscreen. A browser has none of those, which is why
        // this overlay is five lines and not thirty.
        //
        // The shader pool is not one of the absences: it lives on the renderer,
        // which this shell owns, and the chain editor reads it to list what a
        // slot can be set to. Leaving it empty is what made "+ ISF" load a
        // shader that then appeared nowhere.
        self.engine
            .build_mirror(snap, self.analyzer.frame(), &mut self.mirror);
        for entry in &mut self.mirror.clips {
            entry.has_thumb = self.thumbs.contains_key(&entry.id);
        }
        for cue in &mut self.mirror.cues {
            cue.has_thumb = self.thumbs.contains_key(&cue.clip);
        }
        self.mirror.shader_name = self
            .effect
            .and_then(|i| crate::render::BUILTIN_EFFECTS.get(i))
            .map(|(name, _)| (*name).to_string());
        self.build_camera_rows();
        self.mirror.shader_pool = self.renderer.pool_view();
        self.mirror.shader_error = self.renderer.shader_error().cloned();
        self.mirror.fullscreen = false;

        let ctx = self.egui_ctx.clone();
        let full = ctx.run_ui(raw, |ui| {
            crate::ui::control_ui(ui, &self.mirror, &self.cmd_tx, &self.thumbs);
        });

        // Drain on the same frame the panels filled it. Anything the engine does
        // not implement comes back, and the browser says so in its status line
        // rather than swallowing it — a verb that resolves and then does nothing
        // is indistinguishable from a broken one unless it tells you.
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            // The two the page can answer. Natively these open an `rfd` dialog;
            // here they ask the page for a file and come back through
            // `load_isf_source` / `load_shader_source`.
            match cmd {
                Command::PickIsf => request_file(PICK_ISF),
                Command::PickShader => request_file(PICK_SHADER),
                Command::SaveProject | Command::SaveProjectAs => self.save_project(None),
                Command::SaveProjectTo(p) => self.save_project(Some(&p)),
                c @ (Command::RefreshCameras
                | Command::SetCameraOnAir(..)
                | Command::AddCameraCue(_)
                | Command::RelinkCamera { .. }) => self.camera_command(c),
                other => {
                    if let Some(rest) = self.engine.apply_command(other) {
                        self.status = format!("{rest:?} is not something /play can do");
                    }
                }
            }
        }

        UiFrame {
            prims: ctx.tessellate(full.shapes, full.pixels_per_point),
            pixels_per_point: full.pixels_per_point,
            textures_delta: full.textures_delta,
        }
    }

    /// Take a clip into the pool and cue it up.
    ///
    /// Three steps, and all three are engine calls: intern the pool entry (which
    /// owns the id space), put it in a clip bank so the pool cursor can reach
    /// it, and toggle it active so it becomes a full-length cue in the live
    /// bank. That last one is the same call `--clip` makes natively.
    fn load_clip(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), String> {
        let bytes = Rc::new(bytes);
        let probe = Movie::open(Rc::clone(&bytes)).map_err(|e| format!("{name}: {e}"))?;
        let loaded = Loaded {
            bytes: Rc::clone(&bytes),
            width: probe.width(),
            height: probe.height(),
            frames: probe.frame_count(),
            duration: probe.duration_sec(),
        };
        log::info!(
            "loaded {name}: {}x{}, {} frames, {:.2}s",
            loaded.width,
            loaded.height,
            loaded.frames,
            loaded.duration
        );
        let id = self
            .engine
            .intern_clip(ClipSource::File(name.into()), name.into());
        self.library.borrow_mut().insert(id, loaded);

        // Extract and register thumbnail for egui UI tiles if possible
        if let Some(img) = generate_thumbnail(&probe, &bytes) {
            let handle = self.egui_ctx.load_texture(
                format!("thumb:{id}"),
                img,
                egui::TextureOptions::LINEAR,
            );
            self.thumbs.insert(id, handle);
        }

        if let Some(bank) = self.engine.clip_banks.first_mut() {
            bank.clip_ids.push(id);
        } else {
            self.engine.push_clip_bank("dropped".into(), None, vec![id]);
        }
        self.engine.selected_clip = Some(id);
        let beat = self.engine.last_beat;
        self.engine.toggle_clip_active(id, beat);
        // A cue created after the effect was chosen still gets it.
        let effect = self.effect;
        self.set_effect(effect);
        self.status.clear();
        Ok(())
    }

    /// Compile an ISF shader the page read for us, and hang it on the selected
    /// cue.
    ///
    /// Nothing here is browser-specific except where the text came from:
    /// `isf::transpile` is in `vidiotic-core` and `Renderer::load_isf` is in
    /// this crate, so the browser compiles the identical shader the native
    /// player does. Only the `std::fs::read_to_string` was ever native, which
    /// is why this is a dozen lines and not a port.
    ///
    /// Two honest differences from `App::load_isf`. `IMPORTED` images do not
    /// resolve — there is no directory to resolve them against — and
    /// `Renderer::load_isf` binds those black and loads anyway, so a shader
    /// that uses one renders without it rather than refusing. And with no cue
    /// selected this still compiles into the pool instead of declining: the
    /// pool is a list the chain editor can assign from later, so loading is
    /// useful before there is anywhere to put it.
    /// The running session as a `Project`.
    ///
    /// The mirror image of [`Shell::load_project`], and it goes through the
    /// same `from_runtime` the desktop player saves with — so a set marked up
    /// in a browser opens on the machine driving the projector, which is the
    /// only reason to be able to save one at all.
    ///
    /// **Paths are rewritten to `clips/<file>` afterwards.** `from_runtime`
    /// relativizes each clip against a project directory, and there is no
    /// directory here — pool names are whatever the visitor's files were
    /// called. Naming them where the bundle actually puts them is what makes
    /// the archive self-consistent: the project points at its clips, and the
    /// clips are there.
    fn project_snapshot(&mut self) -> vidiotic_core::project::Project {
        use vidiotic_core::project::{ClipMeta, Project, SessionDefaults, SyncSpec};

        let snap = self.engine.clock.snapshot();
        let defaults = SessionDefaults {
            bpm: snap.bpm,
            quantum: self.engine.time_sig.quantum(),
            phrase_len: self.engine.sequencer.phrase_len().round() as u32,
            // Always internal: `SyncKind::Link` is Ableton Link, which has no
            // browser transport. Writing `Link` because the field exists would
            // produce a project that silently waits for a clock that cannot
            // arrive.
            sync: SyncSpec::Internal,
            preserve_playhead: self.engine.preserve_playhead,
            loop_len: self.engine.loop_len,
            advanced: self.engine.advanced,
            ts_num: self.engine.time_sig.num,
            ts_den: self.engine.time_sig.den,
            phrase_cadence: Some(self.engine.phrase_cadence.into()),
            loop_cadence_set: true,
            loop_cadence: self.engine.loop_cadence.map(Into::into),
            // A shader reached this page as source text through a file chooser,
            // so there is no path to record. Recording the *name* would write a
            // project that fails to find a file of that name on load.
            shader_path: None,
        };

        // What the page knows about each clip, which is what it decoded on the
        // way in. Natively this is a cache filled by probing files; here the
        // probe already happened and `Loaded` is the answer.
        let meta: HashMap<ClipId, ClipMeta> = self
            .library
            .borrow()
            .iter()
            .map(|(id, l)| {
                (
                    *id,
                    ClipMeta {
                        fps: (l.duration > 0.0).then(|| l.frames as f64 / l.duration),
                        frames: Some(l.frames as u64),
                        duration_sec: Some(l.duration),
                        ..Default::default()
                    },
                )
            })
            .collect();

        let nowhere = std::path::Path::new("");
        let mut proj = Project::from_runtime(
            nowhere,
            nowhere,
            &self.engine.clips,
            &self.engine.clip_banks,
            &self.engine.banks,
            &meta,
            defaults,
        );
        // Files only. A camera clip has no path — it is a device uid — and
        // giving it one would write a project that looks for a file called
        // `clips/` and reports it missing on every load.
        for (clip, spec) in self.engine.clips.iter().zip(&mut proj.clips) {
            if !matches!(clip.source, ClipSource::File(_)) {
                continue;
            }
            let file = std::path::Path::new(&spec.path)
                .file_name()
                .map_or_else(|| spec.path.clone(), |n| n.to_string_lossy().into_owned());
            spec.path = format!("clips/{file}");
        }
        proj
    }

    /// Answer the four camera commands.
    ///
    /// Two of them are pure engine work — `AddCameraCue` interns a pool clip
    /// and cues it, `RelinkCamera` repoints every clip naming the missing
    /// device — and the engine owns that half, shared with
    /// `vidiotic::app::cameras`. The other two need a browser API, so they
    /// become a request to the page and the answer comes back through
    /// [`set_cameras`] / [`camera_ready`].
    fn camera_command(&mut self, cmd: Command) {
        match cmd {
            Command::RefreshCameras => request_cameras(),
            Command::SetCameraOnAir(uid, on) => {
                if !on {
                    // Dropped here as well as in the page: the tap is what the
                    // opener resolves against, so a cue must stop finding it the
                    // moment the visitor says off, not whenever the page's
                    // teardown happens to run.
                    self.taps.borrow_mut().remove(&*uid);
                }
                request_camera(&uid, on);
            }
            Command::AddCameraCue(uid) => {
                let devices = camera_device_pairs(&self.camera_devices);
                self.engine.add_camera_cue(&devices, &uid);
            }
            Command::RelinkCamera { from, to } => {
                let devices = camera_device_pairs(&self.camera_devices);
                if let Err(msg) = self.engine.relink_camera(&devices, &from, &to) {
                    self.status = msg;
                }
            }
            _ => unreachable!("camera_command called with {cmd:?}"),
        }
    }

    /// The camera rows the panel draws.
    ///
    /// The browser half of what `app/mirror.rs` calls `build_camera_rows`: the
    /// engine cannot fill this because a device is not a session concept, and
    /// the row shape is all the two platforms share.
    fn build_camera_rows(&mut self) {
        let devices = camera_device_pairs(&self.camera_devices);
        let taps = self.taps.borrow();
        self.mirror.cameras = self.engine.camera_rows(&devices, |uid| match taps.get(uid) {
            Some(t) => (true, t.status.clone()),
            None => (false, "off air".into()),
        });
    }

    /// Save the session: build the bundle and hand it to the page.
    ///
    /// All three save commands land here, because in a browser they are one
    /// action. `SaveProject` writes back over what was opened, `SaveProjectAs`
    /// asks where — but a tab has no "where": every save is a download into
    /// whatever folder the browser is configured for, under a name the visitor
    /// can change in the download prompt. Pretending otherwise would mean
    /// drawing a destination picker that decides nothing.
    ///
    /// `SaveProjectTo` does carry a path, and its file stem is used as the
    /// name — a project loaded from `friday.viproj` saves as `friday.zip`.
    fn save_project(&mut self, to: Option<&std::path::Path>) {
        let stem = to
            .and_then(std::path::Path::file_stem)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.project_name.clone());
        let name = vidiotic_core::bundle::sanitize(&stem);
        let entries = self.bundle_entries(&name);
        let clips = entries.len() - 1;
        let archive = match vidiotic_core::bundle::zip(&entries) {
            Ok(archive) => archive,
            Err(e) => {
                self.status = format!("could not save {name}.zip: {e}");
                log::error!("{}", self.status);
                return;
            }
        };
        self.status =
            format!("saved {name}.zip — {clips} clip(s), {} KiB", archive.len() / 1024);
        log::info!("{}", self.status);
        deliver_file(&format!("{name}.zip"), &archive);
    }

    /// The session as a bundle: `[[path, bytes], …]`, the `.viproj` first.
    ///
    /// One archive rather than a bare `.viproj`, for the reason
    /// `vidiotic_core::bundle` exists — a project references clips it does not
    /// contain, so on its own it is the half that points at the other half.
    /// A browser can hand back one file, and this is the one worth handing back.
    fn bundle_entries(&mut self, name: &str) -> Vec<(String, Vec<u8>)> {
        let proj = self.project_snapshot();
        let mut entries = vec![(
            format!("{name}/{name}.viproj"),
            vidiotic_core::project::to_ron_bytes(&proj),
        )];
        let library = self.library.borrow();
        for (clip, spec) in self.engine.clips.iter().zip(&proj.clips) {
            // A camera clip has no bytes to carry — the project names a device,
            // and whether that device exists is a fact about the machine the
            // project is opened on, not about the bundle.
            if !matches!(clip.source, ClipSource::File(_)) {
                continue;
            }
            if let Some(loaded) = library.get(&clip.id) {
                entries.push((format!("{name}/{}", spec.path), (*loaded.bytes).clone()));
            }
        }
        entries
    }

    /// Load a `.viproj`, resolving its clips against what the pool already
    /// holds.
    ///
    /// A swap, not a merge — see [`project`]. The bytes survive; the ids,
    /// banks, cues and tempo are the project's.
    fn load_project(&mut self, text: &str) -> Result<String, String> {
        let parsed = vidiotic_core::project::from_ron_versioned(text, "project")
            .map_err(|e| format!("{e:#}"))?;

        // What the pool holds, by display name, so the new pool can be re-keyed
        // onto the same bytes rather than asking for them again.
        let mut held: HashMap<String, sources::Loaded> = HashMap::new();
        {
            let library = self.library.borrow();
            for clip in &self.engine.clips {
                if let (Some(name), Some(loaded)) =
                    (project::pool_name(&clip.source), library.get(&clip.id))
                {
                    held.insert(name.to_string(), loaded.clone());
                }
            }
        }

        let fs = project::PoolFs::new(held.keys().cloned().collect());
        // An empty project dir: every stored path is compared by file name, so
        // there is no directory for a relative one to hang off.
        let resolved =
            vidiotic_core::project::resolve_with(parsed, std::path::Path::new(""), &fs);
        if !resolved.missing.is_empty() {
            let names = project::missing_names(&resolved);
            return Err(format!(
                "this project needs clip(s) the page does not have: {} — drop them in first",
                names.join(", ")
            ));
        }

        let asm = vidiotic_core::project::assemble(&resolved);
        let d = resolved.project.defaults.clone();
        let clips = asm.clips.len();
        let cues: usize = asm.cue_banks.iter().map(|b| b.cues.len()).sum();
        let banks = asm.cue_banks.len();

        *self.library.borrow_mut() = project::rekey(&asm.clips, &held);

        let mut engine = Engine::new(Boot {
            bpm: if d.bpm > 0.0 { d.bpm } else { 120.0 },
            time_sig: d.time_sig(),
            phrase_cadence: d.phrase_cadence(),
            loop_cadence: d.loop_cadence(),
            clips: asm.clips,
            clip_banks: asm.clip_banks,
            cue_banks: asm.cue_banks,
            auto_active: Vec::new(),
            preserve_playhead: d.preserve_playhead,
            advanced: d.advanced,
            opener: Box::new(WebSources {
                library: self.library.clone(),
                soft: self.soft.clone(),
                paused: self.paused.clone(),
                taps: Rc::clone(&self.taps),
            }),
        });
        // As at boot: the grammar is the only input model here.
        engine.grammar_on = true;

        // Put the live bank's cues into the rotation.
        //
        // `Engine::new` seeds cue *banks* but not the sequencer, so a freshly
        // loaded project has cues and an empty rotation — it comes up silent
        // until somebody fires one. That is defensible in front of a desktop
        // with a controller in your hands; it is the wrong answer for a page
        // somebody was handed a link to, where a black output head is
        // indistinguishable from a broken build.
        //
        // Deliberately a shell decision and not a change to `Engine::new`:
        // what should be playing the moment a project loads is a question about
        // the front end, and the native app answers it differently by having a
        // human there.
        let beat = engine.last_beat;
        for step in engine.cue_steps(engine.live_bank) {
            let ev = engine.sequencer.toggle_active(step, beat);
            engine.apply_seq_events(ev);
        }

        self.engine = engine;
        // The panel's standing effect belonged to the session that just went
        // away, and applying it to the new one would silently override chains
        // the project actually specifies.
        self.effect = None;

        // The project may name a shader; nothing here can fetch one by path.
        if asm.shader.is_some() {
            log::info!("the project names a shader, which a browser cannot resolve by path");
        }
        Ok(format!("loaded {clips} clip(s), {cues} cue(s) in {banks} bank(s)"))
    }

    fn load_isf_source(&mut self, name: &str, src: &str) -> Result<(), String> {
        let name: std::sync::Arc<str> = name.into();
        self.renderer
            .load_isf(
                &self.gfx.device,
                &self.gfx.queue,
                name.clone(),
                src,
                &|p| anyhow::bail!("no IMPORTED images in the browser: {}", p.display()),
            )
            .map_err(|e| format!("{name}: {e}"))?;
        if let Some(cue) = self.engine.selected_cue {
            self.engine
                .edit_cue(cue, |c| c.chain.push(ChainSlot::new(SlotRef::Isf(name.clone()))));
            self.status = format!("loaded {name}");
        } else {
            self.status = format!("loaded {name} into the pool — select a cue to use it");
        }
        Ok(())
    }

    /// Replace the global shader with WGSL the page read for us.
    ///
    /// The native counterpart is `SetShaderPath` plus a `ShaderWatcher`, and the
    /// watcher is the part that does not cross: there is no file to watch after
    /// the read. So this is the one-shot half — pick again to reload.
    fn load_shader_source(&mut self, name: &str, src: &str) -> Result<(), String> {
        self.renderer
            .set_shader(&self.gfx.device, src, crate::shader::ShaderLang::Wgsl);
        if let Some(e) = self.renderer.shader_error() {
            Err(format!("{name}: {e}"))
        } else {
            self.status = format!("compiled {name}");
            Ok(())
        }
    }

    /// Every loaded clip id, for [`engine_state`].
    fn pool_ids(&self) -> Vec<ClipId> {
        self.engine.clips.iter().map(|c| c.id).collect()
    }
}

/// A tessellated egui frame, held between building the UI and painting it.
struct UiFrame {
    prims: Vec<egui::ClippedPrimitive>,
    pixels_per_point: f32,
    textures_delta: egui::TexturesDelta,
}

fn clear_color() -> wgpu::Color {
    let c = phosphor::theme::palette().bg_base;
    // The surface is non-sRGB (gamma space) on purpose — see `gfx` — so the
    // palette's bytes go through unconverted, matching what egui paints.
    wgpu::Color {
        r: f64::from(c.r()) / 255.0,
        g: f64::from(c.g()) / 255.0,
        b: f64::from(c.b()) / 255.0,
        a: 1.0,
    }
}

/// Start the engine on two canvases.
///
/// Both are supplied by the host page, and the output one is expected to live
/// in a window the page opened. That split is deliberate: `window.open()` needs
/// a real user gesture, and a click handler in the page is where one reliably
/// exists — so the popup is JS's job and this function never has to care which
/// document its canvas came from.
///
/// # Errors
/// Returns a JS error if the GPU is unavailable or the device request fails.
///
/// # Panics
/// Panics if there is no `window` or no `performance` clock.
#[wasm_bindgen]
pub async fn boot(
    control: web_sys::HtmlCanvasElement,
    output: web_sys::HtmlCanvasElement,
) -> Result<(), JsValue> {
    let dpr = window().device_pixel_ratio() as f32;
    let gfx = Graphics::new_web(output, control.clone())
        .await
        .map_err(|e| JsValue::from_str(&format!("GPU init failed: {e:#}")))?;

    let renderer = Renderer::new(&gfx.device, gfx.output.config.format);
    let egui_ctx = egui::Context::default();
    phosphor::theme::apply(&egui_ctx);
    // §9a's lo-res face, and `/play` is the one surface entitled to it: its
    // panel is the P0 skeleton, deliberately ad-hoc, so it is *designed* to a
    // 16-point cell rather than retrofitted onto one. `vidiotic` and
    // Set the theme face to Classic (12pt font) by default on web.
    phosphor::theme::set_state(
        &egui_ctx,
        phosphor::theme::ThemeState {
            face: phosphor::theme::Face::Classic,
            ..phosphor::theme::ThemeState::default()
        },
    );
    egui_ctx.set_pixels_per_point(dpr);
    let egui_rend = egui_wgpu::Renderer::new(
        &gfx.device,
        gfx.control.config.format,
        egui_wgpu::RendererOptions {
            msaa_samples: 1,
            ..Default::default()
        },
    );

    let q: input::Shared = Rc::new(RefCell::new(input::Queue::default()));
    input::attach(&control, &q)?;
    // Keys go on the document, not the canvas: a canvas is not focusable by
    // default, so a canvas-scoped listener would only fire after a click and
    // would go silent again the moment focus moved to the file input.
    let doc = window().document().ok_or_else(|| JsValue::from_str("no document"))?;
    input::attach_keys(doc.as_ref(), &q)?;

    // No BC means the block texture cannot exist on this device, so the CPU
    // path is not a preference here, it is the only one.
    let soft: SoftFlag = Rc::new(Cell::new(!gfx.caps.bc));
    let paused: Flag = Rc::new(Cell::new(false));
    let library: Library = Rc::new(RefCell::new(HashMap::new()));
    let taps: cameras::Taps = Rc::new(RefCell::new(HashMap::new()));
    let mut engine = Engine::new(Boot {
        opener: Box::new(WebSources {
            library: library.clone(),
            soft: soft.clone(),
            paused: paused.clone(),
            taps: Rc::clone(&taps),
        }),
        ..Boot::default()
    });
    // The grammar is the only input model here — there is no MIDI mapper and no
    // menu bar — so it is on from the start rather than behind a toggle.
    engine.grammar_on = true;

    let ms = now_ms();
    SHELL.with(|e| {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded();
        *e.borrow_mut() = Some(Shell {
            gfx,
            renderer,
            egui_ctx,
            egui_rend,
            input: q,
            dpr,
            engine,
            library,
            soft,
            paused,
            last_ms: ms,
            elapsed: 0.0,
            fps: 60.0,
            effect: None,
            status: String::new(),
            camera_devices: Vec::new(),
            taps,
            project_name: "session".to_string(),
            mirror: crate::commands::UiMirror::default(),
            cmd_tx,
            cmd_rx,
            thumbs: HashMap::new(),
            // 48 kHz until a real tap says otherwise. It only sets the band
            // edges and the hop; `push_audio` corrects both the moment the page
            // knows its `AudioContext.sampleRate`.
            analyzer: crate::analysis::Analyzer::new(48000.0),
            audio_live: false,
            last_audio_ms: 0.0,
        });
    });

    spawn_frame_loop();
    log::info!("vidiotic /play booted");
    Ok(())
}

/// Drive the shell from `requestAnimationFrame`.
///
/// The closure holds an `Rc` to itself so it can re-arm; this is the standard
/// shape for a wasm rAF loop and the `Rc` cycle is intentional — the loop is
/// meant to run for the page's lifetime.
fn spawn_frame_loop() {
    let cb = Rc::new(RefCell::new(None::<Closure<dyn FnMut()>>));
    let seed = cb.clone();
    *seed.borrow_mut() = Some(Closure::new(move || {
        SHELL.with(|e| {
            if let Some(shell) = e.borrow_mut().as_mut() {
                shell.frame();
            }
        });
        request_frame(cb.borrow().as_ref().expect("loop closure"));
    }));
    request_frame(seed.borrow().as_ref().expect("loop closure"));
}

/// Schedule the next frame. A rejected `requestAnimationFrame` falls back to a
/// timer rather than panicking: this runs once per frame, the panic hook takes
/// the whole page down with it, and one transient failure is not worth losing a
/// running set over. The timer keeps the loop alive at roughly frame rate even
/// if rAF stays unavailable.
fn request_frame(cb: &Closure<dyn FnMut()>) {
    if window()
        .request_animation_frame(cb.as_ref().unchecked_ref())
        .is_ok()
    {
        return;
    }
    log::warn!("requestAnimationFrame was refused; falling back to a timer for this frame");
    if let Err(e) = window().set_timeout_with_callback_and_timeout_and_arguments_0(
        cb.as_ref().unchecked_ref(),
        16,
    ) {
        log::error!("could not schedule the next frame at all: {e:?}");
    }
}

/// The `detail` values [`request_file`] sends, and the page's `accept` keys.
pub(crate) const PICK_ISF: &str = "isf";
pub(crate) const PICK_SHADER: &str = "shader";

/// Ask the page to put a file chooser in front of the visitor.
///
/// The panels emit `Pick*` because a panel must not know how a shell answers it
/// — natively that is `rfd`, and here it is the page's own `<input type=file>`,
/// which already reads clips. So this dispatches and returns; the answer arrives
/// later through [`load_isf_source`] or [`load_shader_source`], the same shape
/// as [`load_clip`]. Reading files stays in JS, where the page already does it.
///
/// The activation subtlety worth naming: a file chooser needs *transient user
/// activation*, and by the time this runs the visitor's click has already been
/// through egui inside a `requestAnimationFrame` callback rather than the
/// pointer handler. That is fine — transient activation lasts five seconds and
/// is not scoped to the dispatching task — but it is fine by a rule rather than
/// by construction, so `play-smoke.mjs` intercepts the chooser and asserts it
/// actually opens. It is exactly the class of thing that works locally and then
/// does not (web-port.md §8 step 4g).
fn request_file(kind: &str) {
    let init = web_sys::CustomEventInit::new();
    init.set_detail(&JsValue::from_str(kind));
    match web_sys::CustomEvent::new_with_event_init_dict("vidiotic-pick", &init) {
        Ok(ev) => {
            let _ = window().dispatch_event(&ev);
        }
        Err(e) => log::error!("could not ask the page for a {kind} file: {e:?}"),
    }
}

/// Ask the page to enumerate capture devices.
///
/// `navigator.mediaDevices.enumerateDevices()` is async and a panel is drawn
/// synchronously, so this is a request like [`request_file`] is: the answer
/// arrives later through [`set_cameras`].
fn request_cameras() {
    let init = web_sys::CustomEventInit::new();
    init.set_detail(&JsValue::NULL);
    match web_sys::CustomEvent::new_with_event_init_dict("vidiotic-cameras", &init) {
        Ok(ev) => {
            let _ = window().dispatch_event(&ev);
        }
        Err(e) => log::error!("could not ask the page for cameras: {e:?}"),
    }
}

/// Ask the page to start or stop a camera.
///
/// Starting one raises the browser's permission prompt, which is why this is a
/// request and not a call: it is `getUserMedia`, it is async, and it can be
/// refused. The answer arrives through [`camera_ready`] or not at all.
fn request_camera(uid: &str, on: bool) {
    let detail = js_sys::Array::new();
    detail.push(&JsValue::from_str(uid));
    detail.push(&JsValue::from_bool(on));
    let init = web_sys::CustomEventInit::new();
    init.set_detail(&detail);
    match web_sys::CustomEvent::new_with_event_init_dict("vidiotic-camera", &init) {
        Ok(ev) => {
            let _ = window().dispatch_event(&ev);
        }
        Err(e) => log::error!("could not ask the page for camera {uid}: {e:?}"),
    }
}

/// Hand the page a file to save, with the name to save it under.
///
/// The counterpart of [`request_file`], and the same boundary: a download needs
/// an anchor and a `Blob`, both of which are the page's, so the bytes are built
/// here and delivered there. `/chop` does the identical thing for its exports.
fn deliver_file(name: &str, bytes: &[u8]) {
    let detail = js_sys::Array::new();
    detail.push(&JsValue::from_str(name));
    detail.push(&js_sys::Uint8Array::from(bytes));
    let init = web_sys::CustomEventInit::new();
    init.set_detail(&detail);
    match web_sys::CustomEvent::new_with_event_init_dict("vidiotic-save", &init) {
        Ok(ev) => {
            let _ = window().dispatch_event(&ev);
        }
        Err(e) => log::error!("could not hand the page {name}: {e:?}"),
    }
}

/// Borrow the booted shell, or report that there isn't one.
fn with_shell<T>(f: impl FnOnce(&mut Shell) -> Result<T, JsValue>) -> Result<T, JsValue> {
    SHELL.with(|e| {
        let mut slot = e.borrow_mut();
        let shell = slot.as_mut().ok_or_else(|| JsValue::from_str("not booted"))?;
        f(shell)
    })
}

/// Hand the player a baked HAP `.mov`, read by the page into a byte array.
///
/// The clip joins the pool and becomes a full-length cue in the live bank —
/// which means a second call adds a *second* cue and the two now rotate on the
/// phrase grid, rather than the first one being replaced.
///
/// # Errors
/// Returns a JS error if the shell has not booted or the file is not a clip
/// this player can decode.
#[wasm_bindgen]
pub fn load_clip(name: &str, bytes: Vec<u8>) -> Result<(), JsValue> {
    with_shell(|s| {
        s.load_clip(name, bytes).map_err(|msg| {
            s.status.clone_from(&msg);
            log::error!("{msg}");
            JsValue::from_str(&msg)
        })
    })
}

/// Compile an ISF shader, read by the page as text, into the shader pool.
///
/// The answer to a `vidiotic-pick` of kind `isf`. `name` is what the visitor
/// called the file and is the key the chain slot holds, so a project saved
/// elsewhere names the same shader the native player would.
///
/// # Errors
/// Returns a JS error if the shell has not booted, the source carries no ISF
/// header, or the transpiled shader fails to compile.
/// Load a `.viproj`'s text, resolving its clips against the loaded pool.
///
/// # Errors
/// If the RON does not parse, if the format version is newer than this build
/// understands, or if the project names clips the page has not been given —
/// which is reported by name, because "drop these files first" is actionable
/// and "project failed to load" is not.
#[wasm_bindgen]
pub fn load_project(text: &str) -> Result<String, JsValue> {
    with_shell(|s| {
        match s.load_project(text) {
            Ok(summary) => {
                s.status.clone_from(&summary);
                log::info!("{summary}");
                Ok(summary)
            }
            Err(msg) => {
                s.status.clone_from(&msg);
                log::error!("{msg}");
                Err(JsValue::from_str(&msg))
            }
        }
    })
}

/// Name the session, so a save comes back under it.
///
/// The page knows the file name a project was opened from; the engine never
/// sees it, because `load_project` takes text. This is that one string.
#[wasm_bindgen]
pub fn set_project_name(name: &str) -> Result<(), JsValue> {
    with_shell(|s| {
        let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
        if !stem.trim().is_empty() {
            s.project_name = stem.to_string();
        }
        Ok(())
    })
}

/// Put a camera on a cue in the edit bank.
///
/// The panel's own button raises the same `AddCameraCue`, and this goes through
/// the same dispatch — so it is the button, reachable from a script. A host
/// page can offer one, and a smoke test has no other way to press it: the
/// panels are pixels on a canvas.
///
/// # Errors
/// If the shell has not booted.
#[wasm_bindgen]
pub fn add_camera_cue(uid: &str) -> Result<(), JsValue> {
    with_shell(|s| {
        s.dispatch(Command::AddCameraCue(uid.into()));
        Ok(())
    })
}

/// The capture devices the page enumerated, as parallel uid/name lists.
///
/// Two `Vec<String>`s rather than one JSON string because that is all a device
/// is here — `deviceId` and `label` — and parsing JSON to rebuild a pair of
/// strings would be ceremony.
///
/// Labels are empty until the visitor grants access to *some* camera, which is
/// a privacy rule rather than a failure; the page substitutes a positional name
/// and calls this again once a stream is granted and the real labels appear.
///
/// # Errors
/// If the shell has not booted, or the two lists are different lengths.
#[wasm_bindgen]
pub fn set_cameras(uids: Vec<String>, names: Vec<String>) -> Result<(), JsValue> {
    if uids.len() != names.len() {
        return Err(JsValue::from_str("camera uid and name lists differ in length"));
    }
    with_shell(|s| {
        s.camera_devices = uids
            .into_iter()
            .zip(names)
            .map(|(uid, name)| cameras::Device { uid, name })
            .collect();
        log::info!("cameras: {} device(s)", s.camera_devices.len());
        Ok(())
    })
}

/// A camera is live: here is the element playing it.
///
/// The page owns `getUserMedia` and the `<video>` it has to attach the stream
/// to, and must not call this until the element has metadata — `videoWidth` is
/// 0 before then, and a zero-sized draw throws.
///
/// # Errors
/// If the shell has not booted, or this browser cannot make a 2-D canvas to
/// sample the element through.
#[wasm_bindgen]
pub fn camera_ready(uid: &str, video: web_sys::HtmlVideoElement) -> Result<(), JsValue> {
    with_shell(|s| {
        let tap = cameras::Tap::new(video).map_err(|e| JsValue::from_str(&e))?;
        s.taps.borrow_mut().insert(uid.to_string(), tap);
        // Any cue already on this device is holding a `None` opener from when
        // it was off air. Dropping it makes the next tick re-open it against
        // the tap that now exists, so switching a camera on lights its cue
        // without waiting for the rotation to come round again.
        let stale: Vec<crate::bank::CueId> = s
            .engine
            .decoders
            .keys()
            .copied()
            .filter(|&id| {
                s.engine
                    .live_cue(id)
                    .is_some_and(|c| s.engine.clip_camera_uid(c.clip).as_deref() == Some(uid))
            })
            .collect();
        for id in stale {
            s.engine.decoders.remove(&id);
        }
        s.status = format!("camera {uid} is on air");
        log::info!("{}", s.status);
        Ok(())
    })
}

/// A camera stopped, or never started. Drops its tap.
///
/// Called by the page when the visitor switches one off, and when
/// `getUserMedia` is refused — the second is why `reason` exists: a camera that
/// simply does not appear is indistinguishable from a broken button.
///
/// # Errors
/// If the shell has not booted.
#[wasm_bindgen]
pub fn camera_stopped(uid: &str, reason: Option<String>) -> Result<(), JsValue> {
    with_shell(|s| {
        s.taps.borrow_mut().remove(uid);
        if let Some(why) = reason {
            s.status = format!("camera {uid}: {why}");
            log::error!("{}", s.status);
        }
        Ok(())
    })
}

/// Save the running session as a `.zip` bundle, handed back through the page.
///
/// Exported as well as reachable from the panels because a host page may want
/// its own save button, and because a bundle is bytes with no pixels — a smoke
/// test has no other way to see one.
///
/// # Errors
/// If the shell has not booted.
#[wasm_bindgen]
pub fn save_project() -> Result<(), JsValue> {
    with_shell(|s| {
        s.save_project(None);
        Ok(())
    })
}

#[wasm_bindgen]
pub fn load_isf_source(name: &str, src: &str) -> Result<(), JsValue> {
    with_shell(|s| {
        s.load_isf_source(name, src).map_err(|msg| {
            s.status.clone_from(&msg);
            log::error!("{msg}");
            JsValue::from_str(&msg)
        })
    })
}

/// Replace the global shader with WGSL read by the page.
///
/// The answer to a `vidiotic-pick` of kind `shader`.
///
/// # Errors
/// Returns a JS error if the shell has not booted or the WGSL fails to compile.
/// A failed compile leaves the previous shader running rather than blanking the
/// output — the same thing the native shader watcher does on a bad save.
#[wasm_bindgen]
pub fn load_shader_source(name: &str, src: &str) -> Result<(), JsValue> {
    with_shell(|s| {
        s.load_shader_source(name, src).map_err(|msg| {
            s.status.clone_from(&msg);
            log::error!("{msg}");
            JsValue::from_str(&msg)
        })
    })
}

/// Select a built-in effect by index, or `None` for a bare passthrough.
///
/// The panel offers the same choice; this exists so the smoke test can exercise
/// it without synthesising clicks. Worth testing: an empty chain takes
/// `render`'s single-pass fast path, and anything else takes the seed +
/// ping-pong path, so these are two genuinely different code paths through the
/// compositor and only one of them is proven by a clip appearing at all.
///
/// # Errors
/// Returns a JS error if the shell has not booted or the index is out of range.
#[wasm_bindgen]
pub fn set_effect(index: Option<usize>) -> Result<(), JsValue> {
    with_shell(|s| {
        if let Some(i) = index {
            if i >= crate::render::BUILTIN_EFFECTS.len() {
                return Err(JsValue::from_str(&format!("no effect {i}")));
            }
        }
        s.set_effect(index);
        Ok(())
    })
}

/// Force (or release) CPU block decompression.
///
/// Turning it *on* is the interesting direction: on a device with BC the
/// fallback would otherwise never run anywhere it could be observed, so the
/// host page exposes it as `?soft=1` and the smoke test drives it. Turning it
/// off on a device without BC is refused — there is no block texture to be had
/// there, and honouring it would just paint black.
///
/// # Errors
/// Returns a JS error if the shell has not booted, or if BC is being asked for
/// on a device that does not have it.
#[wasm_bindgen]
pub fn set_soft_decode(on: bool) -> Result<(), JsValue> {
    with_shell(|s| {
        if !on && !s.gfx.caps.bc {
            return Err(JsValue::from_str(
                "this GPU has no texture-compression-bc; software decode cannot be turned off",
            ));
        }
        // Open sources read the flag every poll and re-decode when it changes,
        // so the switch takes effect on the next frame rather than whenever the
        // clip next happens to advance a sample.
        s.soft.set(on);
        Ok(())
    })
}

/// Set the tempo directly, clamped to the clock's range.
///
/// The grammar and the flat keys move the tempo relative to where it is, which
/// is right for playing and useless for restoring: a session reloaded at 128 bpm
/// has to be *told* 128, not tapped back to it.
///
/// # Errors
/// Returns a JS error if the shell has not booted.
#[wasm_bindgen]
pub fn set_bpm(bpm: f64) -> Result<(), JsValue> {
    with_shell(|s| {
        s.engine.clock.set_bpm(bpm);
        Ok(())
    })
}

/// Feed the analyser mono samples at `sample_rate` Hz.
///
/// This is the browser's whole audio input. Natively the equivalent is a cpal
/// device filling a lock-free ring that an analysis thread drains; here the page
/// taps a `MediaStream` — a microphone, a shared tab, system audio — and posts
/// quanta in. Everything downstream of this call is the same code on both sides
/// (`analysis::Analyzer`), which is what stops an audio-reactive shader looking
/// different on the desktop from how it looks on the web.
///
/// The rate is passed every call rather than configured once because it is the
/// page's to know and it can change under the page's feet: an `AudioContext`
/// built before a device switch reports the old rate, and a wrong rate does not
/// fail, it just puts every band edge in the wrong place. A change resets the
/// smoothing, so this is cheap to pass and not free to get wrong.
///
/// Samples are mono. A stereo tap should down-mix on the page, where the
/// channel layout is known.
///
/// # Errors
/// Returns a JS error if the shell has not booted.
#[wasm_bindgen]
pub fn push_audio(samples: &[f32], sample_rate: f32) -> Result<(), JsValue> {
    with_shell(|s| {
        if sample_rate.is_finite()
            && sample_rate > 0.0
            && (sample_rate - s.analyzer.sample_rate()).abs() > 0.5
        {
            s.analyzer.set_sample_rate(sample_rate);
        }
        s.analyzer.feed(samples);
        s.audio_live = true;
        s.last_audio_ms = now_ms();
        Ok(())
    })
}

/// The stable names of the built-in effects, in the order [`set_effect`] indexes.
#[wasm_bindgen]
#[must_use]
pub fn effect_names() -> Vec<String> {
    crate::render::BUILTIN_EFFECTS
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .collect()
}

/// The session's state, as a JSON string.
///
/// For the smoke test, and worth the surface for the same reason
/// [`centre_pixel`] is: the beat grid, the modal grammar and the cue rotation
/// have no pixels of their own on the output head, so nothing about them is
/// observable from a screenshot. Driving a key and reading a changed tempo — or
/// a changed *cue* — back is the only way to show that the engine crossed to the
/// browser rather than merely linked.
///
/// # Errors
/// Returns a JS error if the shell has not booted.
#[wasm_bindgen]
pub fn engine_state() -> Result<String, JsValue> {
    with_shell(|s| {
        let snap = s.engine.clock.snapshot();
        let g = s.grammar_view();
        let opts: Vec<&str> = g.options.iter().map(|(k, _)| *k).collect();
        let pending = g
            .pending
            .map_or_else(|| "null".to_owned(), |p| format!("\"{p}\""));
        let last_verb = g
            .last_verb
            .map_or_else(|| "null".to_owned(), |v| format!("\"{v}\""));
        let effect = s.effect.map_or_else(|| "null".to_owned(), |i| i.to_string());
        // `clip` is what tells a restored session apart from a fresh one: the
        // page has no other way to ask whether the bytes it pulled out of OPFS
        // actually became a playing clip.
        //
        // Every other string here comes from a fixed table; this one is a
        // filename, so it is the only field that can carry a quote or a
        // backslash and turn the readout into a parse error.
        let library = s.library.borrow();
        let playing_clip = s
            .engine
            .current
            .and_then(|c| s.engine.live_cue(c))
            .map(|cue| cue.clip);
        let clip = match playing_clip.and_then(|id| {
            let pool = s.engine.clips.iter().find(|c| c.id == id)?;
            Some((pool.name.clone(), library.get(&id).map_or(0, |l| l.frames)))
        }) {
            Some((name, frames)) => {
                format!(r#"{{"name":"{}","frames":{frames}}}"#, json_escape(&name))
            }
            None => "null".to_owned(),
        };
        let current = s
            .engine
            .current
            .map_or_else(|| "null".to_owned(), |c| c.to_string());
        // The armed cue as well as the playing one: a rotation that never arms
        // and a rotation that arms but never fires are different faults, and
        // `current` alone cannot tell them apart.
        let armed = s
            .engine
            .sequencer
            .armed()
            .map_or_else(|| "null".to_owned(), |c| c.to_string());
        // The non-built-in half of the shader pool, by name. A shader the page
        // handed us either compiled and joined the pool or it did not, and
        // nothing else the page can see distinguishes those — the output looks
        // the same until something puts the shader on a cue.
        let shaders = s
            .renderer
            .pool_view()
            .iter()
            .filter(|p| !p.builtin)
            .map(|p| format!(r#""{}""#, json_escape(&p.name)))
            .collect::<Vec<_>>()
            .join(",");
        // The pool by name, not just its size. A restored session's whole claim
        // is that *these* clips came back — a count cannot tell one clip
        // restored twice from two clips restored once, and the by-name join a
        // `.viproj` resolves through is exactly what a lossy store would break.
        let clips = s
            .engine
            .clips
            .iter()
            .map(|c| format!(r#""{}""#, json_escape(&c.name)))
            .collect::<Vec<_>>()
            .join(",");
        // The camera rows, as the panel has them. Not drawn from `engine.clips`
        // like the pool above: a device is not a session concept, so whether
        // one is *on air* and what size it is arriving at exist only here.
        let cams = s
            .mirror
            .cameras
            .iter()
            .map(|c| {
                format!(
                    r#"{{"uid":"{}","name":"{}","on_air":{},"status":"{}","missing":{}}}"#,
                    json_escape(&c.uid),
                    json_escape(&c.name),
                    c.on_air,
                    json_escape(&c.status),
                    c.missing
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        // `face`/`cell` so the smoke test can check the §9a grid is the one
        // actually painting, rather than trusting that a `set_state` took.
        let m = phosphor::theme::metrics();
        Ok(format!(
            r#"{{"bpm":{},"beat":{},"pane":"{}","pending":{pending},"options":{},"last_verb":{last_verb},"effect":{effect},"playing":{},"bc":{},"soft":{},"clip":{clip},"cues":{},"banks":{},"pool":{},"clips":[{clips}],"cameras":[{cams}],"active":{},"current":{current},"armed":{armed},"lvl":{},"audio":{},"shaders":[{shaders}],"face":"{:?}","cell":{}}}"#,
            snap.bpm,
            snap.beat,
            g.pane,
            opts.len(),
            s.playing(),
            s.gfx.caps.bc,
            s.soft.get(),
            s.engine.banks[s.engine.live_bank].cues.len(),
            s.engine.banks.len(),
            s.pool_ids().len(),
            s.engine.sequencer.active_len(),
            s.analyzer.frame().level,
            s.audio_live,
            phosphor::theme::state(&s.egui_ctx).face,
            m.cell,
        ))
    })
}

/// The JSON string escapes, for the one field of [`engine_state`] that is not
/// drawn from a fixed table. Not a JSON library — this covers the two
/// characters that break a string literal plus the control range, which is the
/// whole of what a filename can contain that matters here.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Resize a head's swapchain after its canvas changed size.
///
/// # Errors
/// Returns a JS error if the shell has not booted.
#[wasm_bindgen]
pub fn resize(head: &str, width: u32, height: u32) -> Result<(), JsValue> {
    with_shell(|s| {
        let device = &s.gfx.device;
        match head {
            "output" => s.gfx.output.resize(device, width, height),
            "control" => s.gfx.control.resize(device, width, height),
            other => return Err(JsValue::from_str(&format!("no such head: {other}"))),
        }
        Ok(())
    })
}

/// Read back the centre pixel of a head, as `[r, g, b, a]`.
///
/// Exists for the smoke test, and is worth the surface it costs: "no exception
/// was thrown" is not evidence that anything rendered, and a crossed or blank
/// canvas is otherwise indistinguishable from a working one without a human
/// looking at it (web-port.md §10a).
///
/// # Errors
/// Returns a JS error if the shell has not booted or the head is unknown.
///
/// # Panics
/// Panics if the readback buffer cannot be mapped.
#[wasm_bindgen]
pub async fn centre_pixel(head: String) -> Result<Vec<u8>, JsValue> {
    // Re-render into an offscreen copy rather than reading the swapchain: a
    // surface texture is not COPY_SRC, and asking for one that is would change
    // the configuration the rest of the port is measuring.
    let (device, queue, texture, format, w, h) = with_shell(|shell| {
        let cfg = match head.as_str() {
            "output" => &shell.gfx.output.config,
            "control" => &shell.gfx.control.config,
            other => return Err(JsValue::from_str(&format!("no such head: {other}"))),
        };
        let (w, h, format) = (cfg.width, cfg.height, cfg.format);
        let texture = shell.gfx.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("readback"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = shell.gfx.device.create_command_encoder(&Default::default());
        if head == "output" {
            shell
                .renderer
                .render(&shell.gfx.device, &shell.gfx.queue, &mut enc, &view, w, h);
        } else {
            // The control head's content is egui; clearing to the panel colour
            // is enough to prove the surface is live and correctly formatted.
            let _ = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("readback-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color()),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        shell.gfx.queue.submit([enc.finish()]);
        Ok((
            shell.gfx.device.clone(),
            shell.gfx.queue.clone(),
            texture,
            format,
            w,
            h,
        ))
    })?;

    // `copyTextureToBuffer` needs 256-byte-aligned rows; copy a single 64-px
    // row, which at 4 bytes per pixel is exactly 256.
    const ROW: u32 = 64;
    let (cx, cy) = (w / 2, h / 2);
    let x = cx.saturating_sub(ROW / 2).min(w.saturating_sub(ROW));
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback-buf"),
        size: u64::from(ROW) * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&Default::default());
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y: cy, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(ROW * 4),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: ROW,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([enc.finish()]);

    let (tx, rx) = futures_channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        tx(r.is_ok());
    });
    device.poll(wgpu::PollType::Poll).ok();
    if !rx.await {
        return Err(JsValue::from_str("readback map failed"));
    }
    let data = buffer.slice(..).get_mapped_range();
    let mid = (ROW as usize / 2) * 4;
    let mut px = data[mid..mid + 4].to_vec();
    drop(data);
    buffer.unmap();

    // Report RGBA regardless of the surface's channel order, so a caller does
    // not have to know which format the browser handed us.
    if format == wgpu::TextureFormat::Bgra8Unorm || format == wgpu::TextureFormat::Bgra8UnormSrgb {
        px.swap(0, 2);
    }
    Ok(px)
}

/// A minimal oneshot, so this module does not take a futures dependency for
/// exactly one await.
///
/// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>` because `map_async`'s callback
/// is required to be `Send`. On wasm that bound is vacuous — there is one
/// thread and the lock is never contended — but it is the signature, and
/// satisfying it honestly is cheaper than reasoning about a `Send` shim.
fn futures_channel() -> (
    impl FnOnce(bool) + Send + 'static,
    impl std::future::Future<Output = bool>,
) {
    use std::sync::{Arc, Mutex};

    let cell: Arc<Mutex<(Option<bool>, Option<std::task::Waker>)>> =
        Arc::new(Mutex::new((None, None)));
    let set = cell.clone();
    let send = move |v: bool| {
        let mut c = set.lock().expect("oneshot poisoned");
        c.0 = Some(v);
        if let Some(w) = c.1.take() {
            w.wake();
        }
    };
    let fut = std::future::poll_fn(move |cx| {
        let mut c = cell.lock().expect("oneshot poisoned");
        if let Some(v) = c.0 {
            return std::task::Poll::Ready(v);
        }
        c.1 = Some(cx.waker().clone());
        std::task::Poll::Pending
    });
    (send, fut)
}
