// chop.js — the page half of /chop's browser shell.
//
// A separate file rather than a script block for the same reason boot.js is:
// the deploy target's CSP has no 'unsafe-inline'.
//
// What lives here is exactly what a browser API forces to: opening files the
// visitor chose, and decoding video. Everything else — the editor, the panels,
// undo, the span list — is the same Rust the desktop app runs. This file is
// transport, not a second implementation.

import init, {
  boot, video_opened, open_failed, load_project, load_shader, deliver_frame, editor_state,
  export_baked, export_note, export_failed, export_finish, export_finish_files,
  start_export, Baker, bake_size, load_session, storage_note, set_render, set_destination,
} from './pkg-chop/vidiotic_chop.js';

const params = new URLSearchParams(location.search);

// Frames sampled per second of source. Must match `web::ASSUMED_FPS` in the
// Rust: a <video> element has a duration and no frame count, so a frame index
// is a shared convention rather than a property of the file. `?fps=` moves both
// ends at once because the Rust takes the rate as data.
const FPS = Math.min(Math.max(Number(params.get('fps')) || 30, 1), 60);
// Width the preview is decoded at, matching `PREVIEW_WIDTH` in vidiotic-prep.
// The marking session never needs source resolution — it needs a picture to
// judge a cut against.
const PREVIEW_W = 960;
// A seek that never lands. Generous: the first seek into a fresh decoder is
// much slower than the ones after it.
const SEEK_TIMEOUT_MS = 15_000;
// `?capture=seek` forces the old per-frame path, so the two can be compared on
// a real machine rather than argued about. `?capture=play` forces the new one
// even in a hidden document, which is only useful for finding out what that
// actually does on a given browser.
const CAPTURE = params.get('capture');
// Frames of seek-stepping used to decide which capture path this source wants.
const PROBE_FRAMES = 4;
// Above this per-frame cost, seek-stepping is losing to realtime playback.
// Realtime at 30 fps is 33ms; below that a seek is cheap enough to beat a path
// that can never go faster than the clip itself.
const SLOW_SEEK_MS = 30;

const nextEvent = (el, name) =>
  new Promise((resolve) => el.addEventListener(name, resolve, { once: true }));

// --- persistence ----------------------------------------------------------
//
// OPFS holds the video and the sidecar; localStorage holds the video's name.
// Three things, two stores, for the reason /play splits them the same way: the
// video is megabytes of opaque bytes with a write API built for exactly that,
// and the name is one string that should be readable by a human in devtools.
//
// The sidecar goes to OPFS rather than localStorage because it grows with the
// session — a heavy marking session is tens of spans and localStorage is a
// few MB per origin shared with everything else the site stores.
//
// All of it is best-effort. Private-browsing modes deny OPFS, quota is not
// guaranteed, and none of it is worth failing a boot over: a session with no
// storage is exactly the session this page had before storage existed. Every
// path here reports and continues.

const SOURCE_FILE = 'source.bin';
const SESSION_FILE = 'session.vprep';
const NAME_KEY = 'vidiotic.chop.sourceName';

const opfs = () => navigator.storage?.getDirectory?.();

