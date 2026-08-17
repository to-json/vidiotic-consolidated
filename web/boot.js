// The /play boot script.
//
// This is a separate file rather than a <script type="module"> in index.html
// for one reason, and it is not taste: fubarchitect.com serves every page under
// `script-src 'self' 'wasm-unsafe-eval'`, with no 'unsafe-inline'. Inline, none
// of this runs — the page renders its header and its paragraph and then simply
// sits there, which is exactly what it did in the staging container before this
// file existed. The site's own convention says the same thing ("No inline
// scripts — CSP forbids them. Always use external .js files").
//
// It is copied *into the hashed bundle directory* by scripts/release-web.sh,
// alongside the wasm and wasm-bindgen's glue, and the directory hash is taken
// over all three. That placement does two things:
//
//   - the import below stays a plain sibling reference in the released
//     artifact, so it needs no rewriting there;
//   - boot, glue and module can never be served as a mismatched set, because
//     changing any of them changes the directory name.
//
// Served straight out of web/ during development the layout is different — the
// bundle is in ./pkg/ — which is why the import path is one of the substitutions
// release-play.sh asserts on.

import init, {
  boot, load_clip, resize, centre_pixel, set_effect, effect_names,
  engine_state, set_soft_decode, set_bpm, push_audio,
  load_isf_source, load_shader_source,
  Baker, is_baked, bake_size, load_project, save_project, set_project_name,
  set_cameras, camera_ready, camera_stopped, add_camera_cue,
} from './pkg/vidiotic_play.js';

// Replaced by scripts/release-play.sh with a commit-and-date stamp. A page
// served straight out of web/ has nothing to stamp, and "dev" is the honest
// answer — the point of this is being able to tell, from a console in front of
// a projector, which build is actually on the screen.
const BUILD = 'dev';
console.info(`vidiotic /play — build ${BUILD}`);

const $ = (id) => document.getElementById(id);
const status = (msg, isErr = false) => {
  $('status').textContent = msg;
  $('status').classList.toggle('err', isErr);
};

const params = new URLSearchParams(location.search);

/**
 * Refuse to start, and say why.
 *
 * A page that throws `Cannot read properties of undefined (reading
 * 'requestAdapter')` into a console nobody has open is indistinguishable from a
 * broken link. WebGPU is still absent or flagged off in enough places that this
 * is the single most likely thing to happen to a first-time visitor, so it gets
 * a real message naming the actual cause.
 */
function whyNotWebGPU() {
  if (!window.isSecureContext) {
    return {
      title: 'This page needs a secure context.',
      lines: [
        'WebGPU is only exposed over https:// or on http://localhost.',
        `This page was served from ${location.origin || 'a file:// URL'}.`,
      ],
    };
  }
  if (!navigator.gpu) {
    return {
      title: 'This browser has no WebGPU.',
      lines: [
        '/play renders through WebGPU; there is no WebGL fallback.',
        'Chrome or Edge 113+, or Firefox 141+ on Windows.',
        'Safari 26+ has it; earlier Safari needs Develop → Feature Flags → WebGPU.',
        'On Linux, Chrome may still need --enable-unsafe-webgpu.',
      ],
    };
  }
  return null;
}

// Size a canvas's backing store in device pixels while CSS lays it out in CSS
// pixels. egui is told the same device-pixel-ratio, so a point means the same
// thing on both sides and text lands on pixel boundaries.
function fit(canvas, cssW, cssH) {
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(cssW * dpr));
  const h = Math.max(1, Math.round(cssH * dpr));
  const changed = canvas.width !== w || canvas.height !== h;
  canvas.width = w; canvas.height = h;
  return changed ? [w, h] : null;
}

/**
 * Open the output window and return its canvas.
 *
 * Must be called synchronously from a click handler or the popup blocker eats
 * it. `about:blank` from window.open() inherits this origin, which is what lets
 * one GPUDevice drive a canvas inside it (web-port.md §10a).
 */
function openOutputHead() {
  const win = window.open('', 'vidiotic-output', 'width=1280,height=720');
  if (!win) throw new Error('popup blocked — allow popups for this origin and retry');
  win.document.write(`<!doctype html><meta charset="utf-8"><title>vidiotic /play — output</title>
    <style>html,body{margin:0;background:#000;overflow:hidden}canvas{display:block;width:100vw;height:100vh}</style>
    <canvas id="out"></canvas>`);
  win.document.close();
  const canvas = win.document.getElementById('out');
  fit(canvas, win.innerWidth, win.innerHeight);
  // A projector feed is meant to be clean: no UI, no chrome, no context menu.
  win.document.addEventListener('contextmenu', (e) => e.preventDefault());
  return { win, canvas };
}

/**
 * Let a cross-realm GPUCanvasContext satisfy `instanceof GPUCanvasContext`.
 *
 * The output head's canvas lives in the window we opened, so its context is an
 * instance of *that* window's GPUCanvasContext — a different constructor object
 * from this window's. Measured: `ctx instanceof GPUCanvasContext` is false
 * across the realm boundary while `configure({device})` on the very same object
 * is accepted, which is the combination that makes this so easy to miss.
 *
 * WebGPU itself is fine with it (web-port.md §10a measured exactly that). What
 * is not fine is wgpu's binding: `create_surface` does a `dyn_into` on the
 * context, and wasm-bindgen implements that as an `instanceof` against this
 * realm's constructor, so it panics before any WebGPU call happens
 * (wgpu-29.0.4/src/backend/webgpu.rs:1126).
 *
 * Relaxing the check to a structural one is the smallest fix that keeps the
 * one-device architecture. It is scoped to this page, and the property it
 * loosens is precisely the realm identity we do not want enforced.
 */
