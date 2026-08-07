# Scriptable IPC

`vidiotic` exposes a Unix-domain socket that speaks newline-delimited JSON.
Anything the control UI can do, a script can do — set tempo, edit cues, switch
banks, load projects — and it can read engine state back. The wire vocabulary
lives in the `vidiotic-wire` crate; this document is the protocol reference.

## Connecting

The server is **on by default**. On startup it binds:

```
$TMPDIR/vidiotic-<pid>.sock          # the instance's own socket
$TMPDIR/vidiotic-latest.sock         # symlink to the newest instance
```

- `--ipc-path <PATH>` binds an explicit path instead (and skips the symlink).
- `--no-ipc` disables the server entirely.

The socket lives under `$TMPDIR`, which on macOS is a per-user directory
(`/var/folders/.../T`, mode `0700`) — only your user's processes can reach it.
Commands that take a path (`LoadProject`, `SaveProjectTo`, `SetShaderPath`) read
and write arbitrary files as you: that's by design, same trust boundary as
running the app.

Connect with anything that speaks Unix sockets:

```sh
nc -U "$TMPDIR/vidiotic-latest.sock"
```

The server's **first line** is a greeting identifying the endpoint and handing
you the protocol version and current session epoch:

```json
{"vidiotic":{"wire":1,"epoch":0}}
```

## Framing

One JSON object per line, both directions. Every request gets **exactly one
reply** — a reply is your barrier: once it arrives, the command has been
applied (or rejected) and any later query reflects it.

### Request

```json
{"id":1,"epoch":3,"req":{"cmd":{"SetBpm":[128.0]}}}
{"id":2,"req":{"get":"Status"}}
```

- `id` — a number you choose; the reply echoes it so a pipelining client can
  match answers. One request at a time doesn't need to vary it.
- `epoch` — optional. If present, the server rejects the request unless it
  matches the current session generation (see [Epoch](#epoch)). Omit it if you
  don't care.
- `req` — either `{"cmd": <command>}` or `{"get": <query>}`.

Command payloads follow the enum's natural JSON: unit commands are bare strings
(`{"cmd":"TapTempo"}`), single/multi-argument commands key an array
(`{"SetCueIn":[3,1.5]}`), struct-form commands key an object
(`{"RelinkCamera":{"from":"uid:a","to":"uid:b"}}`). See the `WireCommand`
variants in `vidiotic-wire` for the full list.

### Reply

```json
{"id":2,"epoch":3,"ok":{"Status":{"project_path":"/a.viproj","epoch":3,"wire_version":1,"advanced":false,"grammar_on":false}}}
{"id":1,"epoch":3,"ack":true}
{"id":9,"epoch":3,"err":"unknown cue 7"}
```

Exactly one of three keys:

- `ok` — a **query** succeeded; the value is the answer.
- `ack` — a **command** was accepted and dispatched.
- `err` — the request was rejected before dispatch; the value is a message.

`epoch` always carries the current session generation.

## `ack` means dispatched, not "it worked"

The engine applies most commands infallibly — an out-of-range value clamps, an
unknown clip id no-ops. `ack` tells you the command was accepted and run, not
that it had the effect you expected. When you need to be sure, **query for it**.
The server does reject the mistakes it can catch cheaply, with `err`:

- unknown cue id (`RemoveCue`, `SetCueIn`, `SetCueChain`, `SetCueParam`, …),
- out-of-range bank / clip-bank index, or chain slot,
- `SaveProject` / `OpenProjectEditor` with no project loaded (use
  `SaveProjectTo` with an explicit path — the wire never opens a file dialog),
- a stale `epoch`.

Clip ids are **not** validated (the server only sees the active clip bank), so a
truly unknown clip id in `AddCue` / `ToggleClipActive` silently no-ops.

Rejection is checked against the *last published* state, so a command that
depends on a structural change made earlier in the same batch can be wrongly
rejected — e.g. `AddBank` immediately followed by `SetEditBank` on the new
index. It's safe (the engine clamps regardless) and self-corrects on the next
tick; if you're chaining dependent structural edits, read a reply between them.

## Read-your-writes

Requests from one connection are processed in order within a single engine
tick: a `cmd` applies, then a following `get` is answered from the mirror
rebuilt *after* the command. So this reads back the tempo you just set:

```json
{"id":1,"req":{"cmd":{"SetBpm":[140.0]}}}
{"id":2,"req":{"get":"Transport"}}
```

Caveat: only **synchronous** state is immediate. Loading a project or a clip
directory spawns decoder/thumbnail work that completes over later ticks, so a
`Pool` query right after `LoadProject` may show clips still warming up.

## Epoch

Loading a project (`LoadProject`) or replacing the pool (`SetClipDir`) turns
over the whole clip/cue id space and bumps the session **epoch**. Ids you
captured before the bump may now point at different content. Two ways to stay
safe:

- read `epoch` from every reply (and the greeting); if it changed, re-query for
  fresh ids before acting;
- stamp requests with `"epoch": N` — the server rejects them with `err` if the
  session has moved on, so a queued edit can't land on the wrong cue.

## Queries

| `get` | answer | contents |
|-------|--------|----------|
| `Status` | `Status` | project path, epoch, wire version, mode flags |
| `Transport` | `Transport` | bpm, beat/phase/quantum, time sig, cadences, sync, peers |
| `Pool` | `Pool` | clip banks, clips (with duration/fps), selection, cameras |
| `Cues` | `Cues` | cue banks, live/edit bank, cue views, selection |
| `Shaders` | `Shaders` | shader pool with ISF input schemas |
| `Audio` | `Audio` | input devices, current selection, last error |
| `Levels` | `Levels` | 21 log bands + 512-bin spectrum + overall level |

Queries are selective — the 512-bin spectrum is serialized only for `Levels`.

## Sharp edges

- **Cursor-relative commands.** `LoadIsf`, `NudgeCueParam`, and the
  `SetCue{In,Out}ToPlayhead` pair act on the *current selection*. To use them a
  script must `SelectCue` first, which moves the operator's edit selection too.
  The playhead-snap pair also silently no-ops unless the target cue is playing.
- **Heavy commands hitch the output.** `LoadProject`, `SetClipDir`, and shader
  compiles run synchronously on the render thread; a scripted setlist that
  triggers them mid-performance will drop frames on the output window.
- **Camera commands can prompt.** `SetCameraOnAir` starts capture, which on
  macOS raises a TCC permission dialog the first time — and a `tmux`-launched
  session auto-denies it.

## From Rust

`vidiotic-wire`'s `client` feature gives a blocking helper:

```rust
use vidiotic_wire::{WireClient, WireCommand, WireQuery, WireReply};

let mut c = WireClient::connect(format!("{}/vidiotic-latest.sock", std::env::temp_dir().display()))?;
c.command(WireCommand::SetBpm(128.0))?;          // waits for the ack
if let WireReply::Transport(t) = c.query(WireQuery::Transport)? {
    println!("bpm is now {}", t.bpm);
}
```

See `examples/` for `nc`/`jq` scripts that need no client at all.
