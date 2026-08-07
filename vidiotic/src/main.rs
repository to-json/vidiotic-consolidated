use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Args, Parser, Subcommand, ValueEnum};

use vidiotic::analysis::{self, AudioCtl, AudioFrame};
use vidiotic::app::{self, Boot};
use vidiotic::assets;
use vidiotic::audio;
use vidiotic::bank::Bank;
use vidiotic::clippool::{self, Clip, ClipBank};
use vidiotic::ipc;
use vidiotic::commands::{Cadence, ClipId, Command, SyncKind, TimeSig, LOOP_TICKS_PER_BEAT};
use vidiotic::project;
use vidiotic::transcode;

#[derive(Parser)]
#[command(name = "vidiotic", version, about = "VJ controller: audio-reactive shader over video clips")]
struct Cli {
    /// Omitted ⇒ `run`. A double-clicked `.app` is launched with no arguments
    /// at all, so the player has to be what you get by default.
    #[command(subcommand)]
    cmd: Option<Cmd>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the VJ player.
    Run(RunArgs),
    /// Transcode a video to a HAP .mov for fast, near-zero-CPU playback.
    Transcode {
        /// Source video (any format ffmpeg can decode).
        input: PathBuf,
        /// Destination .mov (HAP1).
        output: PathBuf,
    },
}

#[derive(Args)]
struct RunArgs {
    /// What to open, inferred from what it is: a `.viproj` project, a
    /// directory of clips, or a single clip. The form Launch Services and
    /// `open` hand us, and the one a shell user reaches for first.
    #[arg(value_name = "PROJECT|CLIP|DIR")]
    path: Option<PathBuf>,

    /// A single clip to loop (added to the pool and activated immediately).
    #[arg(short, long)]
    clip: Option<PathBuf>,

    /// A directory of clips to populate the pool (toggle them active in the UI).
    #[arg(short = 'd', long)]
    clip_dir: Option<PathBuf>,

    /// A saved `.viproj` project: clips, clip banks, cue banks, and session
    /// defaults. Mutually exclusive with --clip/--clip-dir.
    #[arg(long)]
    project: Option<PathBuf>,

    /// When loading a --project whose clip files have moved, re-match missing
    /// clips by name under this directory.
    #[arg(long)]
    relink_root: Option<PathBuf>,

    /// Fragment shader: .frag/.fs/.glsl (GLSL) or .wgsl. Optional when a
    /// --project supplies one; required otherwise.
    #[arg(short, long)]
    shader: Option<PathBuf>,

    /// Initial BPM.
    #[arg(long, default_value_t = 120.0)]
    bpm: f64,

    /// Phrase length in beats for auto-transitions (16 or 32).
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..))]
    phrase_len: u32,

    /// Clock sync source at startup.
    #[arg(long, value_enum, default_value = "internal")]
    sync: SyncArg,

    /// Output monitor index for fullscreen (default: first non-primary).
    #[arg(long)]
    monitor: Option<usize>,

    /// Stay windowed instead of going fullscreen after the first frame.
    #[arg(long)]
    windowed: bool,

    /// Input device name substring to capture from (default: system default input).
    #[arg(long)]
    audio_device: Option<String>,

    /// Disable the scriptable IPC socket (on by default).
    #[arg(long)]
    no_ipc: bool,

    /// Override the IPC socket path. Default: `$TMPDIR/vidiotic-<pid>.sock`,
    /// with a `vidiotic-latest.sock` symlink pointing at it. An explicit path
    /// skips the symlink.
    #[arg(long)]
    ipc_path: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
enum SyncArg {
    /// App-owned host-time clock.
    Internal,
    /// Follow an Ableton Link session.
    Link,
}

fn main() -> anyhow::Result<()> {
    // Bundled, stderr goes nowhere; the log has to land in ~/Library/Logs.
    phosphor::bundle::init_logging(vidiotic::assets::FAMILY, "vidiotic", "info");
    // Quiets libswscale's "No accelerated colorspace conversion found" notice
    // (and similar av_log() chatter) — it's a perf-path note, not an error, and
    // it bypasses our own log filtering since it's FFmpeg's own logger.
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
    // `phosphor::bundle::args` drops the `-psn_…` serial Launch Services may
    // prepend, which clap would otherwise reject as an unknown flag.
    let cli = Cli::parse_from(phosphor::bundle::args());
    match cli.cmd {
        Some(Cmd::Transcode { input, output }) => transcode::run(&input, &output),
        Some(Cmd::Run(args)) => run_player(args),
        None => run_player(cli.run),
    }
}