function allowCrossRealmCanvasContext() {
  if (typeof GPUCanvasContext === 'undefined') return;
  const native = Object.getOwnPropertyDescriptor(GPUCanvasContext, Symbol.hasInstance);
  Object.defineProperty(GPUCanvasContext, Symbol.hasInstance, {
    configurable: true,
    value(o) {
      if (native?.value?.call(this, o)) return true;
      return o != null && typeof o === 'object'
        && typeof o.configure === 'function'
        && typeof o.getCurrentTexture === 'function';
    },
  });
}

// --- persistence ----------------------------------------------------------
//
// OPFS for the clips, localStorage for the session. Two stores because they
// have two shapes: a clip is megabytes of opaque bytes with a write API built
// for exactly that, and the session is a handful of numbers that have to
// survive being read by a human in devtools.
//
// **A directory, not a file.** This held one `clip.mov` until a `.viproj`
// could name several. A project references clips by name and `PoolFs` resolves
// them by name, so the honest store is a directory whose entry names *are* the
// pool names — then restoring a session is listing it, and a project that
// loaded yesterday loads today. The old single file is migrated in on first
// sight rather than dropped: somebody's clip surviving a deploy is the whole
// point of storing it.
//
// Both stores are best-effort. Private-browsing modes deny one or both, quota
// is not guaranteed, and none of it is worth failing a boot over — a session
// with no storage is the session this page had last week. So every path here
// reports and continues.

const CLIPS_DIR = 'clips';
const PROJECT_FILE = 'session.viproj';
const LEGACY_CLIP_FILE = 'clip.mov';
const NAME_KEY = 'vidiotic.play.clipName';
const SESSION_KEY = 'vidiotic.play.session';

const opfs = () => navigator.storage?.getDirectory?.();

/**
 * Ask the browser to stop counting this origin as disposable.
 *
 * OPFS is best-effort by default: under storage pressure a browser may evict
 * the whole origin without telling anyone, and a clip that silently is not
 * there on Saturday night is worse than one that was never stored. `persist()`
 * moves the origin to the "persistent" bucket, which browsers clear only on an
 * explicit user action.
 *
 * Asked once, and only after a clip has actually been written — Firefox raises
 * a permission prompt for this, and prompting before there is anything to
 * protect asks a question the visitor has no way to answer. Chrome decides
 * silently from site engagement and may well say no; that is not a failure,
 * the clip is stored either way, just evictable.
 */
async function askToPersist() {
  try {
    if (!navigator.storage?.persist) return false;
    if (await navigator.storage.persisted()) return true;
    return await navigator.storage.persist();
  } catch {
    return false;               // denied, or the API is absent behind a flag
  }
}

/**
 * A clip's entry name in the store, and back again.
 *
 * The pool name is whatever the visitor's file was called, and OPFS rejects `/`
 * in an entry name — but replacing the offending characters is not an option:
 * the entry name *is* the pool name on the way back, and a project resolves its
 * clips by that name. A lossy rename would restore a pool that no `.viproj`
 * can match, which looks exactly like the clips having gone missing.
 *
 * So percent-encoding, which is reversible. `decodeURIComponent` throws on a
 * stray `%`, which a clip called `50%.mov` from an older build could carry;
 * that is a name to hand back unchanged, not a reason to hide the clip.
 */
const entryName = (name) => encodeURIComponent(name);
function poolName(entry) {
  try {
    return decodeURIComponent(entry);
  } catch {
    return entry;
  }
}

const CLIPS = { dir: null };

/** The clips directory, created on demand. Null when there is no OPFS at all. */
async function clipsDir(create = false) {
  if (CLIPS.dir) return CLIPS.dir;
  const root = await opfs();
  if (!root) return null;
  try {
    CLIPS.dir = await root.getDirectoryHandle(CLIPS_DIR, { create });
  } catch {
    return null;          // NotFoundError with create:false — nothing stored
  }
  return CLIPS.dir;
}

/**
 * The one `clip.mov` a previous build stored, moved into the directory.
 *
 * Runs once: after the move there is no legacy file to find. Silent on every
 * failure, because the fallback — the clip simply is not restored — is the
 * behaviour of a first visit rather than an error.
 */
async function migrateLegacyClip() {
  const root = await opfs();
  if (!root) return;
  let file;
  try {
    file = await (await root.getFileHandle(LEGACY_CLIP_FILE)).getFile();
  } catch {
    return;                                   // nothing from the old build
  }
  try {
    if (file.size) {
      const name = localStorage.getItem(NAME_KEY) ?? LEGACY_CLIP_FILE;
      await storeClip(name, new Uint8Array(await file.arrayBuffer()));
      console.info(`moved ${name} into ${CLIPS_DIR}/`);
    }
    await root.removeEntry(LEGACY_CLIP_FILE).catch(() => {});
    localStorage.removeItem(NAME_KEY);
  } catch (e) {
    console.warn('could not migrate the previously stored clip', e);
  }
}

async function storeClip(name, bytes) {
  const dir = await clipsDir(true);
  if (!dir) return false;
  const fh = await dir.getFileHandle(entryName(name), { create: true });
  const w = await fh.createWritable();
  await w.write(bytes);
  await w.close();
  return true;
}

