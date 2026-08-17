#!/usr/bin/env node
// chop-smoke — drive /chop in a real browser and prove the editor works there.
//
// The sibling of play-smoke.mjs, and it exists for the same reason: the wasm
// gate proves `vidiotic-chop` *compiles* for wasm32 and that its unit tests pass
// in V8, and neither can tell you that a canvas showed a timeline or that a
// keypress reached the command queue. Twice on this port the target's compiler
// was the only honest reviewer; the thing after the compiler is a browser.
//
// What is checked, and each can fail on its own:
//   1. the module boots and egui painted something — a screenshot with more
//      than one distinct colour in it, because a blank canvas and a working one
//      are otherwise indistinguishable from here;
//   2. a real video opens: the page probes it, the editor takes its shape, and
//      a decoded frame comes back through the seek->canvas->wasm path;
//   3. keys resolve through the shared table — i / o / Enter marks a span,
//      dispatched through Chrome's own input pipeline rather than a synthetic
//      event, so the egui->vidiotic-ctl key spelling is exercised for real;
//   4. undo is wired to the same chord it has natively;
//   5. a `.viproj` reopens into spans without its source video present, which
//      is the browser-only ordering and the one most likely to rot;
//   6. a `Pick*` command opens a real file chooser from inside a rAF callback,
//      which is the one part of the page bridge no unit test can reach.
//
// Not checked, and worth saying so rather than leaving it to be assumed:
// `StartExport` and `ConfirmQuit` only report that they have no browser answer,
// and both are reachable only by clicking through a dialog. A coordinate-driven
// click into an egui window is a worse test than none — it fails when a button
// moves, which tells you nothing about whether the refusal still works.
//
// No npm: Node 22+ has a built-in WebSocket and fetch, and Chrome speaks CDP.
//
// Usage:  node scripts/chop-smoke.mjs [--headful] [--keep] [--url BASE]

import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
// Deliberately not play-smoke's 8123: the two suites are run back to back, and
// a lingering server from one silently serving the other is the exact trap that
// invalidated a run of play-smoke once already.
const PORT = 8124;
const CDP_PORT = 9224;
// A short unbaked source — the file a visitor actually turns up with. The page
// opens it with a <video> element, so what matters is that Chrome can decode it.
const SRC = 'clips/probe.webm';
const HEADFUL = process.argv.includes('--headful');
const KEEP = process.argv.includes('--keep');

const URL_ARG = process.argv[process.argv.indexOf('--url') + 1];
const EXTERNAL = process.argv.includes('--url') ? URL_ARG?.replace(/\/$/, '') : null;
if (process.argv.includes('--url') && !EXTERNAL) {
  console.error('--url needs a base URL, e.g. --url http://localhost:8080');
  process.exit(2);
}
const PAGE = EXTERNAL ? `${EXTERNAL}/chop.html` : `http://127.0.0.1:${PORT}/web/chop.html`;
const FIXTURES = EXTERNAL ? `${EXTERNAL}/` : '/';

// Chrome, by env override or by platform default. `CHROME` exists because the
// only reliable answer on a CI runner (or a Linux desktop) is "wherever the
// image put it": the defaults below are the ordinary install paths, and a
// hosted runner is free to disagree with all of them.
const CHROME = process.env.CHROME || (process.platform === 'darwin'
  ? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
  : ['/usr/bin/google-chrome', '/usr/bin/google-chrome-stable',
     '/usr/bin/chromium', '/usr/bin/chromium-browser',
     '/opt/google/chrome/chrome'].find(existsSync) || '/usr/bin/google-chrome');

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

  on(method, fn) { this.events.set(method, fn); }

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
 * spelling egui derives from it, and a hand-built event would let the adapter
 * agree with a mistake the real pipeline never makes. That spelling is the
 * whole reason `vidiotic_ctl::keys::from_named` exists.
 */
async function keys(page, names) {
  for (const key of names) {
    const single = key.length === 1;
    const code = key === ' ' ? 'Space' : single ? `Key${key.toUpperCase()}` : key;
    await page.send('Input.dispatchKeyEvent', {
      type: single ? 'keyDown' : 'rawKeyDown',
      key,
      code,
      windowsVirtualKeyCode: single ? key.toUpperCase().charCodeAt(0) : (key === 'Enter' ? 13 : 27),
      ...(single ? { text: key } : {}),
      ...(key === 'Enter' ? { text: '\r' } : {}),
    });
    await page.send('Input.dispatchKeyEvent', { type: 'keyUp', key, code });
    // egui drains input once per frame, and the shell only repaints on demand;
    // a gap short enough to coalesce two presses into one frame would make this
    // test's failures depend on the machine it runs on.
    await sleep(120);
  }
}