/**
 * Ask the browser to stop counting this origin as disposable.
 *
 * OPFS is evictable by default: under storage pressure a browser may clear the
 * whole origin without telling anyone, and an evening's marking that silently
 * is not there tomorrow is worse than one that was never stored.
 *
 * Asked once, and only after something has actually been written — Firefox
 * raises a permission prompt for this, and prompting before there is anything
 * to protect asks a question the visitor has no way to answer.
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

async function writeFile(name, data) {
  const root = await opfs();
  if (!root) return false;
  const fh = await root.getFileHandle(name, { create: true });
  const w = await fh.createWritable();
  await w.write(data);
  await w.close();
  return true;
}

async function readFile(name) {
  const root = await opfs();
  if (!root) return null;
  try {
    const file = await (await root.getFileHandle(name)).getFile();
    return file.size ? file : null;
  } catch {
    return null;                // NotFoundError — nothing stored, not a failure
  }
}

async function storeSource(file) {
  try {
    if (!(await writeFile(SOURCE_FILE, file))) { storage_note(undefined); return; }
    localStorage.setItem(NAME_KEY, file.name);
    // Clearing the old sidecar matters: its spans belong to the *previous*
    // video, and restoring them against a new one would put marks at frame
    // numbers that mean something else entirely.
    await (await opfs())?.removeEntry(SESSION_FILE).catch(() => {});
    await noteStorage();
    if (!(await askToPersist())) {
      console.info('storage is not persistent — the browser may evict this session');
    }
  } catch (e) {
    console.warn('could not store the video', e);
    storage_note(undefined);
  }
}

async function storeSession(ron) {
  try {
    await writeFile(SESSION_FILE, new Blob([ron]));
    await noteStorage();
  } catch (e) {
    console.warn('could not store the session', e);
  }
}

async function forgetStored() {
  const root = await opfs();
  await root?.removeEntry(SOURCE_FILE).catch(() => {});
  await root?.removeEntry(SESSION_FILE).catch(() => {});
  localStorage.removeItem(NAME_KEY);
  storage_note(undefined);
}

/** Tell the Rust what is held, so the inspector can say so. */
async function noteStorage() {
  const src = await readFile(SOURCE_FILE);
  if (!src) { storage_note(undefined); return; }
  const name = localStorage.getItem(NAME_KEY) ?? 'a video';
  const ses = await readFile(SESSION_FILE);
  const mb = (src.size / 1e6).toFixed(1);
  storage_note(`${name} (${mb} MB)${ses ? ' and its spans' : ''} kept in this browser`);
}

/** Re-open whatever was stored. Returns whether anything was. */
async function restoreStored() {
  const src = await readFile(SOURCE_FILE);
  if (!src) { storage_note(undefined); return false; }
  const name = localStorage.getItem(NAME_KEY) ?? 'restored.mov';
  await openVideo(new File([src], name), { store: false });
  if (!source) return false;    // it failed to decode; openVideo already said so
  const ses = await readFile(SESSION_FILE);
  if (ses) load_session(await ses.text());
  await noteStorage();
  return true;
}

// --- the open video -------------------------------------------------------

/** @type {{el: HTMLVideoElement, url: string, canvas: HTMLCanvasElement, ctx: CanvasRenderingContext2D, w: number, h: number} | null} */
let source = null;

function closeSource() {
  if (!source) return;
  URL.revokeObjectURL(source.url);
  source.el.removeAttribute('src');
  source.el.load();   // release the decoder now, not at the next GC
  source = null;
}

/**
 * Wait until this document is visible.
 *
 * Chrome will not load a media element in a hidden document: readyState stays
 * 0, no `error` fires, and nothing is ever buffered. /play hit this during a
 * bake and it has no timeout and no event, so an unguarded wait never ends.
 * It applies here too — a visitor can pick a file and switch tabs — but the
 * consequence is milder: a seek that has not landed just means the preview
 * holds its last frame, which is what it does anyway.
 */
async function whileVisible() {
  while (document.visibilityState !== 'visible') {
    await nextEvent(document, 'visibilitychange');
  }
}