/** Entry names in the clips directory, sorted, so a restore is deterministic. */
async function clipNames() {
  const dir = await clipsDir();
  if (!dir) return [];
  const names = [];
  // `keys()` is the async iterator every OPFS implementation shipped first;
  // older Safari has the handle without it, and there the store is simply
  // unreadable rather than broken.
  if (typeof dir.keys !== 'function') return [];
  for await (const name of dir.keys()) names.push(name);
  return names.sort();
}

/**
 * Every stored clip, in name order.
 *
 * Order matters more than it looks: clip ids are assigned as the pool interns
 * them, so a stable order means a stored bank index still points at the clip it
 * pointed at yesterday.
 */
async function storedClips() {
  await migrateLegacyClip();
  const dir = await clipsDir();
  if (!dir) return [];
  const out = [];
  for (const entry of await clipNames()) {
    try {
      const file = await (await dir.getFileHandle(entry)).getFile();
      if (file.size) {
        out.push({ name: poolName(entry), bytes: new Uint8Array(await file.arrayBuffer()) });
      }
    } catch (e) {
      console.warn(`could not read stored clip ${entry}`, e);
    }
  }
  return out;
}

async function forgetClips() {
  const root = await opfs();
  await root?.removeEntry(CLIPS_DIR, { recursive: true }).catch(() => {});
  await root?.removeEntry(LEGACY_CLIP_FILE).catch(() => {});
  await root?.removeEntry(PROJECT_FILE).catch(() => {});
  localStorage.removeItem(NAME_KEY);
  CLIPS.dir = null;
}

/**
 * The project text, kept beside the clips it names.
 *
 * In OPFS rather than localStorage for the reason `/chop` puts its sidecar
 * there: it grows with the session, and localStorage is a few megabytes per
 * origin shared with everything else. Stored whenever one loads, so a reload
 * brings back the cues and banks and not just the pool — clips without their
 * project is a rack of clips and no set.
 */
async function storeProject(text) {
  const root = await opfs();
  if (!root) return false;
  const fh = await root.getFileHandle(PROJECT_FILE, { create: true });
  const w = await fh.createWritable();
  await w.write(new Blob([text]));
  await w.close();
  return true;
}

async function storedProject() {
  const root = await opfs();
  if (!root) return null;
  try {
    const file = await (await root.getFileHandle(PROJECT_FILE)).getFile();
    return file.size ? await file.text() : null;
  } catch {
    return null;              // NotFoundError — no project this session
  }
}

const saveSession = (s) => localStorage.setItem(SESSION_KEY, JSON.stringify(s));
function loadSession() {
  try { return JSON.parse(localStorage.getItem(SESSION_KEY)) ?? {}; } catch { return {}; }
}

// --- boot -----------------------------------------------------------------

let output = null;
let booted = false;

// --- audio ----------------------------------------------------------------
//
// Several of the bundled effects read `lvl` and `fftBand`, and until now every
// one of them was reacting to a hardcoded zero — running, but sitting still.
//
// Only the *tap* is here. The window, the FFT, the band edges and the smoothing
// are `analysis::Analyzer`, which is the same code the desktop player's
// analysis thread runs; the page's whole job is to produce mono f32 samples and
// say what rate they are at. That split is why an audio-reactive shader cannot
// look different on the two platforms.
//
// An AudioWorklet rather than a ScriptProcessor: the latter is deprecated and
// runs its callback on the main thread, which is the thread already doing BC1
// compression and rAF. The worklet source is a Blob URL because this page has
// no build step and refuses to grow one for fifteen lines.

const TAP_WORKLET = `
class Tap extends AudioWorkletProcessor {
  constructor() { super(); this.buf = new Float32Array(2048); this.n = 0; }
  process(inputs) {
    const chans = inputs[0];
    if (!chans || !chans.length) return true;
    const n = chans[0].length;
    for (let i = 0; i < n; i++) {
      // Down-mix here, where the channel count is known. The analyser is mono
      // by construction and guessing on the other side would be worse.
      let sum = 0;
      for (const c of chans) sum += c[i];
      this.buf[this.n++] = sum / chans.length;
      if (this.n === this.buf.length) {
        this.port.postMessage(this.buf.slice(0));   // copy: the buffer is reused
        this.n = 0;
      }
    }
    return true;
  }
}
registerProcessor('vidiotic-tap', Tap);
`;

let audioCtx = null;

/**
 * Start listening, and return what we got.
 *
 * Tab or system audio first (`getDisplayMedia`), microphone as the fallback.
 * That order is deliberate: the point is reacting to the music being played,
 * and a microphone picks up the room. Both need a user gesture and both show a
 * consent prompt, which is why this hangs off a button rather than running at
 * boot.
 */
async function listen() {
  let stream;
  try {
    // `video: true` because Chrome refuses a display capture with audio alone;
    // the video track is stopped immediately below and never read.
    stream = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: true });
    if (!stream.getAudioTracks().length) {
      stream.getTracks().forEach((t) => t.stop());
      throw new Error('no audio was shared \u2014 pick a tab and tick "Share tab audio"');
    }
  } catch (e) {
    if (e.name === 'NotAllowedError') throw new Error('audio capture was declined');
    console.info('display capture unavailable, falling back to the microphone:', e.message ?? e);
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  }
  // The video track is dead weight — a whole screen's worth of frames nobody
  // reads. Stopping it also drops the "sharing" indicator's video half.
  stream.getVideoTracks().forEach((t) => t.stop());

  audioCtx?.close();
  audioCtx = new AudioContext();
  const url = URL.createObjectURL(new Blob([TAP_WORKLET], { type: 'text/javascript' }));
  try {
    await audioCtx.audioWorklet.addModule(url);
  } finally {
    URL.revokeObjectURL(url);
  }
  const node = new AudioWorkletNode(audioCtx, 'vidiotic-tap');
  // Rate is read per message rather than captured once: an AudioContext built
  // before a device switch keeps reporting the rate it was created at, and a
  // wrong rate does not fail, it silently puts every band edge in the wrong
  // place.
  node.port.onmessage = (e) => push_audio(e.data, audioCtx.sampleRate);
  audioCtx.createMediaStreamSource(stream).connect(node);
  // Connected to the destination but never heard: a worklet with no downstream
  // is not guaranteed to be pulled. It emits nothing, so this adds no sound.
  node.connect(audioCtx.destination);

  const [track] = stream.getAudioTracks();
  track.addEventListener('ended', () => {
    $('audio').textContent = 'Listen';
    $('audio').disabled = false;
    status('audio sharing ended');
  });
  return track.label || 'audio';
}

