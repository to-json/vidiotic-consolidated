//! The shipped shader library, and where it lives once the app is a bundle.
//!
//! Livecoding needs a directory the user can edit. Inside a `.app` that cannot
//! be `Contents/Resources` — writing there invalidates the code signature, and
//! a signed app whose seal is broken stops launching. So a bundled run seeds
//! `~/Library/Application Support/Vidiotic/shaders` from Resources on first
//! launch and works against that copy; a cargo run just uses the repo's
//! `shaders/` in place, where the developer is already editing it.

use std::path::PathBuf;

/// The `~/Library/…` family directory name, shared with the sibling tools.
pub const FAMILY: &str = "Vidiotic";

/// The shader that a bare launch (no `--shader`, no project) opens with.
const DEFAULT_SHADER: &str = "demo.frag";

/// The repo's shader directory, baked in at compile time. Only meaningful for
/// a cargo run from this checkout — a bundle uses `Contents/Resources/shaders`.
///
/// It lives under `vidiotic-play` now, alongside the `include_str!`s that bake
/// the built-in effects in, so the constant comes from that crate rather than
/// from this one's `CARGO_MANIFEST_DIR`. Getting this wrong is silent: a
/// non-existent directory just makes `default_shader` return `None`, and the
/// app boots to a black screen that reads like a renderer bug.
fn repo_shaders() -> PathBuf {
    PathBuf::from(vidiotic_play::REPO_SHADERS)
}

/// The user-writable shader library: seeded from the bundle's Resources on
/// first bundled launch, or the repo's own `shaders/` when running from cargo.
///
/// Seeding never overwrites: a shader the user has edited stays theirs, while
/// newly shipped ones appear on the next launch after an update.
#[must_use]
pub fn shader_library() -> PathBuf {
    let Some(shipped) = phosphor::bundle::resources().map(|r| r.join("shaders")) else {
        return repo_shaders();
    };
    let user = phosphor::bundle::data_dir(FAMILY).join("shaders");
    if let Err(e) = phosphor::bundle::seed_dir(&shipped, &user) {
        log::warn!("could not seed shader library at {}: {e}", user.display());
        return shipped;
    }
    user
}

/// The shader to boot with when the invocation named none. `None` when the
/// library has no default shader — the renderer falls back to passthrough,
/// which is a black screen but a running app.
#[must_use]
pub fn default_shader() -> Option<PathBuf> {
    let path = shader_library().join(DEFAULT_SHADER);
    path.exists().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The repo shader directory is named by a compile-time constant in another
    /// crate, and every failure mode of that is silent: `default_shader` returns
    /// `None`, the renderer falls back to passthrough, and the app boots to a
    /// black screen that reads like a renderer bug rather than a missing path.
    /// Nothing else in the suite would notice, so this is the check.
    #[test]
    fn the_repo_shader_directory_is_where_we_think_it_is() {
        let dir = repo_shaders();
        assert!(
            dir.is_dir(),
            "{} is not a directory — vidiotic_play::REPO_SHADERS is stale",
            dir.display()
        );
        assert!(
            dir.join(DEFAULT_SHADER).is_file(),
            "{DEFAULT_SHADER} missing from {}; a bare launch would open on black",
            dir.display()
        );
    }
}