async function openVideo(file, { store = true } = {}) {
  closeSource();
  const url = URL.createObjectURL(file);
  const el = document.createElement('video');
  el.muted = true;
  el.playsInline = true;
  el.preload = 'auto';
  el.src = url;
  try {
    await whileVisible();
    await Promise.race([
      nextEvent(el, 'loadeddata'),
      nextEvent(el, 'error').then(() => {
        throw new Error(`${file.name}: this browser cannot decode it`);
      }),
    ]);
    const duration = el.duration;
    if (!Number.isFinite(duration) || duration <= 0) {
      throw new Error(`${file.name}: no duration — a stream cannot be marked, only a file`);
    }
    const [sw, sh] = [el.videoWidth, el.videoHeight];
    if (!sw || !sh) throw new Error(`${file.name}: ${sw}x${sh} is not a usable video size`);

    // Preview dimensions: scaled to PREVIEW_W, rounded even, never upscaled.
    const scale = Math.min(1, PREVIEW_W / sw);
    const w = Math.max(2, Math.round(sw * scale / 2) * 2);
    const h = Math.max(2, Math.round(sh * scale / 2) * 2);
    const canvas = document.createElement('canvas');
    canvas.width = w;
    canvas.height = h;
    // `willReadFrequently` because every frame is a getImageData; `alpha:false`
    // lets the browser skip the premultiply on each drawImage.
    const ctx = canvas.getContext('2d', { willReadFrequently: true, alpha: false });

    source = { el, url, canvas, ctx, w, h };
    // Strictly inside the duration: a seek to exactly the end lands past the
    // last frame on some decoders and fires nothing.
    const frames = Math.max(1, Math.ceil((duration - 1e-3) * FPS));
    video_opened(file.name, frames, FPS, sw, sh, duration);
    // Stored after it is known to decode: a file this browser cannot play is
    // not worth keeping, and would fail the same way on every reload.
    if (store) void storeSource(file);
  } catch (e) {
    URL.revokeObjectURL(url);
    open_failed(String(e.message ?? e));
  }
}

// --- frame service --------------------------------------------------------
//
// The Rust asks for one frame at a time and waits; this answers. Seeks are
// serialised here as well as there, because a stray double-request would
// interleave two `seeked` waits on one element and resolve them to each
// other's frames.

let seeking = false;
let queued = null;

async function serveFrame(index) {
  // A bake owns the <video> element while it runs: two seek loops on one
  // element resolve each other's `seeked` events and both get wrong frames.
  // The preview simply holds its last picture, which is what it does anyway.
  if (exporting) return;
  if (seeking) { queued = index; return; }
  seeking = true;
  try {
    while (true) {
      if (!source) return;
      const { el, ctx, w, h } = source;
      const t = Math.min(index / FPS, Math.max(0, el.duration - 1e-3));
      await whileVisible();
      await seekTo(t);
      ctx.drawImage(el, 0, 0, w, h);
      const img = ctx.getImageData(0, 0, w, h).data;
      // A Uint8ClampedArray view is not what wasm-bindgen wants for &[u8];
      // this reinterprets the same buffer rather than copying it.
      deliver_frame(index, w, h, new Uint8Array(img.buffer, img.byteOffset, img.byteLength));
      if (queued === null) return;
      index = queued;
      queued = null;
    }
  } catch (e) {
    open_failed(String(e.message ?? e));
  } finally {
    seeking = false;
  }
}

// --- export ---------------------------------------------------------------
//
// The shell hands over a plan — one entry per span, with the in/out seconds it
// wants and the file name the .viproj will reference — and this bakes them.
//
// The bake itself is not here. Each frame goes into a `Baker`, which is
// `vidiotic-bake`'s driver over the same compressor and muxer the desktop
// exporter runs, so a clip baked in a tab is byte-identical to one baked on a
// desktop rather than merely equivalent. What this adds is the part only a
// page can do: seek-stepping the <video> element and scaling each frame.
//
// Seek-stepping rather than playback, for the reason /play's ingest documents:
// rVFC is driven by the document's rendering steps and Chrome pauses muted
// video in a hidden document, so a bake that loses the foreground silently
// truncates. Setting currentTime and waiting for `seeked` has none of that.