// --- ingest ---------------------------------------------------------------
//
// A visitor arrives with an mp4, and the player reads Hap1. Bridging that is
// the page's job for the same reason reading the file is: the decode is a
// browser API. The compressor and the muxer are Rust, and they are the *same*
// Rust the desktop baker runs, so what happens here is transport, not a second
// implementation.
//
// Frames come from a <video> element rather than a demuxer + WebCodecs
// VideoDecoder. That is one fewer dependency (a JS mp4 demuxer) and one fewer
// codec matrix to get wrong: whatever the element can play, this can bake,
// which is exactly the promise to make to somebody dropping a file.

const TIER_NARROW = params.get('tier') === 'narrow';
const BAKE_HIGH = params.get('quality') === 'high';
// Frames sampled per second of source. See bakeToHap for why ingest is
// constant-rate; `?fps=` is the escape hatch for a source worth preserving at
// 60, or a slideshow worth baking at 12.
const BAKE_FPS = Math.min(Math.max(Number(params.get('fps')) || 30, 1), 60);
// A seek that never lands. Generous, because the first seek into a fresh
// decoder is much slower than the ones after it.
const SEEK_TIMEOUT_MS = 15_000;

const nextEvent = (el, name) =>
  new Promise((resolve) => el.addEventListener(name, resolve, { once: true }));

/**
 * Block until this document is visible.
 *
 * Chrome does not load a media element in a hidden document: readyState stays
 * 0, networkState sits at NETWORK_LOADING, no `error` fires, and nothing is
 * ever buffered. Measured, with the tab backgrounded by the output head's
 * popup — the failure has no timeout and no event, so without this a bake that
 * loses the foreground simply never ends.
 *
 * Waiting is the honest answer rather than a workaround, because there is
 * nothing to work around: the browser has decided not to decode video for a
 * page nobody is looking at, which is a reasonable thing for it to decide. The
 * bake resumes exactly where it stopped when the visitor comes back.
 *
 * (The way out of *needing* this is WebCodecs, which has no such tie to
 * document visibility — but it needs a demuxer, which this page deliberately
 * does not have. See web-port.md §3.)
 */
async function whileVisible(note) {
  if (document.visibilityState === 'visible') return false;
  note();
  while (document.visibilityState !== 'visible') {
    await nextEvent(document, 'visibilitychange');
  }
  return true;
}

/**
 * Decode a source video and bake it to a HAP `.mov`. Returns the bytes.
 *
 * **Seek-stepping, not playback.** The obvious implementation plays the clip
 * and captures each frame from `requestVideoFrameCallback`, and it is better in
 * every way but one: rVFC is driven by the document's rendering steps, and
 * Chrome additionally pauses muted video-only media in a hidden document. So
 * the moment the visitor switches tabs — which they will, because a bake takes
 * as long as the clip — playback stops and the callback stops firing, and the
 * bake silently produces a truncated clip or none at all. Measured, not
 * predicted: that is exactly what the first version of this did.
 *
 * Setting `currentTime` and waiting for `seeked` has none of that. It runs at
 * whatever rate the decoder manages, in a hidden tab, with no autoplay policy
 * in the way, and it is deterministic — the same file bakes to the same bytes
 * every time, which a realtime capture cannot promise.
 *
 * The cost is that ingest is constant-rate: a variable-rate source is resampled
 * to `BAKE_FPS`. That is the same trade §3a already makes for resolution — the
 * player wants one predictable tier, not the source's own shape — and a VJ loop
 * retriggered on a beat grid has no use for the original's frame timing.
 */
