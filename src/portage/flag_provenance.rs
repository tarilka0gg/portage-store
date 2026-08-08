use super::make_conf;
use std::path::{Path, PathBuf};

/// Where a USE flag's currently-effective value actually comes from —
/// checked in the same priority order Portage itself resolves USE flags
/// in (most to least specific), so this always reports whichever source
/// is the one actually winning, not just the first one that happens to
/// mention the flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagSource {
    /// Set for this exact atom in one `package.use` file — the file name
    /// only (`zz-portage-store`, or someone's own hand-edited file), not
    /// the full path, since that's the part worth showing.
    PackageUse(String),
    /// Set (or unset) in the global `USE=` variable in `make.conf`.
    MakeConf,
    /// Not overridden anywhere this app checked — whatever value is in
    /// effect comes from the profile's own defaults or the ebuild's own
    /// `IUSE` default (`+flag`), neither of which this traces further.
    Default,
}

impl FlagSource {
    pub fn label(&self) -> String {
        match self {
            Self::PackageUse(file) => format!("Set in package.use/{file}"),
            Self::MakeConf => "Set in make.conf".to_string(),
            Self::Default => "Profile or ebuild default".to_string(),
        }
    }
}

const PACKAGE_USE_PATH: &str = "/etc/portage/package.use";

/// Every `(path, contents)` pair under `package.use` — a single file or a
/// directory of them, the same dual-form convention Portage's own config
/// directories all follow.
fn package_use_files() -> Vec<(PathBuf, String)> {
    let root = Path::new(PACKAGE_USE_PATH);
    if root.is_dir() {
        let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
        entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok().map(|content| (e.path(), content)))
            .collect()
    } else if let Ok(content) = std::fs::read_to_string(root) {
        vec![(root.to_path_buf(), content)]
    } else {
        Vec::new()
    }
}

/// Whether `content` (one `package.use` file's text) has a line for
/// `atom` mentioning `flag` — an exact-atom match only, the same as
/// Portage's own matching for a fully-qualified atom (this app's own
/// managed overrides, and anything else written the same way, always are
/// one). A line for a *different* atom that happens to also match this
/// package (a bare category/name versus this exact version, say) isn't
/// checked here — the common case this exists for is "which line did the
/// GUI or I write for this exact atom", not full atom-matching semantics.
fn mentions_flag(content: &str, atom: &str, flag: &str) -> bool {
    content.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).any(|line| {
        let mut tokens = line.split_whitespace();
        if tokens.next() != Some(atom) {
            return false;
        }
        tokens.any(|tok| tok.trim_start_matches('-') == flag)
    })
}

fn find_in_package_use(atom: &str, flag: &str) -> Option<String> {
    for (path, content) in package_use_files() {
        if mentions_flag(&content, atom, flag) {
            return path.file_name().and_then(|n| n.to_str()).map(str::to_string);
        }
    }
    None
}

/// Whether `make.conf`'s global `USE=` variable mentions `flag` at all
/// (enabling or disabling it) — `+flag` isn't valid syntax in `USE=`
/// itself (that's an `IUSE`/`package.use` default-marker convention), so
/// only a bare `flag` or `-flag` token counts.
fn make_conf_mentions_flag(flag: &str) -> bool {
    let Ok(raw) = make_conf::read_raw() else { return false };
    let vars = make_conf::parse_vars(&raw);
    let Some(use_var) = vars.get("USE") else { return false };
    use_var.split_whitespace().any(|tok| tok.trim_start_matches('-') == flag)
}

/// Where `flag`'s current value for `atom` comes from — the one call
/// site this module is for.
pub fn locate(atom: &str, flag: &str) -> FlagSource {
    if let Some(file) = find_in_package_use(atom, flag) {
        return FlagSource::PackageUse(file);
    }
    if make_conf_mentions_flag(flag) {
        return FlagSource::MakeConf;
    }
    FlagSource::Default
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_an_exact_atom_and_flag_match() {
        let content = "# a comment\n>=media-libs/libsdl2-2.32.8 X opengl\ngames-util/lutris -python_single_target_python3_12\n";
        assert!(mentions_flag(content, ">=media-libs/libsdl2-2.32.8", "opengl"));
        assert!(mentions_flag(content, "games-util/lutris", "python_single_target_python3_12"));
        assert!(!mentions_flag(content, "games-util/lutris", "opengl"));
    }

    #[test]
    fn a_disabled_flag_still_counts_as_mentioned() {
        let content = "games-util/lutris -python_single_target_python3_12\n";
        assert!(mentions_flag(content, "games-util/lutris", "python_single_target_python3_12"));
    }

    #[test]
    fn comment_lines_and_other_atoms_do_not_match() {
        let content = "# other-atom/pkg flag\nother-atom/pkg flag\n";
        assert!(!mentions_flag(content, "some-atom/pkg", "flag"));
    }

    #[test]
    fn make_conf_use_var_tokens_match_with_or_without_the_minus() {
        assert!("wayland clang -X -telemetry".split_whitespace().any(|t| t.trim_start_matches('-') == "X"));
        assert!("wayland clang -X -telemetry".split_whitespace().any(|t| t.trim_start_matches('-') == "wayland"));
        assert!(!"wayland clang -X -telemetry".split_whitespace().any(|t| t.trim_start_matches('-') == "pulseaudio"));
    }

    #[test]
    fn label_wording() {
        assert_eq!(FlagSource::PackageUse("zz-portage-store".to_string()).label(), "Set in package.use/zz-portage-store");
        assert_eq!(FlagSource::MakeConf.label(), "Set in make.conf");
        assert_eq!(FlagSource::Default.label(), "Profile or ebuild default");
    }
}
