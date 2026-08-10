use super::emerge::{self, Job};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One package a preset wants installed — the same shape as
/// `ui::onboarding::StarterPick`, just living in the domain layer so both
/// the GUI and the CLI companion can build a `Job` from it directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetPackage {
    pub atom: &'static str,
    pub blurb: &'static str,
}

/// One USE flag a preset wants set on a package — applied the same way a
/// `PendingRelaxation::Use` fix is (`package_use::set_flag`), just
/// authored ahead of time instead of parsed out of a failed build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetUseFlag {
    pub atom: &'static str,
    pub flag: &'static str,
    pub enabled: bool,
}

/// A small, named, shareable configuration: a handful of packages plus
/// the USE flags that make them work the way the preset's name promises
/// (e.g. "Gaming Desktop" wants `vulkan` on the graphics stack, not just
/// the launchers themselves installed with whatever USE flags happen to
/// be the ebuild defaults). Layers onto whatever's already on the
/// machine — applying one never removes a package or unsets a flag it
/// doesn't itself mention, unlike `profile_bundle`'s full-system import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preset {
    pub name: &'static str,
    pub description: &'static str,
    pub packages: &'static [PresetPackage],
    pub use_flags: &'static [PresetUseFlag],
}

/// Atoms verified against the real tree the same way
/// `onboarding::STARTER_BUNDLES` documents its own picks — several are the
/// exact same atoms, reused rather than re-verified from scratch.
pub const GAMING_DESKTOP: Preset = Preset {
    name: "Gaming Desktop",
    description: "Steam, Lutris, and Wine, with Vulkan enabled on the graphics stack they all depend on.",
    packages: &[
        PresetPackage { atom: "games-util/steam-launcher", blurb: "Steam" },
        PresetPackage { atom: "games-util/lutris", blurb: "Everything that isn't Steam" },
        PresetPackage { atom: "app-emulation/wine-staging", blurb: "Windows compatibility layer" },
    ],
    use_flags: &[
        PresetUseFlag { atom: "media-libs/mesa", flag: "vulkan", enabled: true },
        PresetUseFlag { atom: "media-libs/vulkan-loader", flag: "X", enabled: true },
    ],
};

pub const MINIMAL_SERVER: Preset = Preset {
    name: "Minimal Server",
    description: "The handful of tools a headless box always ends up needing, with X/Wayland pulled out of anything that defaults to bundling it.",
    packages: &[
        PresetPackage { atom: "app-admin/sudo", blurb: "Privilege escalation" },
        PresetPackage { atom: "sys-process/htop", blurb: "Process monitor" },
        PresetPackage { atom: "app-misc/tmux", blurb: "Terminal multiplexer" },
        PresetPackage { atom: "net-misc/rsync", blurb: "File sync/transfer" },
        PresetPackage { atom: "app-editors/nano", blurb: "A terminal editor that needs no manual" },
    ],
    use_flags: &[
        PresetUseFlag { atom: "app-admin/sudo", flag: "X", enabled: false },
    ],
};

pub const BUILTIN_PRESETS: &[Preset] = &[GAMING_DESKTOP, MINIMAL_SERVER];