async function bakeToHap(bytes, name, onProgress) {
  const url = URL.createObjectURL(new Blob([bytes]));
  const video = document.createElement('video');
  video.muted = true;
  video.playsInline = true;
  video.preload = 'auto';
  video.src = url;

  try {
    const held = () => onProgress(0, 0, 'waiting — bring this tab to the front to bake');
    await whileVisible(held);
    await Promise.race([
      nextEvent(video, 'loadeddata'),
      nextEvent(video, 'error').then(() => {
        throw new Error(`${name}: this browser cannot decode it`);
      }),
    ]);

    const [srcW, srcH] = [video.videoWidth, video.videoHeight];
    const [tw, th] = bake_size(srcW, srcH, TIER_NARROW);
    if (!tw || !th) throw new Error(`${name}: ${srcW}x${srcH} is not a usable video size`);
    const duration = video.duration;
    if (!Number.isFinite(duration) || duration <= 0) {
      throw new Error(`${name}: no duration — a stream cannot be baked, only a file`);
    }

    // `alpha: false` because Hap1 is BC1 and carries no alpha; saying so lets
    // the browser skip the premultiply on every drawImage.
    const canvas = document.createElement('canvas');
    canvas.width = tw;
    canvas.height = th;
    const ctx = canvas.getContext('2d', { willReadFrequently: true, alpha: false });

    const step = 1 / BAKE_FPS;
    // Strictly inside the duration: a seek to exactly the end lands past the
    // last frame on some decoders and fires nothing.
    const total = Math.max(1, Math.ceil((duration - 1e-3) * BAKE_FPS));
    // Counted before the baker is built, not after the loop, so it can size its
    // output buffer once instead of doubling it under a heap that has no room
    // to spare by then.
    const baker = new Baker(srcW, srcH, TIER_NARROW, BAKE_HIGH, total);

    const seekTo = async (t) => {
      // A seek to where the head already is fires no `seeked`, which would
      // hang the very first step of every bake.
      if (Math.abs(video.currentTime - t) < 1e-9 && video.readyState >= 2) return;
      const landed = nextEvent(video, 'seeked');
      video.currentTime = t;
      await Promise.race([
        landed,
        new Promise((_, reject) => setTimeout(
          () => reject(new Error(`${name}: seek to ${t.toFixed(2)}s never landed`)),
          SEEK_TIMEOUT_MS)),
      ]);
    };

    for (let i = 0; i < total; i++) {
      const t = Math.min(i * step, duration - 1e-3);
      // Checked every frame, not once: the visitor can background the tab at
      // any point in a bake that runs for minutes.
      if (await whileVisible(held)) onProgress((i + 1) / total, baker.frames);
      await seekTo(t);
      ctx.drawImage(video, 0, 0, tw, th);
      const img = ctx.getImageData(0, 0, tw, th).data;
      // A Uint8ClampedArray view is not what wasm-bindgen wants for &[u8];
      // this reinterprets the same buffer rather than copying it.
      baker.push(new Uint8Array(img.buffer, img.byteOffset, img.byteLength), t);
      if (i % 4 === 0) onProgress((i + 1) / total, baker.frames);
    }
    onProgress(1, baker.frames);
    return baker.finish();
  } finally {
    URL.revokeObjectURL(url);
    video.removeAttribute('src');
    video.load();               // release the decoder now, not at the next GC
  }
}

/**
 * A `.viproj` rather than a video: load it against the clips already in the
 * pool.
 *
 * Routed here rather than in `ingest` because a project is not a clip and never
 * becomes one — nothing about it is baked, stored as a clip, or probed. The
 * Rust reports missing clips by name, which is the message worth surfacing:
 * "drop bun.mov first" is actionable and "project failed to load" is not.
 */
async function loadProjectFile(file) {
  status(`loading ${file.name}…`);
  try {
    const text = await file.text();
    status(load_project(text));
    // So a save gives it back under the name it arrived with.
    set_project_name(file.name);
    // Kept only once it has loaded: storing a project the engine refused would
    // make the refusal permanent, re-run on every reload with no way out but
    // Forget.
    await storeProject(text).catch((e) => console.warn('could not store the project', e));
    showForget();
  } catch (e) {
    status(String(e.message ?? e), true);
  }
}

/** Whether this file is a project rather than something to play. */
const isProject = (name) => name.toLowerCase().endsWith('.viproj');

async function ingest(name, bytes, { persist = true } = {}) {
  // Probe the container rather than the filename: a `.mov` may hold ProRes,
  // and a HAP clip renamed `.mp4` is still playable as-is.
  if (!is_baked(bytes)) {
    status(`baking ${name}…`);
    const t0 = performance.now();
    bytes = await bakeToHap(bytes, name, (frac, frames, note) =>
      status(note ?? `baking ${name}… ${Math.round(frac * 100)}% · ${frames} frames`));
    console.info(`baked ${name} in ${((performance.now() - t0) / 1000).toFixed(1)}s`);
    name = `${name.replace(/\.[^.]+$/, '')}.mov`;
  }
  load_clip(name, bytes);
  status(`playing ${name}`);
  if (!persist) return;
  try {
    if (await storeClip(name, bytes)) {
      showForget();
      if (!(await askToPersist())) {
        console.info('storage is not persistent — the browser may evict this clip');
      }
    }
  } catch (e) {
    console.warn('could not store the clip', e);
    status(`playing ${name} (not stored: ${e.name ?? e})`);
  }
}

/** Offer the way out, now that there is something to forget. */
function showForget() {
  $('forget').hidden = false;
  $('forget').disabled = false;
}

/**
 * Everything the store holds, back on screen: the clips, then their project.
 *
 * That order is the same one a visitor's hands take — clips into the pool
 * first, project second — because `load_project` resolves clip references
 * against what the pool already has. A project restored before its clips would
 * report every one of them missing.
 */
async function resumeStored() {
  const clips = await storedClips();
  const project = await storedProject();
  if (!clips.length && !project) return;
  showForget();
  for (const c of clips) {
    // Already in storage: re-writing it on the way in would be a megabyte of
    // pointless I/O per clip on every reload.
    await ingest(c.name, c.bytes, { persist: false });
  }
  if (!project) {
    status(clips.length === 1 ? `resumed ${clips[0].name}` : `resumed ${clips.length} clips`);
    return;
  }
  try {
    status(`resumed — ${load_project(project)}`);
  } catch (e) {
    // The clips are already up, so this is a degraded resume rather than a
    // failed one, and saying which is the difference between "my set is gone"
    // and "my cues are gone".
    console.warn('the stored project did not load', e);
    status(`resumed ${clips.length} clip(s); the stored project did not load`, true);
  }
}