/**
 * Bake one span. Plays the video and captures presented frames when it can;
 * falls back to seek-stepping when it cannot.
 *
 * # Why both
 *
 * Seek-stepping is the correct-everywhere path: no autoplay policy, no
 * rendering steps, deterministic, and it keeps working in a hidden document.
 * /play's ingest uses it for exactly those reasons and this kept it.
 *
 * It is also *slow*, and the reason is the source rather than the code. Setting
 * `currentTime` per frame asks the demuxer for a random access every time, and
 * on a long-GOP H.264 file — which is what a camera produces — a browser may
 * decode from the previous keyframe to get there. At a 250-frame GOP that is up
 * to 250 decodes per delivered frame. Measured in Chrome on a 4K test source,
 * seek-stepping ran 7.8 fps against playback capture's 12.9; the gap is far
 * wider on a real GPU, where the per-frame scale is nearly free and the seek is
 * all that is left.
 *
 * Playback capture has one real cost and one apparent one. The real one: it
 * cannot beat realtime, because frames arrive as they are presented. The
 * apparent one: a browser under load may not present every frame, so a capture
 * can come back with fewer frames than the span has. That is *not* corruption —
 * `Baker` takes an explicit per-frame `mediaTime`, so a missing frame leaves a
 * longer gap rather than shortening the clip. The muxer records the timing it
 * was given.
 *
 * The fallback triggers on: a hidden document (Chrome pauses muted video-only
 * media there, so playback would silently stall), no `requestVideoFrameCallback`,
 * or a stall — and it resumes from the frame the capture actually reached, so a
 * tab backgrounded mid-span finishes rather than starting over.
 */
async function bakeSpan(span, quality, onFrame) {
  const { el } = source;
  const sx = span.crop ? Math.round(span.crop.x * el.videoWidth) : 0;
  const sy = span.crop ? Math.round(span.crop.y * el.videoHeight) : 0;
  const sw = span.crop ? Math.round(span.crop.w * el.videoWidth) : el.videoWidth;
  const sh = span.crop ? Math.round(span.crop.h * el.videoHeight) : el.videoHeight;
  const [tw, th] = bake_size(sw, sh, false);
  if (!tw || !th) throw new Error(`${sw}x${sh} is not a bakeable size`);
  const canvas = document.createElement('canvas');
  canvas.width = tw;
  canvas.height = th;
  const ctx = canvas.getContext('2d', { willReadFrequently: true, alpha: false });
  const total = Math.max(1, Math.round((span.out_sec - span.in_sec) * FPS));
  // Counted before the baker is built so it can size its output buffer once
  // rather than doubling it mid-span.
  const baker = new Baker(sw, sh, false, quality === 'high', total);

  const grab = (t) => {
    ctx.drawImage(el, sx, sy, sw, sh, 0, 0, tw, th);
    const img = ctx.getImageData(0, 0, tw, th).data;
    return baker.push(new Uint8Array(img.buffer, img.byteOffset, img.byteLength), t);
  };

  const step = 1 / FPS;
  const at = (i) => Math.min(span.in_sec + i * step, Math.max(0, el.duration - 1e-3));

  // Seek-step a few frames and time them, then decide.
  //
  // Neither path wins everywhere, which is why this measures instead of
  // choosing. Seek-stepping asks the demuxer for a random access per frame; on
  // a long-GOP camera file a browser may decode from the previous keyframe to
  // get there, and at a 250-frame GOP that is up to 250 decodes per delivered
  // frame. On an all-intra proxy every frame *is* a keyframe and the same code
  // is the fastest thing available — measured in Chrome on a 4K source:
  // 7.5 fps as shot, 73.7 fps through a tier-sized all-intra proxy.
  //
  // Playback capture has no per-frame seek at all, but it cannot beat realtime
  // because frames arrive as they are presented. So it is the right answer for
  // an unprepared source and the wrong one for a prepared source, and the probe
  // is four frames of work to tell them apart.
  let i = 0;
  const probeStart = performance.now();
  for (; i < Math.min(PROBE_FRAMES, total); i++) {
    await whileVisible();
    await seekTo(at(i));
    grab(at(i));
  }
  const perFrame = i > 0 ? (performance.now() - probeStart) / i : 0;

  if (i < total && perFrame > SLOW_SEEK_MS && canPlayCapture()) {
    try {
      const reached = await captureByPlayback(el, span.in_sec + total * step, grab, (t) =>
        onFrame(Math.round((t - span.in_sec) * FPS), total));
      i = Math.max(i, Math.ceil((reached - span.in_sec) * FPS));
    } catch (e) {
      console.warn('playback capture stalled; finishing by seeking', e);
    }
  }

  // Whatever the chosen path did not reach.
  for (; i < total; i++) {
    await whileVisible();
    await seekTo(at(i));
    grab(at(i));
    if (i % 4 === 0) onFrame(i + 1, total);
  }

  onFrame(total, total);
  // Read before `finish`, which consumes the baker: in an object literal the
  // call would run first and leave `baker` a freed pointer.
  const frames = baker.frames;
  return { bytes: baker.finish(), frames, duration_sec: total / FPS };
}

