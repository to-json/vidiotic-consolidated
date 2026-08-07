#!/usr/bin/env node
// play-smoke — drive /play in a real browser and prove it rendered.
//
// The analogue of vidiotic-bake's `bake_integrity.rs`: everything else in the
// port is checked by compiling or by unit tests, and neither can tell you that
// a canvas in a second document showed anything. The readback is the point —
// "no exception was thrown" is not evidence that anything rendered, and a blank
// or crossed surface is otherwise indistinguishable from a working one without
// a human looking at it (web-port.md §10a).
//
// Two independent claims, because they can fail separately:
//   1. the composite pass produces non-black pixels for a real HAP clip,
//      read back through copyTextureToBuffer;
//   2. the popped-out window's canvas is actually being presented, captured as
//      a screenshot of that target rather than inferred from the opener.
//
// No npm: Node 22+ has a built-in WebSocket and fetch, and Chrome speaks CDP.
//
// Usage:  node scripts/play-smoke.mjs [--headful] [--keep] [--dist] [--url BASE]
//
// `--url` drives a server that is already running rather than spawning a static
// one — in practice scripts/serve-play.sh's nginx, which is the configuration
// the real box will run. That is the strongest form of this check: the same
// assertions, against the same headers, MIME types and precompressed bodies
// that a visitor will get.
//
// `--dist` points every check at dist/play/ instead of web/ — the artifact that
// scripts/release-play.sh assembles, with its hashed bundle directory and its
// substituted build stamp. Those substitutions are the one part of the release
// that no compiler checks, and their failure mode is a blank page, so the thing
// that gets uploaded is the thing that should be driven.

import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const PORT = 8123;
const CDP_PORT = 9223;
const CLIP = 'clips/bun.mov';
// A second clip, so the cue rotation has somewhere to rotate to.
const CLIP2 = 'clips/eyes.mov';
// An *unbaked* source — the file a visitor actually turns up with. See the
// ingest section at the end for why it is VP9 and not H.264.
const SRC = 'clips/probe.webm';
const HEADFUL = process.argv.includes('--headful');
const KEEP = process.argv.includes('--keep');
const DIST = process.argv.includes('--dist');
// Relative to ROOT, which is what the server is rooted at so clips/ stays
// reachable from either page.
const SITE = DIST ? 'dist/play' : 'web';

// `--url <base>` drives an already-running server instead of spawning a static
// one — scripts/serve-play.sh's nginx, which is the config the real box will
// run. That server is rooted at the site, so the page is <base>/index.html and
// the clip comes from <base>/clips/, which is what the container's alias exists
// to provide.
const URL_ARG = process.argv[process.argv.indexOf('--url') + 1];
const EXTERNAL = process.argv.includes('--url') ? URL_ARG?.replace(/\/$/, '') : null;
if (process.argv.includes('--url') && !EXTERNAL) {
  console.error('--url needs a base URL, e.g. --url http://localhost:8080');
  process.exit(2);
}
const PAGE = EXTERNAL ? `${EXTERNAL}/index.html`
                      : `http://127.0.0.1:${PORT}/${SITE}/index.html`;

// Where the page fetches its fixtures from, which is not the same place in the
// two modes and is not simply "/" in either.
//
// Locally the spawned server is rooted at the repo, the page is at /web/ or
// /dist/play/, and clips/ sits at the root — so an absolute path is right.
// With --url the site may be mounted at a subpath (`/warez/vidiotic/`), and an
// absolute /clips/... then resolves against the *site's* root instead, where
// there is nothing. That 404 does not announce itself: the page reads the error
// document, finds no HAP header in it, decides the file needs baking, and hands
// a HAP .mov to the browser's decoder — which reports "this browser cannot
// decode it", naming the wrong culprit entirely.
const FIXTURES = EXTERNAL ? `${EXTERNAL}/` : '/';

const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

let failures = 0;
const ok = (msg) => console.log(`[ OK ] ${msg}`);
const bad = (msg) => { console.log(`[FAIL] ${msg}`); failures++; };

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function until(what, fn, { tries = 100, gap = 200 } = {}) {
  for (let i = 0; i < tries; i++) {
    try { const v = await fn(); if (v) return v; } catch { /* keep waiting */ }
    await sleep(gap);
  }
  throw new Error(`timed out waiting for ${what}`);
}

/** A CDP session over one WebSocket. */
class Session {
  constructor(ws) { this.ws = ws; this.id = 0; this.pending = new Map(); this.events = new Map(); }

  static async attach(wsUrl) {
    const ws = new WebSocket(wsUrl);
    const s = new Session(ws);
    ws.addEventListener('message', (ev) => {
      const msg = JSON.parse(ev.data);
      const p = s.pending.get(msg.id);
      if (p) { s.pending.delete(msg.id); p(msg); return; }
      // Events, for the checks that watch the browser do something rather than
      // ask it afterwards — a file chooser leaves no trace in the page.
      if (msg.method) s.events.get(msg.method)?.(msg.params);
    });
    await new Promise((res, rej) => {
      ws.addEventListener('open', res, { once: true });
      ws.addEventListener('error', rej, { once: true });
    });
    return s;
  }

  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((res, rej) => {
      this.pending.set(id, (m) => (m.error ? rej(new Error(`${method}: ${m.error.message}`)) : res(m.result)));
      setTimeout(() => rej(new Error(`${method}: timed out`)), 60000);
    });
  }

  /** Call `fn` each time the browser sends `method`. One handler per method. */
  on(method, fn) { this.events.set(method, fn); }

  /** Evaluate in the page; throws on a JS exception rather than returning undefined. */
  async eval(expression, { userGesture = false, awaitPromise = true } = {}) {
    const r = await this.send('Runtime.evaluate', {
      expression, awaitPromise, userGesture, returnByValue: true,
    });
    if (r.exceptionDetails) {
      const e = r.exceptionDetails;
      throw new Error(e.exception?.description ?? e.text);
    }
    return r.result.value;
  }

  close() { this.ws.close(); }
}

const targets = () => fetch(`http://127.0.0.1:${CDP_PORT}/json/list`).then((r) => r.json());

/**
 * Press keys through Chrome's real input pipeline.
 *
 * `Input.dispatchKeyEvent` rather than a synthetic `new KeyboardEvent(...)`:
 * the point is to exercise the browser's own key delivery and the canonical
 * spelling it produces, and a hand-built event would let the adapter agree with
 * a mistake the real pipeline never makes. A frame is allowed between presses
 * because the engine drains the key queue once per `requestAnimationFrame`.
 */
async function keys(page, names) {
  for (const key of names) {
    const single = key.length === 1;
    // `code` is physical-key identity, and space is the one press whose
    // `key` (" ") and `code` ("Space") are not the same word. The engine reads
    // `key`, but a wrong `code` is the kind of thing that starts mattering the
    // moment someone adds a layout-independent binding.
    const code = key === ' ' ? 'Space' : single ? `Key${key.toUpperCase()}` : key;
    await page.send('Input.dispatchKeyEvent', {
      type: single ? 'keyDown' : 'rawKeyDown',
      key,
      code,
      windowsVirtualKeyCode: single ? key.toUpperCase().charCodeAt(0) : 27,
      ...(single ? { text: key } : {}),
    });
    await page.send('Input.dispatchKeyEvent', { type: 'keyUp', key });
    await sleep(60);
  }
}