// --- the /chop handoff ----------------------------------------------------
//
// `/chop` can send an export here instead of downloading it. Same origin, same
// OPFS root, so it writes a directory and this claims it — no download, no
// re-upload, no file chooser between marking a video and playing it.
//
// Claimed, not read: the directory is emptied once its contents are in the
// store. A handoff that stayed would reload itself over the top of the
// session on every visit, and the second one would be a surprise.

const HANDOFF_DIR = 'handoff';

/**
 * What is waiting in the handoff directory, or null.
 *
 * The `.viproj` is the marker `/chop` writes last, so its presence means the
 * clips beside it are complete. Without it there is nothing to claim, whatever
 * else the directory holds.
 */
async function peekHandoff() {
  const root = await opfs();
  if (!root) return null;
  let dir;
  try {
    dir = await root.getDirectoryHandle(HANDOFF_DIR);
  } catch {
    return null;                         // nothing waiting, which is the norm
  }
  if (typeof dir.keys !== 'function') return null;
  const names = [];
  for await (const n of dir.keys()) names.push(n);
  const project = names.find((n) => n.toLowerCase().endsWith('.viproj'));
  if (!project) {
    console.warn('a handoff directory with no .viproj — ignoring it');
    return null;
  }
  return { dir, project, clips: names.filter((n) => n !== project).sort() };
}

/** Take a peeked handoff into the store and onto the screen. */
async function claimHandoff(h) {
  const read = async (n) => (await (await h.dir.getFileHandle(n)).getFile());
  for (const n of h.clips) {
    const file = await read(n);
    if (file.size) await ingest(n, new Uint8Array(await file.arrayBuffer()));
  }
  const text = await (await read(h.project)).text();
  const note = load_project(text);
  await storeProject(text);
  showForget();
  status(`from /chop: ${note}`);

  const root = await opfs();
  await root?.removeEntry(HANDOFF_DIR, { recursive: true }).catch((e) =>
    console.warn('could not clear the handoff directory', e));
}

/**
 * Put the last session back, and let a `/chop` handoff outrank it.
 *
 * The two kinds of handoff want opposite things from what is already stored,
 * which is why this is one function rather than two calls in a row:
 *
 * - A **clips** handoff is a whole session. It replaces the store, because its
 *   project names its own clips and nothing else — leaving the previous pool
 *   underneath would fill the library with clips no cue can reach.
 * - An **offsets** handoff is a project and nothing else. It is *about* a video
 *   that has to be here already, so the stored clips are exactly what it needs
 *   and clearing them would delete the thing it references.
 */
async function restoreSession() {
  const handoff = await peekHandoff().catch((e) => {
    console.warn('could not read the /chop handoff', e);
    return null;
  });

  if (handoff?.clips.length) {
    await forgetClips();
    await claimHandoff(handoff);
    return;
  }
  await resumeStored();
  if (handoff) await claimHandoff(handoff);
}

$('start').addEventListener('click', async () => {
  $('start').disabled = true;
  try {
    status('opening output window…');
    output = openOutputHead();           // inside the gesture — do this first

    status('loading wasm…');
    allowCrossRealmCanvasContext();
    await init();

    const control = $('control');
    control.hidden = false;
    $('boot-note').hidden = true;
    fit(control, control.clientWidth, control.clientHeight);

    status('requesting GPU…');
    await boot(control, output.canvas);
    booted = true;
    $('file').disabled = false;
    $('audio').disabled = false;

    // `?soft=1` forces CPU block decompression on a machine that has BC. The
    // fallback would otherwise only ever run where it cannot be observed, which
    // is the same as not having one.
    if (params.get('soft') === '1') set_soft_decode(true);

    // Restore the session before the clip, so the first frame that reaches the
    // screen is already at the right tempo rather than snapping a beat later.
    //
    // Guarded, and the session dropped on failure. A saved effect index that no
    // longer exists — ship a build with one fewer effect and every returning
    // visitor has one — makes set_effect throw, and an exception here lands in
    // the boot catch *after* the engine is already running: the page looks
    // booted, and everything below silently never happens. No stored clip, no
    // autosave, and no ResizeObserver, so dragging the output head to a
    // projector leaves its swapchain at the old size. It would also be
    // permanent, because nothing else ever clears this key.
    try {
      const s = loadSession();
      if (typeof s.bpm === 'number') set_bpm(s.bpm);
      if (s.effect === null || typeof s.effect === 'number') set_effect(s.effect ?? undefined);
    } catch (e) {
      console.warn('discarding a saved session this build cannot restore', e);
      localStorage.removeItem(SESSION_KEY);
    }

    status('running — choose a clip');
    try {
      await restoreSession();
    } catch (e) {
      console.warn('could not restore the stored session', e);
    }

    // Sampled rather than written on every change: the engine's tempo moves
    // continuously under a nudge, and a listener per change would write to
    // localStorage hundreds of times a second.
    setInterval(() => {
      try {
        const st = JSON.parse(engine_state());
        saveSession({ bpm: st.bpm, effect: st.effect });
      } catch { /* not booted, or storage denied */ }
    }, 2000);
    window.addEventListener('pagehide', () => {
      try {
        const st = JSON.parse(engine_state());
        saveSession({ bpm: st.bpm, effect: st.effect });
      } catch { /* nothing to save */ }
    });

    // Keep both swapchains matched to their canvases.
    const sync = () => {
      const c = fit(control, control.clientWidth, control.clientHeight);
      if (c) resize('control', c[0], c[1]);
      if (output && !output.win.closed) {
        const o = fit(output.canvas, output.win.innerWidth, output.win.innerHeight);
        if (o) resize('output', o[0], o[1]);
      }
    };
    new ResizeObserver(sync).observe(control);
    output.win.addEventListener('resize', sync);
    window.addEventListener('beforeunload', () => output?.win?.close());
  } catch (e) {
    console.error(e);
    status(String(e.message ?? e), true);
    $('start').disabled = false;
  }
});