/** Whether playback capture is available and would not be stalled by the tab. */
const canPlayCapture = () =>
  CAPTURE !== 'seek'
  && (CAPTURE === 'play' || document.visibilityState === 'visible')
  && typeof HTMLVideoElement !== 'undefined'
  && 'requestVideoFrameCallback' in HTMLVideoElement.prototype;

/**
 * Play from the current position to `end`, pushing each presented frame.
 * Returns the media time actually reached, so the caller can finish the rest.
 */
async function captureByPlayback(el, end, grab, onTime) {
  const from = el.currentTime;
  await el.play();
  // Not `el.currentTime`: playback has already moved it. A capture that
  // presents nothing must report the position it started from, or the caller
  // skips the frames it never got.
  let reached = from;
  try {
    await new Promise((resolve, reject) => {
      // A watchdog rather than a timeout on the whole span: a long span is not
      // a stall, but a decoder that stops presenting is, and rVFC simply goes
      // quiet when that happens.
      let alarm = null;
      const arm = () => {
        clearTimeout(alarm);
        alarm = setTimeout(() => reject(new Error('no frame presented for 4s')), 4000);
      };
      const done = (fn) => { clearTimeout(alarm); fn(); };
      const onFrame = (_now, meta) => {
        reached = meta.mediaTime;
        if (meta.mediaTime >= end || el.ended) { done(resolve); return; }
        grab(meta.mediaTime);
        reached = meta.mediaTime + 1e-6;
        onTime(meta.mediaTime);
        // Backgrounding mid-span stops presentation; hand back to the seek
        // path rather than waiting for frames that will not come.
        if (document.visibilityState !== 'visible') { done(resolve); return; }
        arm();
        el.requestVideoFrameCallback(onFrame);
      };
      arm();
      el.requestVideoFrameCallback(onFrame);
    });
  } finally {
    el.pause();
  }
  return reached;
}

/** Seek `source.el` to `t` and wait for it to land. Shared with the preview. */
async function seekTo(t) {
  const { el } = source;
  // A seek to where the head already is fires no `seeked`, which would hang.
  if (Math.abs(el.currentTime - t) < 1e-9 && el.readyState >= 2) return;
  const landed = nextEvent(el, 'seeked');
  el.currentTime = t;
  await Promise.race([
    landed,
    new Promise((_, reject) => setTimeout(
      () => reject(new Error(`seek to ${t.toFixed(2)}s never landed`)), SEEK_TIMEOUT_MS)),
  ]);
}