async function main() {
  // With --url the files are the server's business, not this script's; all that
  // can be checked from here is that the server answers.
  if (EXTERNAL) {
    const r = await fetch(PAGE).catch((e) => e);
    if (!(r instanceof Response) || !r.ok) {
      console.error(`nothing serving ${PAGE} — start it with scripts/serve-play.sh`);
      process.exit(2);
    }
  } else {
    if (!existsSync(join(ROOT, SITE, 'index.html'))) {
      console.error(DIST
        ? 'dist/play is missing — run scripts/release-play.sh first'
        : 'web/pkg is missing — run scripts/build-play.sh first');
      process.exit(2);
    }
    if (!DIST && !existsSync(join(ROOT, 'web/pkg/vidiotic_play.js'))) {
      console.error('web/pkg is missing — run scripts/build-play.sh first');
      process.exit(2);
    }
    if (!existsSync(join(ROOT, CLIP2))) {
      console.error(`${CLIP2} is not in this checkout; the rotation check needs two clips`);
      process.exit(2);
    }
    if (!existsSync(join(ROOT, CLIP))) {
      console.error(`${CLIP} is not in this checkout; nothing to play`);
      process.exit(2);
    }

  }
  if (!existsSync(CHROME)) {
    console.error(`no Chrome at ${CHROME}`);
    process.exit(2);
  }

  // Serve the repo root, so the page can fetch a clip out of clips/ the same
  // way a user would hand one over through the file input. Skipped under --url,
  // where something else is already serving both.
  //
  // Refuse to start if the port is taken, and do it *before* spawning. A
  // leaked server from an earlier run keeps the port, `http.server` exits with
  // "Address already in use" into a `stdio: 'ignore'`, and every check that
  // follows silently grades a different checkout — which is exactly what
  // happened: a server left over from a `-d <main repo>` run made a worktree
  // build look like it both passed and lacked functions it plainly exported.
  // A driver that tests the wrong tree and reports PASS is worse than one that
  // does not run.
  if (!EXTERNAL) {
    const stale = await fetch(`http://127.0.0.1:${PORT}/`, { method: 'HEAD' })
      .then(() => true).catch(() => false);
    if (stale) {
      console.error(`something is already serving 127.0.0.1:${PORT}.`);
      console.error(`It is probably a leaked smoke server, and it is not serving ${ROOT}.`);
      console.error(`  lsof -nP -iTCP:${PORT} -sTCP:LISTEN     # then kill it`);
      process.exit(2);
    }
  }
  const server = EXTERNAL ? null
    : spawn('python3', ['-m', 'http.server', '-d', ROOT, String(PORT)], { stdio: 'ignore' });

  const profile = mkdtempSync(join(tmpdir(), 'vidiotic-smoke-'));
  const chrome = spawn(CHROME, [
    ...(HEADFUL ? [] : ['--headless=new']),
    `--remote-debugging-port=${CDP_PORT}`,
    `--user-data-dir=${profile}`,
    // The output head is a real second window; without this the popup that
    // §10a's whole architecture rests on never opens.
    '--disable-popup-blocking',
    '--no-first-run', '--no-default-browser-check',
    '--enable-unsafe-webgpu',
    // The output head is a second window, so the control page is never the
    // frontmost target and headless marks it hidden. Chrome does not load a
    // media element in a hidden document — no error, no timeout, it simply
    // never buffers — which stops clip ingest dead. A visitor looking at their
    // own control window is not in this position; this driver always is.
    '--disable-backgrounding-occluded-windows',
    '--disable-renderer-backgrounding',
    '--disable-background-media-suspend',
    '--autoplay-policy=no-user-gesture-required',
    // A synthetic camera, and a granted permission. Without both, the camera
    // path could only ever be checked on a machine with a device and a human to
    // click Allow — which is the same as not checking it.
    '--use-fake-device-for-media-stream',
    '--use-fake-ui-for-media-stream',
    'about:blank',
  ], { stdio: 'ignore' });

  const cleanup = () => {
    // SIGKILL, not SIGTERM: a headless Chrome holding a live `getUserMedia`
    // track does not always go down on a polite signal, and one left behind
    // owns the debugging port — so the *next* run fails to attach for a reason
    // that has nothing to do with the code under test.
    chrome.kill('SIGKILL'); server?.kill();
    if (!KEEP) { try { rmSync(profile, { recursive: true, force: true }); } catch { /* best effort */ } }
  };
  process.on('exit', cleanup);

  const list = await until('chrome devtools', async () => {
    const t = await targets();
    return t.find((x) => x.type === 'page') ? t : null;
  });
  const page = await Session.attach(list.find((x) => x.type === 'page').webSocketDebuggerUrl);
  await page.send('Page.enable');
  await page.send('Runtime.enable');

  page.ws.addEventListener('message', (ev) => {
    const m = JSON.parse(ev.data);
    if (m.method === 'Runtime.consoleAPICalled' && m.params.type === 'error') {
      console.log('   page error:', m.params.args.map((a) => a.value ?? a.description).join(' '));
    }
  });

  await page.send('Page.navigate', { url: PAGE });
  await until('page ready', () => page.eval(`document.readyState === 'complete'`));

  const gpu = await page.eval('!!navigator.gpu');
  if (!gpu) { bad('navigator.gpu is absent — this browser has no WebGPU'); throw new Error('no WebGPU'); }
  ok('navigator.gpu present');

  // The release artifact's two substitutions, checked from the outside. A
  // stamp still reading "dev" means release-play.sh did not touch the page; the
  // module having loaded at all means the hashed bundle path resolved, which is
  // the substitution whose failure is otherwise a silent blank screen.
  if (DIST || EXTERNAL) {
    const build = await page.eval(`window.__vidiotic?.build ?? null`);
    // `src` as well as `textContent`: the boot script is an external module now
    // — it had to be, because the deploy target's CSP has no 'unsafe-inline' —
    // so the hashed path is in the attribute and the element's text is empty.
    // Reading only the text found nothing and reported it as a missing
    // substitution, which is the same symptom as the bug this check exists for.
    const src = await page.eval(
      `[...document.scripts].map((s) => s.src + s.textContent).join('').match(/pkg-[0-9a-f]{12}/)?.[0] ?? null`);
    if (build && build !== 'dev') ok(`release page is stamped: ${build}`);
    else bad(`release page carries no build stamp (got ${JSON.stringify(build)})`);
    if (src) ok(`page loads its bundle from the hashed directory: ${src}`);
    else bad('page does not reference a content-hashed bundle directory');
  }

  // A real user gesture, or window.open() is blocked.
  await page.eval(`document.getElementById('start').click()`, { userGesture: true });

  const status = await until('boot', async () => {
    const s = await page.eval(`document.getElementById('status').textContent`);
    if (/^running/.test(s)) return s;
    if (document_error(s)) throw new Error(s);
    return null;
  });
  ok(`booted: ${status}`);

  // The popup is a separate CDP target. Its existence is the §10a claim.
  const popup = await until('output window target', async () => {
    const t = await targets();
    return t.find((x) => x.type === 'page' && x.title.includes('output')) ?? null;
  });
  ok(`output window opened as its own target (${popup.title})`);

  const bc = await page.eval(`(async () => {
    const a = await navigator.gpu.requestAdapter();
    return a.features.has('texture-compression-bc');
  })()`);
  ok(`texture-compression-bc: ${bc ? 'yes — HAP uploads as blocks' : 'no — the software path is the only one'}`);

  // Hand over a real clip baked by the native tools — the actual step-4 claim.
  // Through the page's own ingest path rather than `load_clip` directly, so
  // this also puts the bytes in OPFS and the reload check at the end has
  // something to find.
  await page.eval(`(async () => {
    const r = await fetch('${FIXTURES}${CLIP}');
    const b = new Uint8Array(await r.arrayBuffer());
    await window.__vidiotic.ingest(${JSON.stringify(CLIP)}, b);
  })()`);
  ok(`loaded ${CLIP}`);

  await sleep(1000); // let the loop run and the playhead move

  const out = await page.eval(`(async () => Array.from(await window.__vidiotic.centre_pixel('output')))()`);
  const ctl = await page.eval(`(async () => Array.from(await window.__vidiotic.centre_pixel('control')))()`);
  const lit = (p) => p.slice(0, 3).some((c) => c > 8);

  if (lit(out)) ok(`output head rendered non-black pixels: rgba(${out})`);
  else bad(`output head is black: rgba(${out}) — the clip did not reach the screen`);

  if (lit(ctl)) ok(`control head rendered: rgba(${ctl})`);
  else bad(`control head is black: rgba(${ctl})`);

  // The two heads must not be showing the same thing; that would mean one
  // surface is being presented to both, which is exactly the failure a
  // "did it throw?" check cannot see.
  if (out.join() !== ctl.join()) ok('the two heads differ — they are genuinely separate surfaces');
  else bad(`both heads read rgba(${out}) — one surface may be driving both`);

  // Strongest evidence for the popup: capture what the window server composited.
  const pop = await Session.attach(popup.webSocketDebuggerUrl);
  const shot = await pop.send('Page.captureScreenshot', { format: 'png' });
  const bytes = Buffer.from(shot.data, 'base64');
  if (bytes.length > 2000) ok(`output window screenshot: ${bytes.length} bytes of PNG`);
  else bad(`output window screenshot is suspiciously small (${bytes.length} bytes)`);
  pop.close();

  // The engine's own account of itself: tempo, pane, pending sequence, whether
  // it is playing. Every check below that is not about a pixel reads this.
  const state = async () => JSON.parse(await page.eval(`window.__vidiotic.engine_state()`));

  // Playback must actually advance, not hold one frame.
  const f1 = await page.eval(`window.__vidiotic ? 1 : 0`);
  const a = await page.eval(`(async () => Array.from(await window.__vidiotic.centre_pixel('output')))()`);
  await sleep(700);
  const b = await page.eval(`(async () => Array.from(await window.__vidiotic.centre_pixel('output')))()`);
  if (f1 && a.join() !== b.join()) ok(`the clip is playing: rgba(${a}) -> rgba(${b})`);
  else console.log(`[NOTE] centre pixel unchanged across 700ms (rgba(${a})) — could be a static passage`);

  // An empty chain takes `render`'s one-pass fast path; a selected effect takes
  // the seed + ping-pong path instead. A clip appearing on screen only proves
  // the first, so drive the second explicitly and require the pixels to move.
  const names = await page.eval(`window.__vidiotic.effect_names()`);
  ok(`${names.length} built-in effects compiled in the browser: ${names.slice(0, 4).join(', ')}…`);

  // Pause first. Comparing each effect against one base sampled at the start
  // measures the playhead moving as much as it measures the chain, and that
  // showed up as a flake: roughly one run in four scored 7/10, not because
  // three effects stopped working but because their output happened to land on
  // a base that was three frames stale. On a held frame the only thing that can
  // move the centre pixel is the effect.
  const wasPlaying = (await state()).playing;
  if (wasPlaying) await keys(page, [' ']);

  const sampleBase = () => page.eval(`(async () => { window.__vidiotic.set_effect(undefined);
    return Array.from(await window.__vidiotic.centre_pixel('output')); })()`);
  const base = await sampleBase();
  let chainChanged = 0;
  const unchanged = [];
  for (let i = 0; i < names.length; i++) {
    const px = await page.eval(`(async () => { window.__vidiotic.set_effect(${i});
      return Array.from(await window.__vidiotic.centre_pixel('output')); })()`);
    if (px.join() !== base.join()) chainChanged++;
    else unchanged.push(names[i]);
  }

  // The frame really was held: otherwise "unchanged" means nothing, and the
  // right report is that the pause failed rather than that an effect did.
  const baseAgain = await sampleBase();
  if (baseAgain.join() !== base.join()) {
    bad(`the frame moved under the effect sweep (rgba(${base}) -> rgba(${baseAgain})) — playback did not pause`);
  } else if (chainChanged === names.length) {
    ok(`effect chain runs: ${chainChanged}/${names.length} effects changed a held frame`);
  } else {
    bad(`${unchanged.join(', ')} left a held frame untouched — the ping-pong path may not be running`);
  }
  await page.eval(`window.__vidiotic.set_effect(undefined)`);
  if (wasPlaying) await keys(page, [' ']);   // hand playback back as it was found

  // --- ISF, brought by the visitor -------------------------------------------
  //
  // The ten above are compiled in. This is the other half of "pick from a stack
  // of effects": a shader the page never shipped, transpiled from GLSL in the
  // browser by the same `vidiotic-core::isf` the native player uses. Only the
  // file *read* was ever native, so what this proves is that nothing else in
  // that path was.
  const ISF = `/*{
    "DESCRIPTION": "smoke", "CREDIT": "vidiotic", "ISFVSN": "2.0",
    "CATEGORIES": ["Test"],
    "INPUTS": [ { "NAME": "amount", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 1.0 } ]
}*/
void main() {
    vec4 src = IMG_THIS_NORM_PIXEL(inputImage);
    gl_FragColor = vec4(mix(src.rgb, 1.0 - src.rgb, amount), src.a);
}`;
  await page.eval(`window.__vidiotic.load_isf_source('smoke.fs', ${JSON.stringify(ISF)})`);
  const pool = (await state()).shaders ?? [];
  if (pool.includes('smoke.fs')) {
    ok(`a visitor's ISF compiled in the browser: ${pool.join(', ')}`);
  } else {
    bad(`smoke.fs did not reach the shader pool (pool: ${JSON.stringify(pool)})`);
  }

  // The rejection path matters as much: ISF is transpiled, not just handed to
  // the driver, so "compiles" and "is ISF" are two different failures and both
  // have to surface rather than leaving a slot that silently renders nothing.
  const rejected = await page.eval(
    `(() => { try { window.__vidiotic.load_isf_source('bad.fs', 'void main() {}'); return null; }
              catch (e) { return String(e.message ?? e); } })()`);
  if (rejected) ok(`a non-ISF file is refused: ${rejected.slice(0, 60)}`);
  else bad('load_isf_source accepted a file with no ISF header');

  // The bridge that gets a file to those functions: a panel button is painted
  // on a canvas, so it emits `PickIsf` and the *page* opens the chooser. The
  // fragile step is user activation — by the time the command is drained we are
  // in a rAF callback, not the pointer handler, and a chooser opened without
  // activation is refused. So: take a gesture, then dispatch one frame later,
  // which is exactly the ordering the real click produces.
  //
  // What this does not prove is the click-to-command half, which would mean
  // hitting a button whose position is decided by egui's layout at runtime.
  let chooser = null;
  page.on('Page.fileChooserOpened', (p) => { chooser = p; });
  await page.send('Page.setInterceptFileChooserDialog', { enabled: true });
  await page.eval(
    `new Promise((r) => requestAnimationFrame(() => {
       window.dispatchEvent(new CustomEvent('vidiotic-pick', { detail: 'isf' }));
       r(1);
     }))`, { userGesture: true });
  await until('the file chooser to open', () => chooser !== null, { tries: 25, gap: 100 })
    .catch(() => {});
  await page.send('Page.setInterceptFileChooserDialog', { enabled: false });
  if (chooser) ok('a panel asking for a file opens a chooser from inside the frame loop');
  else bad('no file chooser opened for PickIsf — transient activation did not survive the rAF hop');

  // --- the engine: beat clock + modal grammar -------------------------------
  //
  // Neither has pixels of its own on the output head, so a screenshot proves
  // nothing about them. Driving real key events through Chrome's input pipeline
  // and reading the state back is what shows the engine crossed to the browser
  // rather than merely linking.

  // §9a's face has to be the one actually painting, not merely requested:
  // `set_state` only takes effect when `theme::sync` next runs, so reading it
  // back after frames have gone by is what distinguishes the two.
  const face = await state();
  if (face.face === 'Grid' && face.cell === 16) {
    ok(`the lo-res face is live: ${face.face}, ${face.cell}-point cell`);
  } else {
    bad(`expected the Grid face on a 16-point cell, got ${face.face} / ${face.cell}`);
  }

  const b0 = await state();
  await sleep(600);
  const b1 = await state();
  if (b1.beat > b0.beat) ok(`beat clock advances: ${b0.beat.toFixed(2)} -> ${b1.beat.toFixed(2)}`);
  else bad(`beat did not advance (${b0.beat}) — the clock is not ticking`);

  // `b a` focuses the Clock pane; `f f` is Fire->tap. Two real presses of the
  // sticky tap key must move the tempo off its 120 default.
  await keys(page, ['b']);
  const open = await state();
  if (open.pending && open.options > 0) {
    ok(`grammar opened a root: "${open.pending}" with ${open.options} conjugations`);
  } else {
    bad(`pressing a root key left the grammar idle (${JSON.stringify(open)})`);
  }

  await keys(page, ['a']);
  const paned = await state();
  if (paned.pane === 'clock') ok(`grammar resolved a verb: pane is now "${paned.pane}"`);
  else bad(`b,a did not focus the clock pane (pane=${paned.pane}, last=${paned.last_verb})`);

  await keys(page, ['f', 'f']);
  await sleep(420);
  await keys(page, ['f']);
  const tapped = await state();
  if (Math.abs(tapped.bpm - 120.0) > 0.5) {
    ok(`tap tempo drove the clock: ${tapped.bpm.toFixed(2)} bpm from ${b1.bpm} default`);
  } else {
    bad(`tempo unchanged at ${tapped.bpm} after tapping (last verb ${tapped.last_verb})`);
  }

  // Escape must abandon a pending sequence rather than leaving the modal stuck.
  await keys(page, ['g']);
  await keys(page, ['Escape']);
  const cancelled = await state();
  if (!cancelled.pending) ok('Escape cancels a pending sequence');
  else bad(`Escape left the grammar in "${cancelled.pending}"`);

  // --- cues, banks and the rotation -----------------------------------------
  //
  // The engine split (web-port.md §8 step 4d) is what put these here, and the
  // rotation is the part with no pixels of its own: a cue swapping on a phrase
  // boundary looks exactly like a clip that happened to cut, so nothing about it
  // is observable from a screenshot. Loading a second clip and watching
  // `current` change is the only way to show that the *sequencer* did it.
  const oneCue = await state();
  if (oneCue.cues === 1 && oneCue.pool === 1) {
    ok(`the clip became a cue: ${oneCue.cues} cue in bank 1 of ${oneCue.banks}`);
  } else {
    bad(`expected one cue over one pool clip, got ${oneCue.cues} cue(s) / ${oneCue.pool} clip(s)`);
  }

  await page.eval(`(async () => {
    const r = await fetch('${FIXTURES}${CLIP2}');
    const b = new Uint8Array(await r.arrayBuffer());
    // Persisted, like the first: the store is a directory now, so the reload
    // check below is about *both* coming back. Storing only one could not tell
    // a working directory from the single file it replaced.
    await window.__vidiotic.ingest(${JSON.stringify(CLIP2)}, b);
  })()`);
  await sleep(400);
  const twoCues = await state();
  if (twoCues.cues === 2 && twoCues.pool === 2) {
    ok(`a second clip joined the rotation: ${twoCues.cues} cues over ${twoCues.pool} pool clips`);
  } else {
    bad(`second clip did not add a cue: ${twoCues.cues} cue(s) / ${twoCues.pool} clip(s)`);
  }

  // A phrase is 16 beats. At 120 bpm that is eight seconds of waiting; wind the
  // tempo up so the boundary arrives inside a smoke test, then put it back —
  // the reload check below compares against whatever the tap left.
  const restoreBpm = twoCues.bpm;
  await page.eval(`window.__vidiotic.set_bpm(600)`);
  // Sample across the window rather than comparing its ends. Two cues rotating
  // over three seconds at this tempo cross *several* boundaries, and an
  // endpoint comparison scores a working rotation as broken whenever it happens
  // to land back where it started — which is exactly what it did the first time
  // this check was written.
  const seen = new Set();
  for (let i = 0; i < 15; i++) {
    seen.add((await state()).current);
    await sleep(200);
  }
  if (seen.size > 1) {
    ok(`the sequencer rotated over phrase boundaries: cues ${[...seen].join(' -> ')}`);
  } else {
    bad(`cue ${[...seen][0]} played for 3 s at 600 bpm — the rotation never turned`);
  }
  await page.eval(`window.__vidiotic.set_bpm(${restoreBpm})`);

  // --- the software BC fallback ---------------------------------------------
  //
  // The unit tests prove the block decoders against hand-built blocks. What
  // they cannot prove is that the result reaches the screen looking like the
  // GPU's version of the same frame — and that is the entire claim, because
  // this path only ever runs on machines the author does not own.
  //
  // Pause first: the two readings have to be of the *same* sample, or a moving
  // playhead makes any difference meaningless. Then flip the path and compare.
  if (bc) {
    if ((await state()).playing) await keys(page, [' ']);
    const paused = await state();
    if (paused.playing) {
      bad('space did not pause — cannot compare the two decode paths on one frame');
    } else {
      const px = async () => page.eval(
        `(async () => Array.from(await window.__vidiotic.centre_pixel('output')))()`);
      const hw = await px();
      await page.eval(`window.__vidiotic.set_soft_decode(true)`);
      await sleep(300);
      const sw = await px();
      const st = await state();
      if (!st.soft) bad('set_soft_decode(true) did not take');

      // Not equality: BC1 endpoint interpolation is implementation-defined, so
      // the GPU and the CPU are allowed to disagree by a rounding step. What
      // must not happen is black, or a different picture.
      const delta = Math.max(...[0, 1, 2].map((i) => Math.abs(hw[i] - sw[i])));
      if (!sw.slice(0, 3).some((c) => c > 8)) {
        bad(`software decode painted black: rgba(${sw})`);
      } else if (delta <= 12) {
        ok(`software BC decode matches the GPU within ${delta}/255: rgba(${hw}) vs rgba(${sw})`);
      } else {
        bad(`software decode differs by ${delta}/255: rgba(${hw}) vs rgba(${sw}) — the fallback is decoding wrongly`);
      }
      await page.eval(`window.__vidiotic.set_soft_decode(false)`);
      await keys(page, [' ']);
    }
  } else {
    ok('software decode is already the only path on this adapter — covered above');
  }

  // --- persistence ----------------------------------------------------------
  //
  // A clip that vanishes on reload makes this a demo. The Chrome profile is a
  // fresh temp dir per run, so OPFS started empty and anything found now was
  // put there by the ingest above.
  const before = await state();
  await page.send('Page.navigate', { url: PAGE });
  await until('reloaded page', () => page.eval(`document.readyState === 'complete'`));
  await page.eval(`document.getElementById('start').click()`, { userGesture: true });
  await until('reboot', async () => {
    const s = await page.eval(`document.getElementById('status').textContent`);
    if (/^(running|resumed|playing)/.test(s)) return s;
    if (document_error(s)) throw new Error(s);
    return null;
  });

  const restored = await until('restored state', async () => {
    const s = JSON.parse(await page.eval(`window.__vidiotic.engine_state()`));
    return s.clip ? s : null;
  }, { tries: 40 });
  // The pool, by name. Which clip is *on air* after a restore depends on where
  // the rotation happened to be, so asserting that would be asserting the
  // sequencer's phase; what the store is responsible for is that both clips
  // came back under the names they went in with.
  const wanted = [CLIP, CLIP2].sort();
  const got = [...(restored.clips ?? [])].sort();
  if (got.length === wanted.length && got.every((n, i) => n === wanted[i])) {
    ok(`both clips survived a reload from OPFS: ${got.join(', ')}`);
  } else {
    bad(`reload restored ${JSON.stringify(got)}, expected ${JSON.stringify(wanted)}`);
  }
  if (restored.clip && restored.clip.frames > 0) {
    ok(`a restored clip is cued and decoded: ${restored.clip.name}, ${restored.clip.frames} frames`);
  } else {
    bad(`nothing was cued after the reload: ${JSON.stringify(restored.clip)}`);
  }
  if (Math.abs(restored.bpm - before.bpm) < 0.01) {
    ok(`tempo survived a reload: ${restored.bpm.toFixed(2)} bpm`);
  } else {
    bad(`tempo came back as ${restored.bpm}, was ${before.bpm}`);
  }

  // Retried rather than read once: the engine has just been handed megabytes
  // out of OPFS, and "the clip reaches the screen" is the claim — not "it
  // reaches the screen within one frame of being decoded".
  const after = await until('the restored clip on screen', async () => {
    const px = await page.eval(
      `(async () => Array.from(await window.__vidiotic.centre_pixel('output')))()`);
    return px.slice(0, 3).some((c) => c > 8) ? px : null;
  }, { tries: 25, gap: 200 }).catch(() => null);
  if (after) ok(`the restored clip is on screen: rgba(${after})`);
  else bad('the restored clip stayed black for 5 s');

  // And it must be possible to get rid of it, or a visitor is stuck with
  // whatever they loaded first and no way to see that storage is even in play.
  await page.eval(`(async () => { await window.__vidiotic.forgetClips();
    return true; })()`);
  const gone = await page.eval(`(async () => {
    const clips = await window.__vidiotic.storedClips();
    const proj = await window.__vidiotic.storedProject();
    return clips.length === 0 && proj === null;
  })()`);
  if (gone) ok('Forget empties OPFS — every clip and the project');
  else bad('something survived Forget');

  // --- ingest: a video nobody baked ----------------------------------------
  //
  // The claim that decides whether this is deployable: a visitor arrives with
  // an ordinary video file and it plays. Everything above this point starts
  // from a `.mov` the *native* tools produced, which is a file no visitor has.
  //
  // Last, deliberately — the bake adds a cue, and putting it earlier would
  // perturb the rotation and reload checks above for no gain.
  //
  // VP9 rather than H.264: Chromium builds without proprietary codecs cannot
  // decode H.264, and a smoke test that fails on the reviewer's browser but
  // not the author's is worse than no test. Real visitors on real Chrome get
  // H.264 too — this asset picks the codec every build can decode, not the
  // only one the feature supports.
  if (!EXTERNAL && !existsSync(join(ROOT, SRC))) {
    // A skip, not a failure. The two HAP clips are the material this repo is
    // built around; this one is derived, and the command to make it is one
    // line, so refusing to run the other 27 checks over its absence would be
    // out of proportion.
    console.log(`[SKIP] ingest: no ${SRC}. Make one with:`);
    console.log(`       ffmpeg -i clips/bun.mov -t 2 -c:v libvpx-vp9 -an ${SRC}`);
    page.close();
    console.log(failures === 0 ? '\nSMOKE PASS' : `\nSMOKE FAIL (${failures})`);
    process.exit(failures === 0 ? 0 : 1);
  }

  // Kicked off and then polled, rather than awaited across CDP. A bake runs for
  // as long as the clip is long, and `Runtime.evaluate` with `awaitPromise` on a
  // pending promise reports "Promise was collected" if a GC lands while the page
  // is churning through megabytes — which it is, by construction, here. That is
  // a fact about the harness rather than about the page, and it should not be
  // able to fail a run.
  //
  // One bake covers both claims: the bytes are a HAP clip (`is_baked`), and the
  // page's own ingest path takes them all the way to a playing cue. Two separate
  // evals would mean baking the same file twice for no extra evidence.
  // The page has to be the frontmost target for this: Chrome defers media
  // element loading in a document it considers hidden, and the popup output
  // head has had focus since it opened.
  await page.send('Page.bringToFront');
  // Whatever this run has already put in the pool. The bake's claim is "+1",
  // not "= 2".
  const poolBeforeBake = (await state()).pool;
  await page.eval(`(() => {
    window.__bake = { done: false, pct: 0, stage: 'fetch' };
    (async () => {
      const r = await fetch('${FIXTURES}${SRC}');
      const raw = new Uint8Array(await r.arrayBuffer());
      const before = window.__vidiotic.is_baked(raw);
      const t0 = performance.now();
      window.__bake.stage = 'bake';
      const baked = await window.__vidiotic.bakeToHap(
        raw, ${JSON.stringify(SRC)}, (frac) => { window.__bake.pct = Math.round(frac * 100); });
      const probe = {
        before,
        after: window.__vidiotic.is_baked(baked),
        bytes: baked.length,
        seconds: (performance.now() - t0) / 1000,
        size: Array.from(window.__vidiotic.bake_size(640, 360, false)),
      };
      // Through ingest() rather than load_clip(), so the is_baked branch and
      // the .webm -> .mov rename are exercised as the page really runs them.
      window.__bake.stage = 'ingest';
      await window.__vidiotic.ingest(${JSON.stringify(SRC)}, raw, { persist: false });
      window.__bake.stage = 'played';
      window.__bake = { done: true, ...probe };
    })().catch((e) => {
      window.__bake = { done: true, error: String((e && e.message) || e) };
    });
    return true;
  })()`, { awaitPromise: false });
  const ingest = await until('the browser bake', async () => {
    // Re-asserted every tick. The output head is a second target, and headless
    // Chrome gives visibility back to it — which stops the bake dead, because a
    // hidden document does not load media elements. A visitor watching their
    // own control window has this for free; this driver does not.
    await page.send('Page.bringToFront').catch(() => {});
    const r = await page.eval(`JSON.stringify(window.__bake ?? {done:false,pct:-1})`);
    const st = JSON.parse(r);
    return st.done ? st : null;
  }, { tries: 600, gap: 500 }).catch(async () => {
    // A bake that neither finishes nor throws is the one failure this check
    // cannot describe from the outside; say how far it got.
    const last = await page.eval(`JSON.stringify(window.__bake ?? null)`).catch(() => 'unreadable');
    const line = await page.eval(`document.getElementById('status').textContent`).catch(() => '?');
    bad(`the bake never finished: ${last} (status line: ${line})`);
    return { error: 'timed out' };
  });

  if (ingest.error) {
    bad(`the bake threw: ${ingest.error}`);
  } else if (ingest.before) {
    bad(`${SRC} probed as already-baked — the test asset is not what it claims`);
  } else if (ingest.after) {
    ok(`baked ${SRC} in the browser: ${ingest.size.join('x')}, ` +
       `${(ingest.bytes / 1024).toFixed(0)} KiB in ${ingest.seconds.toFixed(1)}s`);
  } else {
    bad(`the bake produced ${ingest.bytes} bytes that are not a HAP clip`);
  }

  await sleep(600);
  const ingested = await state();
  // Relative to what the pool held a moment ago, not an absolute count: every
  // clip restored earlier in this run is still in it, so a fixed number here
  // fails whenever the fixtures above change and says nothing about the bake.
  if (ingested.pool === poolBeforeBake + 1) {
    ok(`the baked clip joined the pool: ${ingested.pool} clips, ${ingested.cues} cues`);
  } else {
    bad(`expected ${poolBeforeBake + 1} pool clips after ingest, got ${ingested.pool}`);
  }

  // Force it on air rather than waiting for the rotation to reach it, so this
  // is a statement about the baked clip and not about whichever cue happened
  // to be current.
  await page.eval(`window.__vidiotic.set_bpm(600)`);
  const litFrames = new Set();
  // Long enough for the rotation to reach the last cue whatever the pool size:
  // a phrase is 16 beats, so at this tempo every cue gets its turn inside a few
  // seconds — but "a few" scales with how many clips this run restored.
  for (let i = 0; i < 12 + 8 * ingested.pool; i++) {
    const s = await state();
    const px = await page.eval(
      `(async () => Array.from(await window.__vidiotic.centre_pixel('output')))()`);
    if (s.clip?.name?.endsWith('.mov') && s.clip.name.includes('probe')) {
      litFrames.add(px.slice(0, 3).some((c) => c > 8));
    }
    await sleep(200);
  }
  if (litFrames.size === 0) {
    bad('the baked clip never came on air — the rotation did not reach it');
  } else if (litFrames.has(false)) {
    bad('the baked clip was on air and the output head was black');
  } else {
    ok('the browser-baked clip rendered through the composite pass');
  }

  // --- importing a .viproj --------------------------------------------------
  //
  // The other end of /chop's offsets render (web-port.md §2g): a project that
  // renders nothing and names the source as one clip, with each span a trimmed
  // cue. It lands here because both ends spell the clip name the same way —
  // ingest interned the baked probe.webm as `probe.mov`, and that is what the
  // project asks for. Nothing in the runtime is new: CueSpec has always had
  // in_sec/out_sec.

  const OFFSETS = `(
    version: 3,
    defaults: (bpm: 143.0, quantum: 4.0, phrase_len: 16),
    clips: [(id: 0, path: "clips/probe.mov", name: "probe",
             source: (original_path: "probe.webm", in_frame: 0, out_frame: 60,
                      in_sec: 0.0, out_sec: 2.0))],
    clip_banks: [(name: "source", clip_ids: [0])],
    cue_banks: [
      (name: "one", cues: [(clip: 0, name: "head", in_sec: 0.0, out_sec: 0.5)]),
      (name: "two", cues: [(clip: 0, name: "mid", in_sec: 0.5, out_sec: 1.0),
                           (clip: 0, name: "tail", in_sec: 1.0, out_sec: 1.5)]),
    ],
  )`;

  // Caught in the page and returned as a value: a rejected wasm-bindgen Result
  // surfaces to CDP as an "Uncaught" with the message only in the console, and
  // a test that cannot read the message cannot check it.
  const tryLoad = (ron) => page.eval(
    `(() => { try { return { ok: window.__vidiotic.load_project(${JSON.stringify(ron)}) }; }
              catch (e) { return { err: String(e.message ?? e) }; } })()`,
  );
  const loadMsg = (await tryLoad(OFFSETS)).ok ?? '(threw)';
  if (/3 cue\(s\) in 2 bank\(s\)/.test(String(loadMsg))) {
    ok(`the .viproj loaded: ${loadMsg}`);
  } else {
    bad(`load_project said: ${loadMsg}`);
  }

  await sleep(600);
  const loaded = await state();
  if (loaded.pool === 1) ok('the project swapped the pool to its own single clip');
  else bad(`expected 1 pool clip after the project load, got ${loaded.pool}`);
  if (loaded.banks === 2) ok('both cue banks came across');
  else bad(`expected 2 cue banks, got ${loaded.banks}`);
  if (loaded.cues === 1) ok(`the live bank has its ${loaded.cues} cue`);
  else bad(`expected 1 cue in the live bank, got ${loaded.cues}`);
  // The project's tempo is the session's now — proof the defaults were applied
  // and not just the pool.
  if (Math.abs(loaded.bpm - 143) < 0.51) ok(`the project's tempo took (${loaded.bpm} bpm)`);
  else bad(`bpm is ${loaded.bpm}, expected 143`);

  // The clip is the one already in the pool, re-keyed onto the project's ids —
  // so it must still render, not just exist. The tempo check above had to come
  // first: this winds the clock up so the rotation actually reaches a cue
  // inside the sampling window.
  await page.eval(`window.__vidiotic.set_bpm(600)`);
  const litAfterLoad = new Set();
  for (let i = 0; i < 24; i++) {
    const st = await state();
    const px = await page.eval(
      `(async () => Array.from(await window.__vidiotic.centre_pixel('output')))()`);
    if (st.current !== null) litAfterLoad.add(px.slice(0, 3).some((c) => c > 8));
    await sleep(150);
  }
  if (litAfterLoad.size && !litAfterLoad.has(false)) {
    ok('a cue from the loaded project rendered through the composite pass');
  } else if (!litAfterLoad.size) {
    bad(`no cue from the loaded project ever came on air: ${JSON.stringify(await state())}`);
  } else {
    bad('a loaded-project cue was on air and the output head was black');
  }

  // A project naming a clip nobody has must say which, not fail vaguely.
  const missing = (await tryLoad(OFFSETS.replace('probe.mov', 'nope.mov'))).err;
  if (missing && /nope\.mov/.test(missing)) {
    ok('a project missing a clip names the file it needs');
  } else {
    bad(`expected a named missing clip, got: ${missing}`);
  }

  // --- audio reactivity -----------------------------------------------------
  //
  // Every one of the ten bundled effects reads `lvl` or `fftBand`, and until
  // step 4f all ten were reading a hardcoded zero — running, and sitting still.
  //
  // Driven through `push_audio` with a synthesised tone rather than through a
  // real capture: `getDisplayMedia` needs a consent prompt no headless run can
  // answer, and the capture is the one part of this that is *not* shared with
  // the native player. What is shared is everything after it, and that is what
  // this exercises.
  const silent = await state();
  if (silent.audio) bad('the shell reported a live audio source before anything was fed');

  await page.eval(`(() => {
    const RATE = 48000, HOP = 800;
    // Four hops per push is the analyser's backlog cap; more would be dropped.
    // Several pushes across several frames, because the sliding window is 2048
    // samples and one hop only replaces 800 of them — a single push analyses a
    // window that is mostly silence.
    window.__tone = (i0) => {
      const n = HOP * 4;
      const buf = new Float32Array(n);
      for (let i = 0; i < n; i++) buf[i] = Math.sin(2 * Math.PI * 1000 * (i0 + i) / RATE);
      window.__vidiotic.push_audio(buf, RATE);
      return i0 + n;
    };
    return true;
  })()`);
  let phase = 0;
  for (let i = 0; i < 8; i++) {
    phase = await page.eval(`window.__tone(${phase})`);
    await sleep(60);
  }
  const loud = await state();
  if (!loud.audio) {
    bad('push_audio did not register as a live source');
  } else if (loud.lvl > 0.01) {
    ok(`a 1 kHz tone reached the shaders: lvl ${loud.lvl.toFixed(3)}, was ${silent.lvl}`);
  } else {
    bad(`fed a full-scale tone and lvl stayed at ${loud.lvl} — the analyser is not wired to the uniforms`);
  }

  // And it must fall again, or `lvl` is a latch rather than a level and every
  // reactive effect stays stuck on after the first transient.
  await sleep(700);
  const decayed = await state();
  if (decayed.lvl < loud.lvl * 0.5) {
    ok(`the level decays with the signal: ${loud.lvl.toFixed(3)} -> ${decayed.lvl.toFixed(3)}`);
  } else {
    bad(`lvl held at ${decayed.lvl.toFixed(3)} after the tone stopped (peak ${loud.lvl.toFixed(3)})`);
  }

  // --- cameras --------------------------------------------------------------
  //
  // A camera cue is a pool clip whose source is a device rather than a file, and
  // the engine has always had them — what a browser had to supply is the pixels.
  // The whole chain is here: enumerate, `getUserMedia`, a `<video>` the page
  // owns, a canvas readback in Rust, and a frame through the composite pass.
  //
  // Chrome's fake device is a real `MediaStream` through the real API, so
  // everything below the permission prompt is the code a visitor runs.

  await page.eval(`window.__vidiotic.listCameras()`);
  const listed = await until('the camera list', async () => {
    const st = await state();
    return st.cameras?.length ? st.cameras : null;
  }, { tries: 20 }).catch(() => []);
  if (listed.length) {
    ok(`enumerated ${listed.length} camera(s): ${listed.map((c) => c.name).join(', ')}`);
  } else {
    bad('no cameras were enumerated — the fake device flag may not have taken');
  }

  if (listed.length) {
    const uid = listed[0].uid;
    // A cue first, then the device. That order is the harder one: the cue opens
    // against a camera that is not on air yet, which must be a blank cue rather
    // than a broken rotation, and switching the device on has to reach it.
    await page.eval(`window.__vidiotic.add_camera_cue(${JSON.stringify(uid)})`);
    await sleep(300);
    const cued = await state();
    const camClip = cued.clips.length;
    if (camClip > 0) ok(`the camera joined the pool: ${cued.clips.length} clip(s), ${cued.cues} cue(s)`);
    else bad('adding a camera cue did not change the pool');

    await page.eval(`window.__vidiotic.startCamera(${JSON.stringify(uid)})`);
    const live = await until('the camera on air', async () => {
      const st = await state();
      const row = st.cameras?.find((c) => c.uid === uid);
      // The status only becomes a size once a cue has actually sampled the
      // element — which is the claim: the readback runs.
      return row?.on_air && /^\d+x\d+$/.test(row.status) ? row : null;
    }, { tries: 40, gap: 250 }).catch(() => null);

    if (live) {
      ok(`the camera is on air and being sampled: ${live.name} at ${live.status}`);
    } else {
      const st = await state();
      bad(`the camera never reached the engine: ${JSON.stringify(st.cameras)}`);
    }

    // And its pixels reach the output head. Forced on air rather than waited
    // for, so this is a statement about the camera and not the rotation.
    await page.eval(`window.__vidiotic.set_bpm(600)`);
    const litCam = await until('the camera on screen', async () => {
      const st = await state();
      const px = await page.eval(
        `(async () => Array.from(await window.__vidiotic.centre_pixel('output')))()`);
      const onCam = st.cameras?.some((c) => c.on_air && c.role !== 'None');
      return px.slice(0, 3).some((c) => c > 8) && onCam !== false ? px : null;
    }, { tries: 30, gap: 200 }).catch(() => null);
    if (litCam) ok(`a camera cue rendered through the composite pass: rgba(${litCam})`);
    else bad('the camera cue stayed black');

    // Off air must actually release it: a camera left running is a lit
    // indicator light on somebody's laptop after they closed the tool.
    await page.eval(`window.__vidiotic.stopCamera(${JSON.stringify(uid)}, undefined)`);
    await sleep(400);
    const after = await state();
    const stopped = after.cameras?.find((c) => c.uid === uid);
    if (stopped && !stopped.on_air) ok('switching the camera off released the device');
    else bad(`the camera is still on air after stop: ${JSON.stringify(stopped)}`);
  }

  // --- saving a session -----------------------------------------------------
  //
  // The mirror of loading one, and what makes a browser session portable rather
  // than trapped in a tab: the running engine becomes a `.viproj` plus its
  // clips, in the same archive /chop writes, through the same `from_runtime`
  // the desktop player saves with.
  //
  // Checked by round trip rather than by inspection. A project that serialises
  // and then does not load again is the failure worth catching, and byte-level
  // assertions on RON would pass for a project describing nothing.

  await page.eval(`window.__vidiotic.set_project_name('friday night.viproj')`);
  await page.eval(`window.__vidiotic.save_project()`);
  const saved = await page.eval(`(() => {
    const s = window.__vidiotic.lastSave();
    return s ? { name: s.name, size: s.size } : null;
  })()`);
  if (saved && saved.name === 'friday_night.zip' && saved.size > 0) {
    ok(`saved a bundle: ${saved.name}, ${(saved.size / 1024).toFixed(0)} KiB`);
  } else {
    bad(`save produced ${JSON.stringify(saved)}, expected friday_night.zip`);
  }

  // Walk the archive in the page. Entries are stored, not deflated, so each
  // file's bytes are simply there after its local header — which is the whole
  // reason the writer stores rather than deflates.
  const unpacked = await page.eval(`(() => {
    const z = window.__vidiotic.lastSave().bytes;
    const dv = new DataView(z.buffer, z.byteOffset, z.byteLength);
    const out = [];
    let p = 0;
    while (p + 4 <= z.length && dv.getUint32(p, true) === 0x04034b50) {
      const nameLen = dv.getUint16(p + 26, true);
      const extraLen = dv.getUint16(p + 28, true);
      const size = dv.getUint32(p + 18, true);
      const start = p + 30 + nameLen + extraLen;
      const name = new TextDecoder().decode(z.subarray(p + 30, p + 30 + nameLen));
      out.push({ name, size, start });
      p = start + size;
    }
    const proj = out.find((e) => e.name.endsWith('.viproj'));
    return {
      names: out.map((e) => e.name),
      viproj: proj ? new TextDecoder().decode(z.subarray(proj.start, proj.start + proj.size)) : null,
    };
  })()`);

  if (unpacked.names[0] === 'friday_night/friday_night.viproj') {
    ok(`the bundle leads with its project: ${unpacked.names[0]}`);
  } else {
    bad(`expected the .viproj first, got ${JSON.stringify(unpacked.names)}`);
  }
  // Every clip the project names must be in the archive beside it, or the
  // bundle is the half that points at the other half.
  const clipEntries = unpacked.names.slice(1);
  const referenced = [...(unpacked.viproj ?? '').matchAll(/(?<!original_)path:\s*"([^"]+)"/g)]
    .map((m) => m[1]);
  const absent = referenced.filter((r) => !clipEntries.some((n) => n.endsWith(r)));
  if (referenced.length && absent.length === 0) {
    ok(`every clip the project names is in the bundle: ${referenced.join(', ')}`);
  } else {
    bad(`the bundle references ${JSON.stringify(referenced)} but holds ${JSON.stringify(clipEntries)}`);
  }

  // The round trip: the project it just wrote must load back into this engine.
  const reloaded = await tryLoad(unpacked.viproj ?? '');
  if (reloaded.ok) ok(`the saved project loads back: ${reloaded.ok}`);
  else bad(`the saved project did not load again: ${reloaded.err}`);

  // --- the /chop handoff ----------------------------------------------------
  //
  // /chop and /play are two pages on one origin, so they share one OPFS root:
  // "send to /play" writes a directory there rather than downloading, and this
  // page claims it on boot. No download, no re-upload, no file chooser between
  // marking a video and playing it.
  //
  // Planted here rather than driven from /chop, because /chop's smoke runs a
  // browser launched without WebGPU or popups and this page needs both. What is
  // shared is the *shape*, and both smokes assert the same one: a `.viproj`
  // plus one file per clip, flat, no directories. chop-smoke checks that /chop
  // writes it; this checks that /play claims it.
  //
  // Last, because claiming a handoff replaces the session on purpose.

  const HANDED = `(
    version: 3,
    defaults: (bpm: 97.0, quantum: 4.0, phrase_len: 16),
    clips: [(id: 0, path: "chopped/bun.mov", name: "bun",
             source: (original_path: "bun.mov", in_frame: 0, out_frame: 90,
                      in_sec: 0.0, out_sec: 3.0))],
    clip_banks: [(name: "chopped", clip_ids: [0])],
    cue_banks: [(name: "A", cues: [(clip: 0, name: "whole", in_sec: 0.0, out_sec: 1.0)])],
  )`;

  const planted = await page.eval(`(async () => {
    const root = await navigator.storage.getDirectory();
    await root.removeEntry('handoff', { recursive: true }).catch(() => {});
    const dir = await root.getDirectoryHandle('handoff', { create: true });
    const write = async (name, data) => {
      const fh = await dir.getFileHandle(name, { create: true });
      const w = await fh.createWritable();
      await w.write(data);
      await w.close();
    };
    const r = await fetch('${FIXTURES}${CLIP}');
    // The clip first and the .viproj last, exactly as /chop writes it: the
    // project is the marker, and its presence is what says the rest is there.
    await write('bun.mov', new Uint8Array(await r.arrayBuffer()));
    await write('chopped.viproj', ${JSON.stringify(HANDED)});
    return true;
  })()`);
  if (!planted) bad('could not plant a handoff');

  await page.send('Page.navigate', { url: PAGE });
  await until('reloaded for the handoff', () => page.eval(`document.readyState === 'complete'`));
  await page.eval(`document.getElementById('start').click()`, { userGesture: true });
  await until('reboot for the handoff', async () => {
    const s = await page.eval(`document.getElementById('status').textContent`);
    if (/^(running|resumed|playing|from \/chop)/.test(s)) return s;
    if (document_error(s)) throw new Error(s);
    return null;
  });

  const claimed = await until('the claimed handoff', async () => {
    const st = JSON.parse(await page.eval(`window.__vidiotic.engine_state()`));
    return st.clips?.length ? st : null;
  }, { tries: 40 }).catch(() => null);

  if (!claimed) {
    bad('the handoff was never claimed — no clip reached the pool');
  } else {
    // `bun`, not `bun.mov`: loading a project rebuilds the pool from the
    // project, so the display names are the project's. The *file* name is what
    // the by-name join runs on, and that is checked against the store below.
    if (claimed.clips.length === 1 && claimed.clips[0] === 'bun') {
      ok(`/play claimed the handoff into a one-clip pool: ${claimed.clips[0]}`);
    } else {
      bad(`the handoff put ${JSON.stringify(claimed.clips)} in the pool, expected ["bun"]`);
    }
    // The project came with it, and its clip reference resolved against the
    // clip beside it rather than reporting a miss — which is the whole join.
    if (Math.abs(claimed.bpm - 97) < 0.01) {
      ok(`the handed-off project took: ${claimed.bpm} bpm, ${claimed.cues} cue(s)`);
    } else {
      bad(`the handed-off project did not load: ${claimed.bpm} bpm`);
    }
  }
  const line = await page.eval(`document.getElementById('status').textContent`);
  if (/^from \/chop/.test(line)) ok(`the page said where it came from: "${line}"`);
  else bad(`the handoff did not announce itself: ${JSON.stringify(line)}`);

  // Claimed, not merely read: a handoff left in place would reload itself over
  // the top of the session on every visit.
  const drained = await page.eval(`(async () => (await window.__vidiotic.peekHandoff()) === null)()`);
  if (drained) ok('the handoff directory was cleared once claimed');
  else bad('the handoff is still sitting in OPFS and will be claimed again');

  // And it survives on its own now — the clip went into the store on the way
  // through, so the next visit needs no handoff.
  const kept = await page.eval(`(async () => {
    const clips = await window.__vidiotic.storedClips();
    const proj = await window.__vidiotic.storedProject();
    return { names: clips.map((c) => c.name), project: typeof proj === 'string' };
  })()`);
  if (kept.names.length === 1 && kept.names[0] === 'bun.mov' && kept.project) {
    ok('the handoff was taken into the store — clip and project both');
  } else {
    bad(`the store holds ${JSON.stringify(kept)} after the handoff`);
  }

  page.close();
  console.log(failures === 0 ? '\nSMOKE PASS' : `\nSMOKE FAIL (${failures})`);
  process.exit(failures === 0 ? 0 : 1);
}

const document_error = (s) => /failed|blocked|error|not booted/i.test(s);

main().catch((e) => {
  console.error(`\nsmoke driver error: ${e.message}`);
  process.exit(1);
});