$('file').addEventListener('change', async (ev) => {
  const files = [...(ev.target.files ?? [])];
  if (files.length) await acceptFiles(files);
});

$('audio').addEventListener('click', async () => {
  $('audio').disabled = true;
  try {
    const what = await listen();
    $('audio').textContent = 'Listening';
    status(`audio: ${what}`);
  } catch (e) {
    console.error(e);
    status(String(e.message ?? e), true);
    $('audio').disabled = false;
  }
});

$('forget').addEventListener('click', async () => {
  await forgetClips();
  $('forget').disabled = true;
  status('stored clips and project removed — they will not come back on reload');
});

// The control panels ask for files too, and they are painted on a canvas, so
// they cannot hold an `<input>` of their own. A `Pick*` command becomes a
// `vidiotic-pick` event here and the answer goes back through an export, which
// keeps every file read in this file — the same boundary `load_clip` draws.
//
// The chooser needs transient user activation. We are inside a rAF callback by
// now rather than the pointer handler that started this, which is allowed: the
// activation window is five seconds wide and the visitor clicked microseconds
// ago. It is still the fragile step, so the smoke test asserts the chooser
// opens rather than trusting the rule.
const PICKERS = {
  isf: {
    accept: '.fs,.frag,.glsl,.txt',
    apply: (name, text) => load_isf_source(name, text),
  },
  shader: {
    accept: '.wgsl,.txt',
    apply: (name, text) => load_shader_source(name, text),
  },
};

window.addEventListener('vidiotic-pick', (ev) => {
  const kind = PICKERS[ev.detail];
  if (!kind) { console.warn(`unknown pick: ${ev.detail}`); return; }
  const input = document.createElement('input');
  input.type = 'file';
  input.accept = kind.accept;
  input.addEventListener('change', async () => {
    const file = input.files?.[0];
    if (!file) return;
    try {
      // Text, not bytes: a shader is source either way, and the engine
      // transpiles the same string the native player reads off disk.
      kind.apply(file.name, await file.text());
    } catch (e) {
      console.error(e);
      status(String(e.message ?? e), true);
    }
  });
  input.click();
});

