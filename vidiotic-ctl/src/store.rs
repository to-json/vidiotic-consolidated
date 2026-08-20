//! Persistence for the global control map: `MapFile` is the versioned
//! on-disk envelope, versioned independently of `.viproj`'s `FORMAT_VERSION`
//! since the global map and a project's embedded map evolve separately.

use std::path::{Path, PathBuf};

use nanoserde::{DeRon, SerRon};

use crate::model::ControlMap;

/// Bumped on any breaking change to the on-disk map shape.
///
/// v2: `Action::Prep` verbs (`vidiotic-prep`'s bindable vocabulary). v1 files
/// load unchanged — the player's variants kept their serialized names — but a
/// v2 map that actually uses a Prep verb fails in a v1 binary at the unknown
/// variant, which is what the bump is for.
pub const MAP_VERSION: u32 = 2;

#[derive(SerRon, DeRon, Clone, Debug, Default)]
pub struct MapFile {
    #[nserde(default)]
    pub version: u32,
    #[nserde(default)]
    pub map: ControlMap,
}

/// The per-user config directory, matching what `dirs::config_dir()` returned.
///
/// Inlined rather than taken as a dependency: `dirs`, `dirs-sys`, and
/// `option-ext` are all archived upstream (the first two since 2025-01, the
/// third since 2023-05), so nothing in that chain will ever be patched again —
/// a poor trade for one function call.
///
/// Platform rules are `dirs`' own, kept verbatim so existing install paths
/// keep resolving:
///
/// - **macOS** — `$HOME/Library/Application Support`
/// - **Linux** — `$XDG_CONFIG_HOME` when set to an absolute path, else
///   `$HOME/.config`. The absoluteness check is part of the XDG spec: a
///   relative value must be ignored, not joined.
/// - **Windows** — `%APPDATA%` (the roaming profile, `FOLDERID_RoamingAppData`)
/// - **wasm / anything else** — `None`, and the caller falls back to `.`
fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join("Library/Application Support"))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                std::env::var_os("HOME")
                    .filter(|h| !h.is_empty())
                    .map(|h| PathBuf::from(h).join(".config"))
            })
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .filter(|a| !a.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

fn config_map_path(name: &str) -> PathBuf {
    config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("vidiotic")
        .join(name)
}

/// A missing file (first run) is the normal case and loads silently; a
/// present-but-unparseable one warns and falls back to the same default.
fn load_or_default(path: &Path) -> ControlMap {
    if !path.exists() {
        return ControlMap::default();
    }
    load_map(path).unwrap_or_else(|err| {
        log::warn!("failed to load control map at {}: {err}", path.display());
        ControlMap::default()
    })
}

/// `<config dir>/vidiotic/global.vmap` — e.g.
/// `~/Library/Application Support/vidiotic/global.vmap` on macOS. The
/// *player's* map.
#[must_use]
pub fn global_map_path() -> PathBuf {
    config_map_path("global.vmap")
}

/// Load the player's global map.
#[must_use]
pub fn load_global() -> ControlMap {
    load_or_default(&global_map_path())
}

/// # Errors
/// If serialization or the write fails.
pub fn save_global(map: &ControlMap) -> anyhow::Result<()> {
    save_map(&global_map_path(), map)
}

/// `<config dir>/vidiotic/prep.vmap` — `vidiotic-prep`'s editor bindings.
///
/// Deliberately not `global.vmap` and not the `.viproj`/`.vprep` embed: those
/// two hold the *player's* map, and sharing a file would let a player binding
/// mask a prep default via [`crate::Mapper`]'s any-match-wins rule and then
/// resolve to a verb prep can't run — silently killing the key. Editor
/// keybindings are also a user preference, not a project property: they should
/// not travel with a `.viproj` to another machine.
#[must_use]
pub fn prep_map_path() -> PathBuf {
    config_map_path("prep.vmap")
}

/// Load `vidiotic-prep`'s editor bindings.
#[must_use]
pub fn load_prep() -> ControlMap {
    load_or_default(&prep_map_path())
}

/// # Errors
/// If serialization or the write fails.
pub fn save_prep(map: &ControlMap) -> anyhow::Result<()> {
    save_map(&prep_map_path(), map)
}

/// # Errors
/// If the file cannot be read, if it does not parse, or if the map was written
/// by a newer version. Callers that want a silent default for a missing file
/// should check [`Path::exists`] first (see [`load_global`]).
pub fn load_map(path: &Path) -> anyhow::Result<ControlMap> {
    let text = std::fs::read_to_string(path)?;
    let file =
        MapFile::deserialize_ron(&text).map_err(|err| anyhow::anyhow!("parse {path:?}: {err}"))?;
    anyhow::ensure!(
        file.version <= MAP_VERSION,
        "{} is map v{} but this build reads up to v{MAP_VERSION} — update vidiotic",
        path.display(),
        file.version
    );
    let mut map = file.map;
    map.canonicalize_keys();
    Ok(map)
}

/// # Errors
/// If the directory cannot be created, or the write fails.
pub fn save_map(path: &Path, map: &ControlMap) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = MapFile {
        version: MAP_VERSION,
        map: map.clone(),
    };
    std::fs::write(path, file.serialize_ron())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, Binding, ControlSource};

    fn sample_map() -> ControlMap {
        ControlMap {
            bindings: vec![Binding {
                source: ControlSource::Key {
                    key: "t".into(),
                    ctrl: false,
                    alt: false,
                    shift: false,
                    cmd: false,
                },
                action: Action::TapDownbeat,
            }],
        }
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vidiotic_ctl_store_test_{name}_{}.vmap",
            std::process::id()
        ))
    }

    #[test]
    fn round_trips_through_a_temp_path() {
        let path = temp_path("round_trip");
        let map = sample_map();
        save_map(&path, &map).expect("save");
        let back = load_map(&path).expect("load");
        assert_eq!(map.bindings, back.bindings);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_errors_from_load_map() {
        let path = temp_path("missing");
        let _ = std::fs::remove_file(&path);
        assert!(load_map(&path).is_err());
    }

    #[test]
    fn global_map_path_ends_with_vidiotic_global_vmap() {
        let path = global_map_path();
        assert_eq!(path.file_name().unwrap(), "global.vmap");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "vidiotic");
    }

    #[test]
    fn prep_map_path_ends_with_vidiotic_prep_vmap() {
        let path = prep_map_path();
        assert_eq!(path.file_name().unwrap(), "prep.vmap");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), "vidiotic");
    }

    /// The two maps must never collide: prep's editor keys and the player's
    /// performance bindings are separate vocabularies with separate lifetimes.
    #[test]
    fn prep_and_global_maps_are_different_files() {
        assert_ne!(prep_map_path(), global_map_path());
    }

    /// A v1 map — written before prep had any bindings — must still load.
    #[test]
    fn v1_map_file_loads_unchanged() {
        let path = temp_path("v1_compat");
        std::fs::write(
            &path,
            r#"(version:1, map:(bindings:[
                (source:Key(key:"t", ctrl:false, alt:false, shift:false, cmd:false),
                 action:TapDownbeat),
            ]))"#,
        )
        .expect("write");
        let map = load_map(&path).expect("v1 map must load");
        assert_eq!(map.bindings.len(), 1);
        assert_eq!(map.bindings[0].action, Action::TapDownbeat);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn newer_map_version_refuses() {
        let path = temp_path("future");
        std::fs::write(
            &path,
            format!("(version:{}, map:(bindings:[]))", MAP_VERSION + 1),
        )
        .expect("write");
        let err = load_map(&path).expect_err("future map version must refuse");
        assert!(err.to_string().contains("map v"), "unexpected error: {err}");
        let _ = std::fs::remove_file(&path);
    }

    /// A hand-edited map can spell `[` as `OpenBracket` and `1` as `Num1` —
    /// neither of which a live key event produces, so the binding would
    /// silently never fire. Loading normalizes them to the canonical literal.
    #[test]
    fn load_canonicalizes_hand_edited_key_spellings() {
        let path = temp_path("hand_edited_key_names");
        std::fs::write(
            &path,
            r#"(version:2, map:(bindings:[
                (source:Key(key:"OpenBracket", ctrl:false, alt:false, shift:false, cmd:false),
                 action:TapDownbeat),
                (source:Key(key:"Num1", ctrl:false, alt:false, shift:false, cmd:false),
                 action:TapTempo),
                (source:Key(key:"t", ctrl:false, alt:false, shift:false, cmd:false),
                 action:SoftReset),
            ]))"#,
        )
        .expect("write");
        let map = load_map(&path).expect("v2 map must load");
        let keys: Vec<&str> = map
            .bindings
            .iter()
            .map(|b| match &b.source {
                ControlSource::Key { key, .. } => key.as_str(),
                other => panic!("expected a key binding, got {other:?}"),
            })
            .collect();
        assert_eq!(keys, ["[", "1", "t"]);
        let _ = std::fs::remove_file(&path);
    }

    /// A map that binds only letters and named keys must survive a load
    /// byte-identical — the migration is a respelling, not a rewrite.
    #[test]
    fn loading_leaves_already_canonical_keys_alone() {
        let path = temp_path("canonical_noop");
        save_map(&path, &sample_map()).expect("save");
        let map = load_map(&path).expect("load");
        assert_eq!(map.bindings, sample_map().bindings);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_stamps_the_current_version() {
        let path = temp_path("stamp");
        save_map(&path, &sample_map()).expect("save");
        let text = std::fs::read_to_string(&path).expect("read");
        let file = MapFile::deserialize_ron(&text).expect("parse");
        assert_eq!(file.version, MAP_VERSION);
        let _ = std::fs::remove_file(&path);
    }
}