async function runExport(plan) {
  if (!source) { export_failed('no video is open'); return; }
  // The preview shares the one <video> element, so a seek for a frame and a
  // seek for a bake would fight over it. Baking owns it until it is done.
  exporting = true;
  try {
    const started = performance.now();
    let baked = 0;
    for (const [n, span] of plan.spans.entries()) {
      const label = `baking ${n + 1}/${plan.spans.length}: ${span.name}`;
      export_note(label);
      const { bytes, frames, duration_sec } = await bakeSpan(span, plan.quality, (done, total) => {
        // The achieved rate, live. A bake that is going to take four minutes
        // should say so while there is still time to stop it and pick a
        // different render.
        const fps = baked + done > 0 ? (baked + done) / ((performance.now() - started) / 1000) : 0;
        export_note(`${label} — ${done}/${total} frames · ${fps.toFixed(1)} fps`);
      });
      baked += frames;
      export_baked(span.file, span.source, span.in_sec, span.out_sec,
                   FPS, frames, duration_sec, bytes);
    }
    const rate = baked / ((performance.now() - started) / 1000);
    console.info(`baked ${baked} frames at ${rate.toFixed(1)} fps `
      + `(${canPlayCapture() ? 'playback capture' : 'seek-stepping'})`);
    if (plan.dest === 1) {
      export_note('handing over…');
      await writeHandoff(export_finish_files());
    } else {
      export_note('packing…');
      download(export_finish(), 'project.zip');
    }
  } catch (e) {
    export_failed(String(e.message ?? e));
  } finally {
    exporting = false;
  }
}

/** Hand bytes to the visitor as a download. The only way out of a tab. */
function download(bytes, name) {
  const type = name.endsWith('.zip') ? 'application/zip' : 'application/octet-stream';
  const url = URL.createObjectURL(new Blob([bytes], { type }));
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  a.click();
  // Revoking immediately can race the download starting in some browsers.
  setTimeout(() => URL.revokeObjectURL(url), 30_000);
  lastExport = { name, size: bytes.length };
  lastZip = bytes;
  lastName = name;
}

let exporting = false;
let lastExport = null;
// Kept so the smoke test can read back the archive the visitor was handed,
// rather than a second one built for the test.
let lastZip = null;
let lastName = null;

// --- the handoff ----------------------------------------------------------
//
// A chop's destination can be `/play` rather than the visitor's downloads
// folder, and then it never leaves the browser: both routes are served from one
// origin, so they share one OPFS root, and this writes into a directory the
// other one claims on boot.
//
// **Loose files, and the marker written last.** The alternative — one archive —
// would mean compressing bytes so the tab next door could decompress them,
// which is work done to move a file between two directories that are already
// the same directory. The cost of loose files is that a half-written handoff is
// a project missing clips, so `project.viproj` is written *after* everything it
// names: /play claims a handoff only when the marker is there, and the marker
// is only there when the rest is.

const HANDOFF_DIR = 'handoff';

/**
 * Write an export into the directory `/play` reads.
 *
 * `entries` is `[[path, bytes], …]` from `export_finish_files`, with the
 * project first. Paths carry the archive's `name/clips/…` shape; only the file
 * name survives here, because a project resolves clips by name in a browser
 * (`PoolFs`) and a directory tree would be a layout with nothing to lay out.
 */
async function writeHandoff(entries) {
  const root = await opfs();
  if (!root) throw new Error('this browser is not storing anything for us');
  // Anything left from a previous handoff is a project /play would load
  // alongside this one. Clear first, and clear the marker before the clips so
  // an interrupted overwrite is an empty handoff rather than a mixed one.
  await root.removeEntry(HANDOFF_DIR, { recursive: true }).catch(() => {});
  const dir = await root.getDirectoryHandle(HANDOFF_DIR, { create: true });
  const base = (p) => p.split('/').pop();

  let project = null;
  for (const [path, bytes] of entries) {
    if (path.endsWith('.viproj')) { project = [base(path), bytes]; continue; }
    await writeInto(dir, base(path), bytes);
  }
  if (!project) throw new Error('the export produced no .viproj');
  // Last: this is the marker.
  await writeInto(dir, project[0], project[1]);
  await askToPersist();
}

async function writeInto(dir, name, data) {
  const fh = await dir.getFileHandle(name, { create: true });
  const w = await fh.createWritable();
  await w.write(data);
  await w.close();
}

/**
 * What is sitting in the handoff directory, as `[{name, size}, …]`.
 *
 * For the smoke test: a handoff is bytes in a directory no pixel reflects, and
 * the tab that reads it is a different page with a different graphics
 * requirement. This is how one side's half of the contract gets checked.
 */