/// The pool and session state assembled from either a `--project` file or the
/// `--clip`/`--clip-dir` flags, before audio/window plumbing is attached.
struct Loaded {
    clips: Vec<Clip>,
    clip_banks: Vec<ClipBank>,
    cue_banks: Vec<Bank>,
    auto_active: Vec<ClipId>,
    /// Per-clip probe metadata retained for a faithful save; empty for the
    /// `--clip`/`--clip-dir` path (raw files carry no baked metadata).
    clip_meta: HashMap<ClipId, project::ClipMeta>,
    /// The `.viproj` this was loaded from, if any (the default save target).
    project_path: Option<PathBuf>,
    bpm: f64,
    time_sig: TimeSig,
    phrase_cadence: Cadence,
    sync: SyncKind,
    preserve_playhead: bool,
    loop_cadence: Option<Cadence>,
    advanced: bool,
    shader: PathBuf,
    controls: vidiotic_ctl::ControlMap,
}

/// Build the pool from `--clip`/`--clip-dir`: a flat pool wrapped in one clip
/// bank, no cue banks (the engine starts with a default empty bank). With
/// neither flag this is the empty session a bare launch opens.
fn load_from_flags(cli: &RunArgs) -> anyhow::Result<Loaded> {
    let mut clips = Vec::new();
    if let Some(dir) = &cli.clip_dir {
        clips = clippool::scan(dir);
    }
    let mut auto_active: Vec<ClipId> = Vec::new();
    if let Some(single) = &cli.clip {
        let id = clips.len() as ClipId;
        let name: Arc<str> = single
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("clip")
            .into();
        clips.push(Clip {
            id,
            source: clippool::ClipSource::File(single.clone()),
            name,
            bpm: None,
        });
        auto_active.push(id);
    }
    // An empty pool is a legitimate session: it is what double-clicking the
    // app gives you, and the library panel is where you go from there.
    let shader = cli
        .shader
        .clone()
        .or_else(assets::default_shader)
        .unwrap_or_default();
    // One clip bank covering the whole flat pool, named for the source folder.
    let name: Arc<str> = cli
        .clip_dir
        .as_ref()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("clips")
        .into();
    // No clips ⇒ no bank, rather than an empty one named after nothing. The
    // library panel's "add folder" (`AddClipDirAsBank`) is the way in from here.
    let clip_banks = if clips.is_empty() {
        Vec::new()
    } else {
        vec![ClipBank {
            name,
            dir: cli.clip_dir.clone(),
            clip_ids: clips.iter().map(|c| c.id).collect(),
        }]
    };
    Ok(Loaded {
        clips,
        clip_banks,
        cue_banks: Vec::new(),
        auto_active,
        clip_meta: HashMap::new(),
        project_path: None,
        bpm: cli.bpm,
        time_sig: TimeSig::default(),
        phrase_cadence: Cadence::Note(cli.phrase_len.max(1) * LOOP_TICKS_PER_BEAT),
        sync: match cli.sync {
            SyncArg::Internal => SyncKind::Internal,
            SyncArg::Link => SyncKind::Link,
        },
        preserve_playhead: true,
        loop_cadence: None,
        advanced: false,
        shader,
        controls: vidiotic_ctl::ControlMap::default(),
    })
}

/// Load a `.viproj`: resolve clip paths (relinking missing ones under
/// `--relink-root` if given), then rebuild the flat pool, clip banks, and cue
/// banks with fresh ids.
fn load_from_project(cli: &RunArgs, path: &Path) -> anyhow::Result<Loaded> {
    let project = project::load(path)?;
    let project_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut resolved = project::resolve(project, project_dir);

    if !resolved.missing.is_empty() {
        if let Some(root) = &cli.relink_root {
            for cand in project::relink_by_root(&resolved, root) {
                if let Some(found) = cand.found {
                    project::apply_relink(&mut resolved, cand.clip_id, found);
                }
            }
        }
        if !resolved.missing.is_empty() {
            let names: Vec<String> = resolved
                .missing
                .iter()
                .filter_map(|id| resolved.project.clips.iter().find(|c| &c.id == id))
                .map(|c| c.name.clone())
                .collect();
            anyhow::bail!(
                "missing clip files (pass --relink-root <dir> to re-locate): {}",
                names.join(", ")
            );
        }
    }

    let assembled = project::assemble(&resolved);
    let d = &resolved.project.defaults;

    // Shader: prefer the project's, then --shader, then the shipped default.
    let shader = assembled
        .shader
        .or_else(|| cli.shader.clone())
        .or_else(assets::default_shader)
        .unwrap_or_default();

    Ok(Loaded {
        clips: assembled.clips,
        clip_banks: assembled.clip_banks,
        cue_banks: assembled.cue_banks,
        auto_active: Vec::new(),
        clip_meta: assembled.clip_meta,
        project_path: Some(path.to_path_buf()),
        bpm: if d.bpm > 0.0 { d.bpm } else { cli.bpm },
        time_sig: d.time_sig(),
        phrase_cadence: if d.phrase_cadence.is_some() || d.phrase_len > 0 {
            d.phrase_cadence()
        } else {
            Cadence::Note(cli.phrase_len.max(1) * LOOP_TICKS_PER_BEAT)
        },
        sync: match d.sync {
            project::SyncSpec::Internal => SyncKind::Internal,
            project::SyncSpec::Link => SyncKind::Link,
        },
        preserve_playhead: d.preserve_playhead,
        loop_cadence: d.loop_cadence(),
        advanced: d.advanced,
        shader,
        controls: resolved.project.controls.clone(),
    })
}

