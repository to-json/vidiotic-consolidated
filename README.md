# vidiotic

A suite for slicing and playing video clips on tempo with live or DJ'd music.

Clips are cut and trimmed once, baked to HAP so playback is a texture upload
rather than a decode, and then fired from a cue grid against a beat clock, under
audio-reactive live-reloaded shaders. There is a native app and a browser one,
and they are the same engine.

## The pieces

| crate | what it is |
|---|---|
| `vidiotic` | the native VJ app: windows, cameras, ffmpeg, the IPC server |
| `vidiotic-play` | the portable player — render core, engine, panels. Native `vidiotic` links it; the browser `/play` *is* it |
| `vidiotic-chop` | the portable span editor: mark, trim, bake, export a project |
| `vidiotic-prep` | the native shell around `vidiotic-chop` |
| `vidiotic-ctl` | control mapping (MIDI, keyboard, gamepad) and the binding editor |
| `vidiotic-core` | the session model both shells share: `.viproj`, ISF, the clock |
| `vidiotic-bake` | HAP: the frame compressor, the container reader, the muxer |
| `vidiotic-wire` | the scriptable IPC protocol — anything the UI can do, a script can do |
| `phosphor` | the character-grid egui idiom everything is drawn in |

The split is load-bearing rather than tidy: `vidiotic-play` and `vidiotic-chop`
carry no filesystem, no ffmpeg and no sockets, which is what lets them cross to
`wasm32-unknown-unknown` unchanged. `scripts/wasm-gate.sh` is the ratchet that
keeps it true.

## Getting started

```sh
just --list          # every task in the repo
just check           # fmt, clippy, tests, and the wasm ratchet
cargo run -p vidiotic -- --help
```

`just check` is what CI runs, minus the parts a hosted runner cannot do
(see `.github/workflows/ci.yml`).

### Prerequisites

- **Rust** (stable) — `rustup target add wasm32-unknown-unknown` for the web builds.
- **ffmpeg 8** with headers, for every native build: `brew install ffmpeg pkg-config`.
  `ffmpeg-next` binds the libav\* headers directly, so a distro shipping ffmpeg 6
  or 7 will not build it.
- **wasm-bindgen-cli**, at exactly the version in `Cargo.lock` — a mismatch fails
  at page load, not at build time. `scripts/wasm-gate.sh` prints the install line.
- **binaryen** (`wasm-opt`) — optional; the web bundle ships about a third larger
  without it.
- **Chrome**, for the browser smoke suites. Set `$CHROME` if it is not in the
  usual place.
- **clips/** — your own video. It is gitignored, is not derivable from this
  repository, and the smoke suites plus a few `vidiotic-bake` conformance tests
  read from it. Without it those skip or fail loudly rather than passing
  vacuously; everything else runs.
- **docker**, only for `scripts/serve-play.sh`, which rehearses a deploy behind
  the same nginx the server runs.

### The web builds

```sh
just build           # both wasm bundles into web/ (--debug for the fast one)
just demo            # serve a release at http://localhost:8080
just release         # assemble dist/web for deployment
just gate            # the portability ratchet, and the wasm test run
just smoke           # drive both pages in a real Chrome
```

### Native packaging

```sh
just app             # dist/Vidiotic.app
just dmg             # and a disk image
just install-app     # into /Applications
```

## Documentation

`docs/web-port.md` is the long one: it is the record of taking the native app to
the browser, and most of the architectural "why" lives there. `docs/ui-flows/`
walks the interfaces as somebody actually operates them. The IPC protocol is
documented where it is defined — `vidiotic-wire`'s module docs, and
`vidiotic/src/ipc.rs` for the server side.

## Working in this repo

Comments here explain *why*, usually with the measurement that settled it, and
tests are behavioral rather than structural. Both are conventions worth keeping.

See `LICENSE`.
