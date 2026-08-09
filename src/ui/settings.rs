use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persisted, user-facing app preferences — small enough not to warrant a
/// GSettings schema (which would need installing alongside the binary);
/// a plain JSON file under XDG_CONFIG_HOME is the whole mechanism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Whether GitHub's repo-level social-preview image is shown as a
    /// last-resort illustration when a package's README has no screenshot
    /// of its own. On by default since it's often a genuinely useful
    /// stats card — but a repo's maintainer can set that image to
    /// literally anything in their GitHub settings, unrelated to what the
    /// software does, and there's no way to tell the two cases apart from
    /// the API alone. Advanced/off-by-request rather than removed, since
    /// it's still the right default for most repos.
    #[serde(default = "default_true")]
    pub github_page_preview: bool,
    /// Whether install/update runs pass `--getbinpkg`, letting a
    /// configured binary repo (Gentoo's official binhost by default on a
    /// modern stage3, or a custom `binrepos.conf` entry) satisfy a
    /// package instead of building it from source. On by default — a
    /// prebuilt match is strictly faster with no downside when one
    /// exists, and nothing changes for packages the binhost doesn't have.
    #[serde(default = "default_true")]
    pub prefer_binary_packages: bool,
    /// Whether build/install jobs run under `nice`/`ionice` (idle I/O
    /// class) with `MAKEOPTS` capped to a RAM- and core-aware job count
    /// instead of whatever's in `make.conf`. On by default: an
    /// unthrottled parallel build can exhaust RAM and get processes
    /// unrelated to the build killed by the OOM killer — a real failure
    /// mode, not a hypothetical one, since it happened once during this
    /// app's own testing.
    #[serde(default = "default_true")]
    pub throttle_builds: bool,
    /// Whether mutating jobs (installs, updates, removals) should wait
    /// for the configured night window instead of starting immediately.
    /// Off by default — most people queuing a build want it to just
    /// start; this is for the "leave it running overnight" workflow
    /// specifically, not a universal default.
    #[serde(default)]
    pub night_builds_only: bool,
    /// Whether the health checks (news, config updates, GLSAs, orphaned
    /// packages) re-run on their own every few hours instead of only at
    /// startup or when the health dashboard is actually opened. Off by
    /// default — a background check every few hours is a reasonable
    /// thing to opt into, not a reasonable thing to do to someone by
    /// default. Separate from `night_builds_only`: this is read-only
    /// (no build, no resource cost worth gating to off-hours), so it
    /// runs whenever its own timer fires.
    #[serde(default)]
    pub periodic_health_checks: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            github_page_preview: true,
            prefer_binary_packages: true,
            throttle_builds: true,
            night_builds_only: false,
            periodic_health_checks: false,
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let dir = base.join("portage-store");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("settings.json"))
}

pub fn load() -> Settings {
    path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(settings: &Settings) {
    let Some(path) = path() else { return };
    if let Ok(text) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, text);
    }
}