// The other direction: the panels' Save writes a `.viproj` and its clips, and a
// download is the only way out of a tab. The Rust builds the archive and asks
// the page to hand it over, which keeps blobs and anchors here — the same
// boundary `load_clip` and the picker bridge draw.
window.addEventListener('vidiotic-save', (ev) => {
  const [name, bytes] = ev.detail;
  const url = URL.createObjectURL(new Blob([bytes], { type: 'application/zip' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  a.click();
  // Revoking immediately can race the download starting in some browsers.
  setTimeout(() => URL.revokeObjectURL(url), 30_000);
  lastSave = { name, size: bytes.length, bytes };
});

// Kept so the smoke test can read back the archive the visitor was handed,
// rather than a second one built for the test.
let lastSave = null;

// --- cameras --------------------------------------------------------------
//
// The engine has always had camera cues — a pool clip whose source is a device
// rather than a file, with its timeline knobs inert and a delay instead. What a
// browser has to supply is the pixels, and the only way to get them is
// `getUserMedia` -> `MediaStream` -> a media element, because a stream is not
// bytes and there is nothing else that will play one.
//
// So the page owns the stream and the `<video>`, and the Rust samples the
// element. Same division as everywhere else here: browser APIs live in this
// file, and what a camera *means* to a session lives in the engine.
//
// Two things about enumeration are worth knowing before reading this:
//
//   - `enumerateDevices` lists cameras before permission is granted, but with
//     **empty labels**. That is a privacy rule. So a first listing is
//     positional ("Camera 1"), and this re-enumerates once a stream is granted,
//     when the real labels appear.
//   - `deviceId` is the uid a `.viproj` stores. It is stable per origin, which
//     is what makes a saved camera cue find its device again — and why it does
//     *not* survive being opened on somebody else's machine, where the row goes
//     missing and offers a relink.

/** Live streams and their elements, by device id. */
const CAMERAS = new Map();

/** Ask the browser what capture devices exist, and tell the engine. */
async function listCameras() {
  if (!navigator.mediaDevices?.enumerateDevices) {
    status('this browser has no camera access', true);
    return;
  }
  try {
    const all = await navigator.mediaDevices.enumerateDevices();
    const cams = all.filter((d) => d.kind === 'videoinput');
    set_cameras(
      cams.map((d) => d.deviceId),
      cams.map((d, i) => d.label || `Camera ${i + 1}`),
    );
  } catch (e) {
    console.error(e);
    status(`could not list cameras: ${e.message ?? e}`, true);
  }
}

/**
 * Start a camera and hand its element to the engine.
 *
 * The element is attached to the document, off-screen rather than
 * `display:none`: a hidden element is one a browser is entitled to stop
 * driving, and a video that stops presenting is a cue that freezes.
 */
async function startCamera(uid) {
  if (CAMERAS.has(uid)) return;
  try {
    const stream = await navigator.mediaDevices.getUserMedia({
      video: { deviceId: { exact: uid } },
      audio: false,
    });
    const el = document.createElement('video');
    el.autoplay = true;
    el.muted = true;
    el.playsInline = true;
    el.srcObject = stream;
    el.style.cssText = 'position:fixed;left:-10000px;top:0;width:2px;height:2px';
    document.body.appendChild(el);
    CAMERAS.set(uid, { stream, el });

    // Not before metadata: videoWidth is 0 until then, and the Rust samples
    // through a canvas it sizes from that.
    if (!el.videoWidth) {
      await new Promise((resolve, reject) => {
        el.addEventListener('loadedmetadata', resolve, { once: true });
        el.addEventListener('error', () => reject(new Error('the camera element failed')), { once: true });
        setTimeout(() => reject(new Error('the camera produced no picture in 10s')), 10_000);
      });
    }
    await el.play().catch(() => {});
    camera_ready(uid, el);
    // The labels are real now that a stream was granted, so the rows can stop
    // saying "Camera 1".
    await listCameras();
  } catch (e) {
    console.error(e);
    stopCamera(uid, e.name === 'NotAllowedError' ? 'permission refused' : String(e.message ?? e));
  }
}

/** Stop a camera, release the device, and tell the engine why if it failed. */
function stopCamera(uid, reason) {
  const held = CAMERAS.get(uid);
  if (held) {
    held.stream.getTracks().forEach((t) => t.stop());
    held.el.srcObject = null;
    held.el.remove();
    CAMERAS.delete(uid);
  }
  camera_stopped(uid, reason);
}

window.addEventListener('vidiotic-cameras', () => { void listCameras(); });
window.addEventListener('vidiotic-camera', (ev) => {
  const [uid, on] = ev.detail;
  if (on) void startCamera(uid);
  else stopCamera(uid, undefined);
});

// Devices come and go — a USB camera unplugged mid-set is a row that should
// stop claiming to exist.
navigator.mediaDevices?.addEventListener?.('devicechange', () => {
  for (const uid of [...CAMERAS.keys()]) {
    // A stream whose track has ended is a device that went away.
    const held = CAMERAS.get(uid);
    if (held && held.stream.getVideoTracks().every((t) => t.readyState === 'ended')) {
      stopCamera(uid, 'the device was disconnected');
    }
  }
  void listCameras();
});

// Drag and drop, on the document. `dragover` must be cancelled or the browser
// navigates to the file instead, which reads as the page crashing.
let dragDepth = 0;
document.addEventListener('dragover', (e) => { e.preventDefault(); });
document.addEventListener('dragenter', (e) => {
  e.preventDefault();
  if (booted && ++dragDepth === 1) document.body.classList.add('dropping');
});
document.addEventListener('dragleave', () => {
  if (--dragDepth <= 0) { dragDepth = 0; document.body.classList.remove('dropping'); }
});
document.addEventListener('drop', async (e) => {
  e.preventDefault();
  dragDepth = 0;
  document.body.classList.remove('dropping');
  const files = [...(e.dataTransfer?.files ?? [])];
  if (!files.length) return;
  if (!booted) { status('click Start first', true); return; }
  await acceptFiles(files);
});

/**
 * Take everything the visitor dropped, clips before projects.
 *
 * The order is the point of accepting more than one at a time: a `.viproj`
 * resolves clip references against the pool as it stands, so dropping a
 * project and its clips together only works if the clips land first.
 * Sorted within each group so the pool interns them in a stable order.
 */
async function acceptFiles(files) {
  const clips = files.filter((f) => !isProject(f.name))
    .sort((a, b) => a.name.localeCompare(b.name));
  const projects = files.filter((f) => isProject(f.name));
  try {
    for (const f of clips) {
      status(`reading ${f.name}…`);
      await ingest(f.name, new Uint8Array(await f.arrayBuffer()));
    }
    // Only the last one can be in effect — loading a project swaps the session
    // — so loading the others first would be work whose result is discarded.
    if (projects.length) await loadProjectFile(projects[projects.length - 1]);
  } catch (err) {
    console.error(err);
    status(String(err.message ?? err), true);
  }
}

// Say no before the first click rather than after it.
const blocker = whyNotWebGPU();
if (blocker) {
  $('start').disabled = true;
  $('boot-note').innerHTML =
    `<div class="fatal"><b>${blocker.title}</b><ul>${
      blocker.lines.map((l) => `<li>${l}</li>`).join('')
    }</ul></div>`;
  status('unsupported browser', true);
}

// For the smoke test: it drives this page over CDP and needs the readback.
// "No exception was thrown" is not evidence that anything rendered.
window.__vidiotic = {
  build: BUILD,
  centre_pixel, load_clip, set_effect, effect_names, engine_state,
  set_soft_decode, set_bpm, is_baked, bake_size, bakeToHap, push_audio,
  // The shader half of the picker bridge. The chooser itself cannot be driven
  // from a script, so the smoke proves the two halves separately: that a
  // chooser opens at all, and that source handed to these compiles.
  load_isf_source, load_shader_source,
  // The storage path too: it is a claim about what survives a reload, and
  // nothing about it is visible in a pixel.
  ingest, loadProjectFile, load_project, forgetClips, storedClips, storedProject,
  acceptFiles, peekHandoff, restoreSession,
  save_project, set_project_name, lastSave: () => lastSave,
  // The camera bridge. `getUserMedia` needs a device and a permission a
  // headless run has neither of, so the smoke drives the two halves it can:
  // that enumeration reaches the engine, and that a cue on a camera the page
  // never started opens nothing rather than breaking the rotation.
  set_cameras, listCameras, startCamera, stopCamera, add_camera_cue,
};