/// Fold a bare path argument into the flag it stands for, by looking at what
/// the path actually is: `.viproj` ⇒ `--project`, directory ⇒ `--clip-dir`,
/// anything else ⇒ `--clip`. An explicit flag always wins.
fn absorb_path_argument(cli: &mut RunArgs) {
    let Some(path) = cli.path.take() else { return };
    let is_proj = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("viproj"));
    let slot = if is_proj {
        &mut cli.project
    } else if path.is_dir() {
        &mut cli.clip_dir
    } else {
        &mut cli.clip
    };
    if slot.is_none() {
        *slot = Some(path);
    } else {
        log::warn!("ignoring {} — the matching flag was also given", path.display());
    }
}

fn run_player(mut cli: RunArgs) -> anyhow::Result<()> {
    absorb_path_argument(&mut cli);
    anyhow::ensure!(
        cli.project.is_none() || (cli.clip.is_none() && cli.clip_dir.is_none()),
        "--project is mutually exclusive with --clip/--clip-dir"
    );
    let loaded = match &cli.project {
        Some(path) => load_from_project(&cli, path)?,
        None => load_from_flags(&cli)?,
    };
    let thumb_rx = Some(clippool::spawn_thumbnailer(loaded.clips.clone()));

    // Audio analysis handoff.
    let (ctl_tx, ctl_rx) = crossbeam_channel::unbounded::<AudioCtl>();
    let (err_tx, err_rx) = crossbeam_channel::bounded::<cpal::Error>(8);
    let (audio_in, audio_out) = triple_buffer::triple_buffer(&AudioFrame::default());
    std::thread::Builder::new()
        .name("analysis".into())
        .spawn(move || analysis::run(ctl_rx, audio_in))?;

    let host = cpal::default_host();
    let audio_devices: Vec<Arc<str>> = audio::list_input_devices(&host)
        .into_iter()
        .map(|(_, name)| name.into())
        .collect();
    let audio_capture =
        audio::build_capture(&host, None, cli.audio_device.as_deref(), &ctl_tx, err_tx)?;
    log::info!("capturing audio from '{}'", audio_capture.device_name);

    let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<Command>();

    // Session generation, shared with the IPC server so its greeting reads the
    // live value as the engine bumps it on project reloads.
    let epoch = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    // Scriptable IPC. On by default at $TMPDIR/vidiotic-<pid>.sock (+ a
    // vidiotic-latest.sock symlink); --ipc-path overrides and skips the symlink;
    // --no-ipc disables it. A bind failure is non-fatal — the app still runs.
    let ipc = if cli.no_ipc {
        None
    } else {
        let (sock, latest) = if let Some(p) = &cli.ipc_path {
            (p.clone(), None)
        } else {
            let dir = std::env::temp_dir();
            let sock = dir.join(format!("vidiotic-{}.sock", std::process::id()));
            (sock, Some(dir.join("vidiotic-latest.sock")))
        };
        match ipc::IpcServer::spawn(sock, epoch.clone(), latest) {
            Ok(engine) => Some(engine),
            Err(e) => {
                log::warn!("ipc: disabled (bind failed: {e})");
                None
            }
        }
    };

    let boot = Boot {
        shader_path: loaded.shader,
        windowed: cli.windowed,
        monitor: cli.monitor,
        bpm: loaded.bpm,
        time_sig: loaded.time_sig,
        phrase_cadence: loaded.phrase_cadence,
        initial_sync: loaded.sync,
        clips: loaded.clips,
        clip_banks: loaded.clip_banks,
        cue_banks: loaded.cue_banks,
        auto_active: loaded.auto_active,
        clip_meta: loaded.clip_meta,
        project_path: loaded.project_path,
        epoch,
        ipc,
        preserve_playhead: loaded.preserve_playhead,
        loop_cadence: loaded.loop_cadence,
        advanced: loaded.advanced,
        thumb_rx,
        audio_out,
        audio_capture,
        audio_err_rx: err_rx,
        audio_ctl_tx: ctl_tx,
        host,
        audio_devices,
        cmd_tx,
        cmd_rx,
        controls: loaded.controls,
    };
    app::run(boot)
}