async function handoffContents() {
  const root = await opfs();
  if (!root) return [];
  let dir;
  try {
    dir = await root.getDirectoryHandle(HANDOFF_DIR);
  } catch {
    return [];
  }
  if (typeof dir.keys !== 'function') return [];
  const out = [];
  for await (const name of dir.keys()) {
    const file = await (await dir.getFileHandle(name)).getFile();
    out.push({ name, size: file.size });
  }
  return out.sort((a, b) => a.name.localeCompare(b.name));
}

window.addEventListener('vidiotic-chop-export', (ev) => { void runExport(JSON.parse(ev.detail)); });
window.addEventListener('vidiotic-chop-save', (ev) => { void storeSession(ev.detail); });
// The offsets export renders nothing, so it never goes through the bake loop —
// the Rust builds the .viproj and asks for it to be saved directly.
window.addEventListener('vidiotic-chop-handoff', (ev) => {
  const [name, bytes] = ev.detail;
  void writeHandoff([[name, bytes]]).catch((e) => export_failed(String(e.message ?? e)));
});
window.addEventListener('vidiotic-chop-download', (ev) => {
  const [name, bytes] = ev.detail;
  download(bytes, name);
});
window.addEventListener('vidiotic-chop-forget', () => { void forgetStored(); });

// --- file choosers --------------------------------------------------------
//
// The panels post a `Pick*` command; the Rust turns it into this event; the
// page owns the <input type=file> that answers. The chain matters: a chooser
// must be opened from inside the gesture that asked for it, and the gesture is
// a click on a canvas the page owns.

const PICKERS = {
  video: {
    accept: 'video/*,.mov,.mp4,.mkv,.m4v,.webm',
    apply: (file) => openVideo(file),
  },
  project: {
    accept: '.viproj',
    apply: async (file) => load_project(file.name, await file.text()),
  },
  shader: {
    accept: '.wgsl,.fs,.frag,.glsl,.txt',
    // Only the name travels: prep stores a path string in the project's
    // defaults and nothing in the editor reads the file.
    apply: (file) => load_shader(file.name),
  },
};

function pick(kind) {
  const spec = PICKERS[kind];
  if (!spec) { console.warn(`unknown pick: ${kind}`); return; }
  const input = document.createElement('input');
  input.type = 'file';
  input.accept = spec.accept;
  input.addEventListener('change', async () => {
    const file = input.files?.[0];
    if (file) {
      try { await spec.apply(file); } catch (e) { open_failed(String(e.message ?? e)); }
    }
    input.remove();
  });
  input.click();
}

window.addEventListener('vidiotic-chop-pick', (ev) => pick(ev.detail));
window.addEventListener('vidiotic-chop-frame', (ev) => { void serveFrame(Number(ev.detail)); });

// Dropping a file on the page is the same act as choosing one, so it takes the
// same path rather than going through the editor's `Open` command — which in a
// browser can only turn round and ask for a chooser anyway.
window.addEventListener('dragover', (ev) => ev.preventDefault());
window.addEventListener('drop', (ev) => {
  ev.preventDefault();
  const file = ev.dataTransfer?.files?.[0];
  if (!file) return;
  if (file.name.toLowerCase().endsWith('.viproj')) void PICKERS.project.apply(file);
  else void openVideo(file);
});

// --- boot -----------------------------------------------------------------

await init();
await boot('chop');
// After boot, so the editor exists to restore into. Failures here are reported
// and stepped over: an empty editor is a working page.
try {
  await restoreStored();
} catch (e) {
  console.warn('could not restore the stored session', e);
  storage_note(undefined);
}

// The smoke test's only window into a canvas. Same role as /play's
// `window.__vidiotic`.
window.__chop = { editor_state, openVideo, load_project, pick, FPS, start_export, set_render,
                  set_destination,
                  lastExport: () => lastExport, lastZip: () => lastZip, lastName: () => lastName,
                  forgetStored, restoreStored, handoffContents,
                  exporting: () => exporting };