/// An owned copy of `Preset` — what `import` produces, since a preset read
/// back from someone else's exported file isn't `'static` the way the
/// built-ins are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPreset {
    pub name: String,
    pub description: String,
    pub packages: Vec<OwnedPresetPackage>,
    pub use_flags: Vec<OwnedPresetUseFlag>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPresetPackage {
    pub atom: String,
    pub blurb: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPresetUseFlag {
    pub atom: String,
    pub flag: String,
    pub enabled: bool,
}

impl From<&Preset> for OwnedPreset {
    fn from(preset: &Preset) -> Self {
        Self {
            name: preset.name.to_string(),
            description: preset.description.to_string(),
            packages: preset
                .packages
                .iter()
                .map(|p| OwnedPresetPackage { atom: p.atom.to_string(), blurb: p.blurb.to_string() })
                .collect(),
            use_flags: preset
                .use_flags
                .iter()
                .map(|f| OwnedPresetUseFlag { atom: f.atom.to_string(), flag: f.flag.to_string(), enabled: f.enabled })
                .collect(),
        }
    }
}

/// Whether `atom` is shaped like a real Portage atom (`category/name`,
/// both non-empty, no whitespace) — not a full atom-syntax validator
/// (that's `pkgcraft`-shaped territory, deliberately not adopted — see
/// `binpkg::CachedVersion`), just enough to reject an obviously-malformed
/// import before it reaches `emerge`.
fn looks_like_an_atom(atom: &str) -> bool {
    let Some((category, name)) = atom.split_once('/') else { return false };
    !category.is_empty() && !name.is_empty() && !atom.contains(char::is_whitespace)
}

/// Writes `preset` to `dest` as JSON — unprivileged, since a preset is
/// metadata about what to install/enable, not a system file itself.
pub fn export(preset: &OwnedPreset, dest: &Path) -> Result<()> {
    let text = serde_json::to_string_pretty(preset).context("failed to serialize preset")?;
    std::fs::write(dest, text).with_context(|| format!("failed to write {}", dest.display()))
}

/// Reads a preset back from a file `export` (or a hand-written one
/// matching its shape) produced, validating it's actually usable before
/// handing it back — an import feeding a malformed atom straight to
/// `emerge`/`package_use::set_flag` would surface as a confusing failure
/// several steps later instead of here, at the one point that actually
/// knows what "malformed" means for this shape.
pub fn import(path: &Path) -> Result<OwnedPreset> {
    let text = std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let preset: OwnedPreset = serde_json::from_str(&text).context("not a valid preset file")?;
    if preset.name.trim().is_empty() {
        bail!("preset has no name");
    }
    if preset.packages.is_empty() && preset.use_flags.is_empty() {
        bail!("preset has no packages and no USE flags — nothing to apply");
    }
    for pkg in &preset.packages {
        if !looks_like_an_atom(&pkg.atom) {
            bail!("'{}' doesn't look like a package atom (expected category/name)", pkg.atom);
        }
    }
    for flag in &preset.use_flags {
        if !looks_like_an_atom(&flag.atom) {
            bail!("'{}' doesn't look like a package atom (expected category/name)", flag.atom);
        }
    }
    Ok(preset)
}

/// Applies every USE flag in `preset` — the same `package_use::set_flag`
/// call `ui::mod::PendingRelaxation::apply` already loops over for its own
/// (auto-detected, rather than pre-authored) USE-flag list. Privileged
/// under the hood (each `set_flag` call writes via `priv_write`), but
/// synchronous — call off the main thread from the GUI, or straight from
/// `main` in the CLI.
pub fn apply_use_flags(preset: &OwnedPreset) -> Result<()> {
    for flag in &preset.use_flags {
        super::package_use::set_flag(&flag.atom, &flag.flag, flag.enabled)
            .with_context(|| format!("failed to set {} on {}", flag.flag, flag.atom))?;
    }
    Ok(())
}

/// Builds the one `emerge` job that installs every package in `preset` —
/// a thin wrapper over the already-existing `emerge::install_many_job`,
/// the same primitive `profile_bundle`'s own atom-list install uses.
pub fn install_job(preset: &OwnedPreset, getbinpkg: bool) -> Job {
    let atoms: Vec<String> = preset.packages.iter().map(|p| p.atom.clone()).collect();
    emerge::install_many_job(&atoms, getbinpkg, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OwnedPreset {
        OwnedPreset {
            name: "Test Preset".to_string(),
            description: "A preset for tests.".to_string(),
            packages: vec![OwnedPresetPackage { atom: "www-client/firefox".to_string(), blurb: "Browser".to_string() }],
            use_flags: vec![OwnedPresetUseFlag { atom: "media-libs/mesa".to_string(), flag: "vulkan".to_string(), enabled: true }],
        }
    }

    fn temp_path() -> std::path::PathBuf {
        let unique =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        std::env::temp_dir().join(format!("portage-store-preset-test-{unique}.json"))
    }

    #[test]
    fn export_then_import_round_trips() {
        let path = temp_path();
        let preset = sample();
        export(&preset, &path).unwrap();
        let read_back = import(&path).unwrap();
        assert_eq!(read_back, preset);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn import_rejects_a_missing_name() {
        let path = temp_path();
        let mut preset = sample();
        preset.name = String::new();
        export(&preset, &path).unwrap();
        assert!(import(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn import_rejects_a_malformed_atom() {
        let path = temp_path();
        let mut preset = sample();
        preset.packages[0].atom = "not-an-atom".to_string();
        export(&preset, &path).unwrap();
        assert!(import(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn import_rejects_a_preset_with_nothing_to_apply() {
        let path = temp_path();
        let preset = OwnedPreset { name: "Empty".to_string(), description: String::new(), packages: Vec::new(), use_flags: Vec::new() };
        export(&preset, &path).unwrap();
        assert!(import(&path).is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn every_builtin_preset_atom_is_well_formed() {
        for preset in BUILTIN_PRESETS {
            for pkg in preset.packages {
                assert!(looks_like_an_atom(pkg.atom), "{} has a malformed atom: {}", preset.name, pkg.atom);
            }
            for flag in preset.use_flags {
                assert!(looks_like_an_atom(flag.atom), "{} has a malformed USE-flag atom: {}", preset.name, flag.atom);
            }
        }
    }

    #[test]
    fn no_duplicate_atom_within_a_single_builtin_preset() {
        for preset in BUILTIN_PRESETS {
            let mut atoms: Vec<&str> = preset.packages.iter().map(|p| p.atom).collect();
            let count = atoms.len();
            atoms.sort_unstable();
            atoms.dedup();
            assert_eq!(atoms.len(), count, "{} lists the same package atom more than once", preset.name);
        }
    }
}
