//! Which Horizon release this build is (docs/versioning.md). Not Minecraft's version: that is
//! [`crate::version`], generated from the game's own files.

/// The release's version (`0.2.3`, no `v`): release.yml builds every release with `HORIZON_VERSION`
/// set from its tag, so there are no version-bump commits. None for a build from source.
pub const RELEASE: Option<&str> = match option_env!("HORIZON_VERSION") {
    Some(version) if !version.is_empty() => Some(version),
    _ => None,
};

/// [`RELEASE`], or this crate's Cargo.toml version for a build from source: what `--version`, user
/// agents and reports say.
pub const VERSION: &str = match RELEASE {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// A nightly's date and commit (`2026-10-28 0c02067`): nightly-build.yml builds the opt-in Nightly
/// channel with `HORIZON_NIGHTLY` set (docs/release-flow.md). None for a release or a build from source.
pub const NIGHTLY: Option<&str> = match option_env!("HORIZON_NIGHTLY") {
    Some(nightly) if !nightly.is_empty() => Some(nightly),
    _ => None,
};

/// The git commit this build was made from, short (12 hex): the browser build sets
/// `HORIZON_COMMIT` when it packages the single file (`web/build.sh`). None for a build that did
/// not record one.
pub const COMMIT: Option<&str> = match option_env!("HORIZON_COMMIT") {
    Some(commit) if !commit.is_empty() => Some(commit),
    _ => None,
};

/// What the title screen calls this build: `0.2.3` for a release, the commit for a build that
/// recorded one, else nothing. The player has no other way to say which build they are looking at.
pub fn build_label() -> Option<String> {
    if let Some(nightly) = nightly_label() {
        return Some(nightly);
    }
    COMMIT.map(|commit| format!("Build {commit}"))
}

/// How a nightly names itself where players read it, so their reports say which build they come
/// from: `Nightly 2026-10-28 · 0c02067`. None for any other build.
pub fn nightly_label() -> Option<String> {
    NIGHTLY.map(label)
}

/// A nightly's version as the launcher installs it (`nightly-2026-10-28-0c02067`).
pub fn nightly_version() -> Option<String> {
    NIGHTLY.map(|n| format!("nightly-{}", n.replace(' ', "-")))
}

fn label(nightly: &str) -> String {
    match nightly.split_once(' ') {
        Some((date, commit)) => format!("Nightly {date} · {commit}"),
        None => format!("Nightly {nightly}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_nightly_names_its_date_and_commit() {
        assert_eq!(
            super::label("2026-10-28 0c02067"),
            "Nightly 2026-10-28 · 0c02067"
        );
        assert_eq!(super::label("2026-10-28"), "Nightly 2026-10-28");
        assert_eq!(super::nightly_label().is_some(), super::NIGHTLY.is_some());
    }

    #[test]
    fn the_version_is_the_release_or_the_crate() {
        assert_eq!(
            super::RELEASE,
            option_env!("HORIZON_VERSION").filter(|v| !v.is_empty())
        );
        assert_eq!(
            super::VERSION,
            super::RELEASE.unwrap_or(env!("CARGO_PKG_VERSION"))
        );
        assert!(!super::VERSION.starts_with('v'));
    }

    /// The title screen's build line: a nightly names its date and commit, a
    /// browser build names its commit, and a release from a tag names neither
    /// (so nothing is drawn over the corner).
    #[test]
    fn the_build_label_names_what_made_this_build() {
        assert_eq!(super::label("2026-10-28 0c02067"), "Nightly 2026-10-28 · 0c02067");
        assert_eq!(super::build_label(), super::nightly_label().or_else(|| {
            super::COMMIT.map(|commit| format!("Build {commit}"))
        }));
        assert_eq!(
            super::COMMIT,
            option_env!("HORIZON_COMMIT").filter(|c| !c.is_empty()),
            "a build without the variable reports no commit"
        );
        assert!(
            super::build_label().is_none() || super::build_label().is_some_and(|l| l.len() < 48),
            "the label must fit the corner it is drawn in"
        );
    }
}