/** The editor's own view of itself. Empty until the first frame is drawn. */
const state = (page) => page.eval('window.__chop.editor_state()').then((s) => (s ? JSON.parse(s) : null));

/** Click the canvas, so egui has focus and keys are delivered to it. */
async function focusCanvas(page) {
  const box = await page.eval(
    'JSON.stringify((() => { const r = document.getElementById("chop").getBoundingClientRect(); return {x: r.x + 40, y: r.y + r.height - 40}; })())',
  );
  const { x, y } = JSON.parse(box);
  for (const type of ['mousePressed', 'mouseReleased']) {
    await page.send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
  }
  await sleep(150);
}

async function main() {
  if (EXTERNAL) {
    const r = await fetch(PAGE).catch((e) => e);
    if (!(r instanceof Response) || !r.ok) {
      console.error(`nothing serving ${PAGE}`);
      process.exit(2);
    }
  } else {
    if (!existsSync(join(ROOT, 'web/pkg-chop/vidiotic_chop.js'))) {
      console.error('web/pkg-chop is missing — run scripts/build-chop.sh first');
      process.exit(2);
    }
    if (!existsSync(join(ROOT, SRC))) {
      console.error(`${SRC} is not in this checkout; nothing to open`);
      process.exit(2);
    }
    // The stale-server trap, in the form play-smoke learned it: the spawn below
    // is `stdio: 'ignore'`, so a port already bound fails silently and every
    // check then runs against somebody else's files. Refuse instead.
    const stale = await fetch(`http://127.0.0.1:${PORT}/`, { method: 'HEAD' })
      .then(() => true).catch(() => false);
    if (stale) {
      console.error(`something is already serving 127.0.0.1:${PORT} — stop it first`);
      console.error('(a stale server would silently serve a different checkout than the one under test)');
      process.exit(2);
    }
  }
  if (!existsSync(CHROME)) {
    console.error(`no Chrome at ${CHROME} — set CHROME to its path`);
    process.exit(2);
  }

  // Rooted at the repo, so the page can fetch clips/ the same way a visitor
  // hands a file over through the input.
  const server = EXTERNAL ? null
    : spawn('python3', ['-m', 'http.server', '-d', ROOT, String(PORT)], { stdio: 'ignore' });
  const profile = mkdtempSync(join(tmpdir(), 'chop-smoke-'));
  const chrome = spawn(CHROME, [
    `--remote-debugging-port=${CDP_PORT}`,
    `--user-data-dir=${profile}`,
    ...(HEADFUL ? [] : ['--headless=new']),
    // WebGL2 in headless needs a rasterizer that exists there; /chop paints
    // egui through glow, so this is the whole graphics requirement.
    '--use-gl=angle',
    '--use-angle=swiftshader',
    '--no-first-run',
    '--no-default-browser-check',
    '--window-size=1400,900',
    PAGE,
  ], { stdio: 'ignore' });

  let page = null;
  try {
    const target = await until('the page', async () => (await targets()).find((t) => t.url.includes('chop.html')));
    page = await Session.attach(target.webSocketDebuggerUrl);
    await page.send('Runtime.enable');
    await page.send('Page.enable');

    // --- 1. it booted, and it painted -------------------------------------

    await until('the wasm module', () => page.eval('!!window.__chop'), { tries: 150 });
    ok('the module booted and exported its handle');

    const noteGone = await page.eval('!document.getElementById("boot-note")');
    if (noteGone) ok('the boot note was removed, so the import resolved');
    else bad('the boot note is still up — chop.js threw on import');

    const first = await until('the first frame', () => state(page), { tries: 150 });
    if (first && first.frames === 0 && first.spans.length === 0) {
      ok('the editor reports an empty session before anything is opened');
    } else {
      bad(`unexpected initial state: ${JSON.stringify(first)}`);
    }

    // A screenshot, because "no exception was thrown" is not evidence that a
    // canvas showed anything. egui's panels are several colours; a canvas that
    // failed to acquire a context is exactly one.
    const shot = await page.send('Page.captureScreenshot', { format: 'png' });
    const distinct = await page.eval(`(async () => {
      const img = new Image();
      img.src = 'data:image/png;base64,${shot.data}';
      await img.decode();
      const c = document.createElement('canvas');
      c.width = img.width; c.height = img.height;
      const x = c.getContext('2d');
      x.drawImage(img, 0, 0);
      const d = x.getImageData(0, 0, c.width, c.height).data;
      const seen = new Set();
      for (let i = 0; i < d.length; i += 4 * 97) {
        seen.add((d[i] << 16) | (d[i + 1] << 8) | d[i + 2]);
        if (seen.size > 8) break;
      }
      return seen.size;
    })()`);
    if (distinct > 2) ok(`the canvas painted (${distinct}+ distinct colours sampled)`);
    else bad(`the canvas looks blank — ${distinct} distinct colour(s)`);

    // --- 2. a real video opens, and a frame comes back --------------------

    await page.eval(`(async () => {
      const r = await fetch('${FIXTURES}${SRC}');
      if (!r.ok) throw new Error('fetch ${SRC}: ' + r.status);
      const b = await r.blob();
      await window.__chop.openVideo(new File([b], 'probe.webm', { type: 'video/webm' }));
    })()`);

    const opened = await until('the video to open', async () => {
      const s = await state(page);
      return s && s.frames > 1 ? s : null;
    });
    if (opened.source === 'probe.webm') ok(`the editor took the source (${opened.frames} frames @ ${opened.fps} fps)`);
    else bad(`source is ${JSON.stringify(opened.source)}`);
    // 2 s at 30 fps. The count is the page's convention, not the file's, so
    // this is checking that both ends agree on it rather than probing a codec.
    if (opened.frames >= 55 && opened.frames <= 65) ok('frame count matches the constant-rate convention');
    else bad(`${opened.frames} frames is not ~60 — the page and the Rust disagree on the rate`);
    if (opened.out === opened.frames) ok('the out mark opened at the end of the source');
    else bad(`out mark is ${opened.out}, expected ${opened.frames}`);

    const previewed = await until('a decoded frame', async () => {
      const s = await state(page);
      return s?.preview ? s : null;
    }, { tries: 150 });
    if (previewed) ok('a frame came back through the seek → canvas → wasm path');

    // --- 3. keys reach the command queue ----------------------------------

    await focusCanvas(page);
    // Marks at the playhead, then a span from them. `a` rather than Enter for
    // the add, because both are bound and `a` is the one that cannot be
    // confused with a text field committing.
    await keys(page, ['ArrowRight', 'ArrowRight', 'i']);
    const afterIn = await state(page);
    if (afterIn.cur === 2) ok('the arrow key stepped the playhead');
    else bad(`playhead is at ${afterIn.cur} after two steps, expected 2`);
    if (afterIn.in === 2) ok('"i" set the in mark through the shared key table');
    else bad(`in mark is ${afterIn.in}, expected 2`);

    await keys(page, ['ArrowRight', 'ArrowRight', 'ArrowRight', 'o', 'a']);
    const marked = await until('a span', async () => {
      const s = await state(page);
      return s.spans.length > 0 ? s : null;
    });
    if (marked.spans.length === 1) {
      const sp = marked.spans[0];
      ok(`"a" added a span [${sp.in}..${sp.out}) on ${sp.source}`);
      if (sp.in === 2 && sp.out === 6) ok('the span took both marks');
      else bad(`span is [${sp.in}..${sp.out}), expected [2..6)`);
    } else {
      bad(`${marked.spans.length} spans after marking one`);
    }

    // --- 4. undo is the same chord it is natively -------------------------

    await page.send('Input.dispatchKeyEvent', {
      type: 'keyDown', key: 'z', code: 'KeyZ', modifiers: 2, windowsVirtualKeyCode: 90,
    });
    await page.send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'z', code: 'KeyZ', modifiers: 2 });
    const undone = await until('the undo', async () => {
      const s = await state(page);
      return s.spans.length === 0 ? s : null;
    }, { tries: 30 });
    if (undone) ok('ctrl+z removed the span — undo is wired to the reserved chord');

    // --- 5. the export bakes a real clip and hands back a real zip --------
    //
    // The end of the whole port: the same compressor and muxer the desktop
    // exporter runs, driven from a page, producing the project format that one
    // `assemble` writes for both shells.

    // Three spans, not one. "One video" is a limit on *sources*, not on cuts:
    // slicing one source into many clips is the entire job, and an export loop
    // that only ever ran once in a test is an export loop that works once.
    for (let n = 0; n < 3; n++) {
      await keys(page, ['i', 'ArrowRight', 'ArrowRight', 'ArrowRight', 'ArrowRight', 'o', 'a']);
      await keys(page, ['ArrowRight', 'ArrowRight']);
    }
    const toBake = await until('three spans to export', async () => {
      const st = await state(page);
      return st.spans.length === 3 ? st : null;
    });
    if (toBake) {
      ok(`marked 3 spans to export: ${toBake.spans.map((sp) => `[${sp.in}..${sp.out})`).join(' ')}`);
      const overlap = toBake.spans.some((a, i) =>
        toBake.spans.some((b, j) => j > i && a.in < b.out && b.in < a.out));
      if (!overlap) ok('the three spans are distinct ranges of the one source');
      else bad('the marked spans overlap; the test is not checking what it thinks');
    }

    await page.eval('window.__chop.start_export()', { awaitPromise: false });
    const zipInfo = await until('the export', async () => {
      const r = await page.eval('JSON.stringify(window.__chop.lastExport())');
      return r && r !== 'null' ? JSON.parse(r) : null;
    }, { tries: 200, gap: 250 }).catch(async () => {
      const st = await state(page);
      bad(`export never finished; status: ${JSON.stringify(st.status)} error=${st.error}`);
      return null;
    });
    if (zipInfo) ok(`the export produced ${zipInfo.name} (${Math.round(zipInfo.size / 1024)} KiB)`);

    const after = await state(page);
    if (/exported project\.zip/.test(after.status) && !after.error) {
      ok(`the shell reported it: "${after.status}"`);
    } else {
      bad(`unexpected post-export status: ${JSON.stringify(after.status)}`);
    }

    // Parse the archive the visitor would actually receive. Stored entries, so
    // walking the central directory is enough — and it is the only way to know
    // the .viproj and the clips ended up under one root with the relative paths
    // the project references. A zip that unpacks to the wrong shape is a
    // project that loads and finds no clips.
    const listing = await page.eval(`(() => {
      const bytes = window.__chop.lastZip();
      const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      // Locate the end-of-central-directory record.
      let end = -1;
      for (let i = bytes.length - 22; i >= 0; i--) {
        if (dv.getUint32(i, true) === 0x06054b50) { end = i; break; }
      }
      if (end < 0) throw new Error('no end-of-central-directory record');
      const count = dv.getUint16(end + 10, true);
      let p = dv.getUint32(end + 16, true);
      const out = [];
      const dec = new TextDecoder();
      for (let i = 0; i < count; i++) {
        if (dv.getUint32(p, true) !== 0x02014b50) throw new Error('bad central header');
        const nlen = dv.getUint16(p + 28, true);
        const size = dv.getUint32(p + 24, true);
        const local = dv.getUint32(p + 42, true);
        const name = dec.decode(bytes.subarray(p + 46, p + 46 + nlen));
        const lnlen = dv.getUint16(local + 26, true);
        const body = bytes.subarray(local + 30 + lnlen, local + 30 + lnlen + size);
        // The .viproj is a few KB of text; read it whole rather than a
        // prefix, or a field near the end reads as missing.
        const head = name.endsWith('.viproj') ? dec.decode(body) : '';
        out.push({ name, size, head });
        p += 46 + nlen;
      }
      return JSON.stringify(out.map((e) => ({ name: e.name, size: e.size,
        viproj: e.name.endsWith('.viproj') ? e.head : undefined })));
    })()`);
    const entries = JSON.parse(listing);
    const proj = entries.find((e) => e.name.endsWith('.viproj'));
    const movs = entries.filter((e) => e.name.endsWith('.mov'));
    if (proj && proj.name === 'project/project.viproj') {
      ok('the archive holds project/project.viproj at the root');
    } else {
      bad(`no .viproj at the expected path: ${entries.map((e) => e.name).join(', ')}`);
    }
    if (movs.length === 3 && movs.every((m) => m.name.startsWith('project/clips/'))) {
      ok(`three baked clips under clips/ (${movs.map((m) => Math.round(m.size / 1024) + 'K').join(', ')})`);
    } else {
      bad(`expected three clips under clips/, got ${movs.map((m) => m.name).join(', ')}`);
    }
    if (movs.length === 3 && movs.every((m) => m.size > 1024)) {
      ok('every clip has real bytes in it, not an empty container');
    } else {
      bad('a baked clip is suspiciously small');
    }
    // Distinct names, or two spans would have clobbered each other in the
    // archive and the project would reference one file twice. This is what the
    // index prefix in `clip_file_name` is for.
    if (new Set(movs.map((m) => m.name)).size === movs.length) {
      ok('the clips have distinct file names');
    } else {
      bad(`clip names collide: ${movs.map((m) => m.name).join(', ')}`);
    }
    if (proj && (proj.viproj.match(/original_path/g) ?? []).length === 3) {
      ok('the .viproj describes all three clips with provenance');
    } else {
      bad('the .viproj does not describe three clips');
    }
    if (proj && /probe\.webm/.test(proj.viproj)) {
      ok('the .viproj carries the provenance a retrim needs');
    } else {
      bad('the .viproj has no usable provenance');
    }

    // --- 5b. the offsets render: no bake, one .viproj ---------------------
    //
    // The fast round trip: no clips rendered at all, every span a trimmed cue
    // over the source the player has already ingested. What makes it land is
    // that both ends spell the clip name the same way — /play's ingest renames
    // a baked file to <stem>.mov, and this names it that.

    await page.eval('window.__chop.set_render(2)', { awaitPromise: false });
    await page.eval('window.__chop.start_export()', { awaitPromise: false });
    const offsets = await until('the offsets export', async () => {
      const n = await page.eval('window.__chop.lastName()');
      return n && n.endsWith('.viproj') ? n : null;
    }, { tries: 100 }).catch(async () => {
      const st = await state(page);
      bad(`offsets export never landed; status: ${JSON.stringify(st.status)}`);
      return null;
    });
    if (offsets) ok(`the offsets render produced ${offsets} with no bake`);

    const offsetsRon = await page.eval(
      'new TextDecoder().decode(window.__chop.lastZip())',
    );
    // `(?<!original_)` because provenance carries an `original_path` too, and
    // matching it would report two clips where the whole claim is that there
    // is one.
    const clipPaths = [...offsetsRon.matchAll(/(?<!original_)path:\s*"([^"]+)"/g)].map((m) => m[1]);
    if (clipPaths.length === 1 && clipPaths[0] === 'probe.mov') {
      ok('one clip, named the way /play will have interned it (probe.mov)');
    } else {
      bad(`expected one clip named probe.mov, got ${JSON.stringify(clipPaths)}`);
    }
    // Scoped to the cue banks: the clip's provenance has in_sec/out_sec of its
    // own, and counting those would pass with no cues at all.
    const cueSection = offsetsRon.slice(offsetsRon.indexOf('cue_banks:['));
    const cues = (cueSection.match(/clip:0/g) ?? []).length;
    const outs = [...cueSection.matchAll(/out_sec:\s*([0-9.]+)/g)].map((m) => Number(m[1]));
    if (cues === 3) ok('three cues, every one pointing at the single clip');
    else bad(`expected three cues over clip 0, found ${cues}`);
    if (outs.length === 3 && new Set(outs).size === 3) {
      ok(`each cue carries its own trim (out_sec ${outs.map((o) => o.toFixed(2)).join(', ')})`);
    } else {
      bad(`expected three distinct trims, got ${JSON.stringify(outs)}`);
    }
    // Instant is the point: an offsets project is text, not a bake.
    const bytes = await page.eval('window.__chop.lastZip().length');
    if (bytes < 8000) ok(`the whole project is ${bytes} bytes`);
    else bad(`an offsets project should be tiny; got ${bytes} bytes`);

    // --- 5c. the handoff: an export that never leaves the browser ---------
    //
    // /chop and /play are two pages on one origin, so they share one OPFS root.
    // "send to /play" writes a directory there instead of downloading, and the
    // other page claims it on boot.
    //
    // Only this half is checked here: /play needs WebGPU and a popup window,
    // and this browser is launched with neither (egui paints /chop through
    // WebGL2). The other half is checked in play-smoke against the same shape —
    // a `.viproj` plus one file per clip, flat, no directories.

    await page.eval('window.__chop.set_destination(1)', { awaitPromise: false });
    await page.eval('window.__chop.start_export()', { awaitPromise: false });
    const handoff = await until('the handoff', async () => {
      const files = await page.eval('window.__chop.handoffContents()');
      return files.length ? files : null;
    }, { tries: 100 }).catch(async () => {
      const st = await state(page);
      bad(`the handoff was never written; status: ${JSON.stringify(st.status)}`);
      return [];
    });
    // Offsets is still the render, so this is one file and no clips.
    if (handoff.length === 1 && handoff[0].name.endsWith('.viproj') && handoff[0].size > 0) {
      ok(`/chop handed off ${handoff[0].name} (${handoff[0].size} bytes) with no download`);
    } else {
      bad(`expected one .viproj in the handoff, got ${JSON.stringify(handoff)}`);
    }
    // The status line is the only thing that tells a visitor it went somewhere
    // other than their downloads folder.
    const sent = (await state(page)).status;
    if (typeof sent === 'string' && /sent .* to \/play/.test(sent)) {
      ok(`the shell said where it went: "${sent}"`);
    } else {
      bad(`the handoff did not report itself: ${JSON.stringify(sent)}`);
    }

    // Back to a download of rendered clips, so the storage checks below see a
    // normal state.
    await page.eval('window.__chop.set_destination(0)', { awaitPromise: false });
    await page.eval('window.__chop.set_render(0)', { awaitPromise: false });

    // --- 6. a project reopens without its source present ------------------
    //
    // The browser-only ordering: prep opens the video and *then* adopts the
    // project, because it can. Here the spans have to land first and the video
    // is a request, so this is the path with no native equivalent to lean on.

    const viproj = `(
      version: 3,
      clips: [
        (id: 1, path: "one.mov", name: "one", source: (original_path: "elsewhere.mov", in_frame: 10, out_frame: 40, in_sec: 0.0, out_sec: 0.0)),
        (id: 2, path: "two.mov", name: "two", source: (original_path: "elsewhere.mov", in_frame: 90, out_frame: 120, in_sec: 0.0, out_sec: 0.0)),
      ],
      clip_banks: [(name: "cuts", clip_ids: [1, 2])],
      cue_banks: [],
      defaults: (bpm: 128.0, quantum: 4.0, phrase_len: 16),
    )`;
    await page.eval(`window.__chop.load_project('retrim.viproj', ${JSON.stringify(viproj)})`,
                    { awaitPromise: false });
    // The span marked for the export is still in the list, and must stay: a
    // reopen replaces only the spans belonging to the project's own source.
    const reopened = await until('the reopened project', async () => {
      const s = await state(page);
      return s.spans.filter((sp) => sp.source === 'elsewhere.mov').length === 2 ? s : null;
    }, { tries: 60 });
    if (reopened) {
      ok('a .viproj reopened into 2 spans with its source video absent');
      const kept = reopened.spans.filter((sp) => sp.source === 'probe.webm').length;
      if (kept === 3) ok("the open video's own spans survived the reopen");
      else bad(`${kept} spans left from the open video, expected 3`);
      if (reopened.banks === 1) {
        ok('bank names came back off the project');
      } else {
        bad(`banks=${reopened.banks}`);
      }
      if (/open elsewhere\.mov/.test(reopened.status)) {
        ok('the status line asks for the source video by name');
      } else {
        bad(`status does not name the missing source: ${JSON.stringify(reopened.status)}`);
      }
    }


    // And with those spans in the list, an export must refuse rather than bake
    // frames from the wrong video. There is one <video> element; prep reopens
    // each span's own source by path and has no such limit.
    await page.eval('window.__chop.start_export()', { awaitPromise: false });
    const refused = await until('the export refusal', async () => {
      const st = await state(page);
      return st.error && /another video/.test(st.status) ? st : null;
    }, { tries: 40 }).catch(() => null);
    if (refused) ok('exporting spans from a video that is not open is refused by name');
    else bad('a foreign-source export was not refused');

    // The file chooser has to open from inside the gesture that asked for it,
    // and the gesture arrives through egui in a rAF callback — the one thing
    // about this bridge that a unit test cannot reach. /play verified the same
    // property for PickIsf; this verifies it for a page that owns the canvas.
    let chooserOpened = false;
    page.on('Page.fileChooserOpened', () => { chooserOpened = true; });
    await page.send('Page.setInterceptFileChooserDialog', { enabled: true });
    await page.eval(
      `requestAnimationFrame(() => window.dispatchEvent(
         new CustomEvent('vidiotic-chop-pick', { detail: 'video' })))`,
      { userGesture: true, awaitPromise: false },
    );
    await until('the file chooser', async () => chooserOpened, { tries: 40 }).catch(() => {});
    if (chooserOpened) ok('a Pick* command opens a real file chooser from a rAF callback');
    else bad('the file chooser never opened — the transient-activation window was missed');
    await page.send('Page.setInterceptFileChooserDialog', { enabled: false });

    // --- 8. the session survives a reload ---------------------------------
    //
    // The claim OPFS exists for. Everything above proves the editor works in a
    // tab; this proves closing the tab is not the same as losing the evening.
    // Nothing is stubbed: the video comes back out of OPFS as bytes and is
    // re-decoded, and the spans come back out of the same `.vprep` RON the
    // desktop app writes beside a file.

    const before = await state(page);
    const kept = before.spans.filter((sp) => sp.source === 'probe.webm').length;
    // The autosave is throttled to ~1 Hz, so give it a beat to have run.
    await sleep(1600);
    const stored = await page.eval(
      '(async () => !!(await navigator.storage.getDirectory()))()',
    );
    if (stored) ok('OPFS is available to the page');
    else bad('OPFS is not available — nothing below can pass');

    await page.send('Page.reload');
    await until('the reboot', () => page.eval('!!window.__chop'), { tries: 200 });
    const restored = await until('the restored session', async () => {
      const st = await state(page);
      return st && st.frames > 1 ? st : null;
    }, { tries: 200 });

    if (restored.source === 'probe.webm') {
      ok('the video came back out of OPFS and re-decoded');
    } else {
      bad(`source after reload is ${JSON.stringify(restored.source)}`);
    }
    if (restored.spans.length === kept) {
      ok(`${kept} span(s) came back out of the stored .vprep`);
    } else {
      bad(`${restored.spans.length} spans after reload, expected ${kept}`);
    }
    if (restored.frames >= 55 && restored.frames <= 65) {
      ok('the restored video has the same frame count as the original');
    } else {
      bad(`restored frame count is ${restored.frames}`);
    }
    // Spans from the *other* video are deliberately not stored: the sidecar is
    // scoped to one source, exactly as prep's is, so a reload cannot resurrect
    // marks whose frame numbers belong to a file that is not open.
    if (!restored.spans.some((sp) => sp.source === 'elsewhere.mov')) {
      ok('spans from an unopened video were not stored');
    } else {
      bad('a foreign-source span came back from storage');
    }

    // And forgetting clears it: a stored session the visitor cannot get rid of
    // is worse than one that was never kept.
    await page.eval('window.__chop.forgetStored()');
    await page.send('Page.reload');
    await until('the second reboot', () => page.eval('!!window.__chop'), { tries: 200 });
    await sleep(1200);
    const cleared = await state(page);
    if (cleared && cleared.frames === 0 && cleared.spans.length === 0) {
      ok('"forget it" cleared the store — the next visit starts empty');
    } else {
      bad(`storage survived a forget: ${JSON.stringify(cleared)}`);
    }

    // --- console hygiene --------------------------------------------------
    //
    // A page that works and logs an exception every frame is a page that is
    // about to stop working.
    const errs = await page.eval('JSON.stringify(window.__chopErrors ?? [])');
    const parsed = JSON.parse(errs);
    if (parsed.length === 0) ok('no uncaught page errors');
    else bad(`page errors: ${parsed.join(' | ')}`);
  } catch (e) {
    bad(String(e.message ?? e));
  } finally {
    if (page && !KEEP) page.close();
    if (!KEEP) {
      chrome.kill();
      server?.kill();
      try { rmSync(profile, { recursive: true, force: true }); } catch { /* best effort */ }
    } else {
      console.log(`\n--keep: chrome is still up on ${PAGE} (profile ${profile})`);
    }
  }

  console.log(failures === 0 ? '\nSMOKE PASS' : `\nSMOKE FAIL (${failures})`);
  process.exit(failures === 0 ? 0 : 1);
}

await main();
