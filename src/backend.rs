//! The seam between "where a package comes from" and "how the UI shows
//! it". Portage and Flatpak have genuinely different semantics — one has
//! USE flags, dependency accounting and a system-wide root lock; the
//! other is a sandboxed, rootless, single-file-per-app install with none
//! of that — so the UI is meant to branch on `Caps`, not on a source
//! check sprinkled through every screen.
//!
//! This module intentionally does *not* pull the existing Portage
//! search/detail/install pipeline (`portage::eix`, `ui::detail`, the job
//! queue) onto a `Backend` trait wholesale — that pipeline is already
//! Portage-shaped throughout the UI, and forcing it through a generic
//! interface today would touch nearly every screen for no behavioral
//! gain yet. What lives here is the part that actually needs to be
//! source-agnostic right now: telling two sources' results apart (or
//! recognizing they're the same app), and letting the UI ask "what can I
//! even do with this" instead of hardcoding it.

use crate::flatpak;
use crate::portage::{eix, flathub};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceId {
    Portage,
    Flatpak,
}

/// What a source can and can't do — the UI reads this instead of
/// checking `source == SourceId::Flatpak` at every call site. Adding a
/// third backend later means filling in one more `Caps` value, not
/// hunting down every place that assumed there were only two.
#[derive(Debug, Clone, Copy)]
pub struct Caps {
    /// Installing/removing needs `pkexec` and touches the whole system.
    pub needs_root: bool,
    /// Has a real dependency graph worth showing (reverse deps, orphan
    /// cleanup) — Flatpak's runtime/extension relationships aren't this.
    pub has_deps: bool,
    /// USE flags exist as a concept for this source.
    pub use_flags: bool,
    /// Installs are sandboxed by the source itself (Flatpak's own
    /// per-app sandbox), independent of this app's own `sandbox.rs`.
    pub sandboxed: bool,
    /// A "Build Time" figure is meaningful — never true for a source that
    /// only ever ships prebuilt artifacts.
    pub shows_build_time: bool,
}

impl SourceId {
    pub const fn caps(self) -> Caps {
        match self {
            SourceId::Portage => Caps { needs_root: true, has_deps: true, use_flags: true, sandboxed: false, shows_build_time: true },
            SourceId::Flatpak => Caps { needs_root: false, has_deps: false, use_flags: false, sandboxed: true, shows_build_time: false },
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            SourceId::Portage => "Portage",
            SourceId::Flatpak => "Flatpak",
        }
    }
}

/// Reduces a name to comparable letters and digits — the same shape as
/// `flathub::normalize`, duplicated here rather than made `pub` there
/// since this one also needs to strip Gentoo's own packaging suffixes,
/// which is meaningless for a Flatpak app name.
fn normalize(name: &str) -> String {
    name.trim_end_matches("-bin").trim_end_matches("-git").chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

/// Best-effort "is this Flatpak app the same program as this Portage
/// package" check, in order of confidence:
///
/// 1. A cached Flathub lookup for the Portage package name already
///    resolved to this exact `app_id` (see `flathub.rs` — the same
///    lookup the detail page already does for screenshots, reused here
///    rather than making a second request for the same answer). No
///    network call is made if nothing's cached yet; an uncached package
///    just doesn't get a chip, it doesn't block or slow the merge down.
/// 2. Falling that, the normalized names match — checked against *both*
///    the Flatpak app's display name and the last segment of its
///    reverse-DNS id. The display name alone isn't enough: real
///    `flatpak search` output names `org.gimp.GIMP` "GNU Image
///    Manipulation Program", nothing like the Portage package's own
///    `gimp` — confirmed against the live `flatpak`/`eix` output while
///    building this, not assumed. The id tail is what actually carries
///    the recognizable name in cases like that one. Still no fuzzier
///    than an exact (suffix-stripped) match on either side, matching
///    `flathub::is_confident_match`'s same two-pronged shape: showing
///    the same app twice is a much smaller mistake than silently
///    merging two different ones.
fn same_app(portage_name: &str, flatpak_app: &flatpak::FlatpakApp) -> bool {
    if let Some(cached) = flathub::cached(portage_name)
        && cached.app_id == flatpak_app.app_id
    {
        return true;
    }
    let wanted = normalize(portage_name);
    if wanted.len() < 3 {
        return false;
    }
    let id_tail = flatpak_app.app_id.rsplit('.').next().unwrap_or_default();
    wanted == normalize(&flatpak_app.name) || wanted == normalize(id_tail)
}

/// Finds a confident Flatpak match for a single Portage package name among
/// `hits` (e.g. a `flatpak::search` run against that same name) — the
/// single-package version of `merge_search_results`' own matching, used
/// by the detail page to offer "Install via Flatpak" without needing a
/// full search-results merge in hand.
pub fn find_flatpak_match(portage_name: &str, hits: &[flatpak::FlatpakApp]) -> Option<flatpak::FlatpakApp> {
    hits.iter().find(|app| same_app(portage_name, app)).cloned()
}

/// The result of reconciling one query's Portage and Flatpak hits.
/// Portage stays authoritative for order and content — the caller already
/// has its own Portage result list on screen; `chips` is purely additive
/// metadata for the cards drawn from it, and `flatpak_only` is a separate
/// list the UI renders as a collapsed section beneath the main results,
/// never interleaved with them.
pub struct MergedSearch {
    pub chips: std::collections::HashMap<String, Vec<flatpak::FlatpakApp>>,
    pub flatpak_only: Vec<flatpak::FlatpakApp>,
}

/// Deduplicates `flatpak_hits` (the same app can appear once per remote)
/// and reconciles them against `portage_hits`, keyed by atom.
pub fn merge_search_results(portage_hits: &[eix::PackageSummary], flatpak_hits: &[flatpak::FlatpakApp]) -> MergedSearch {
    let mut by_app_id: std::collections::HashMap<&str, &flatpak::FlatpakApp> = std::collections::HashMap::new();
    for app in flatpak_hits {
        by_app_id.entry(&app.app_id).or_insert(app);
    }

    let mut chips: std::collections::HashMap<String, Vec<flatpak::FlatpakApp>> = std::collections::HashMap::new();
    let mut flatpak_only: Vec<flatpak::FlatpakApp> = Vec::new();

    for app in by_app_id.into_values() {
        match portage_hits.iter().find(|pkg| same_app(&pkg.name, app)) {
            Some(pkg) => chips.entry(pkg.atom()).or_default().push(app.clone()),
            None => flatpak_only.push(app.clone()),
        }
    }
    flatpak_only.sort_by(|a, b| a.name.cmp(&b.name));

    MergedSearch { chips, flatpak_only }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portage_pkg(category: &str, name: &str) -> eix::PackageSummary {
        eix::PackageSummary {
            category: category.to_string(),
            name: name.to_string(),
            description: String::new(),
            homepage: String::new(),
            license: String::new(),
            latest_version: String::new(),
            iuse: Vec::new(),
            masked: false,
            overlay: None,
        }
    }

    fn flatpak_app(app_id: &str, name: &str) -> flatpak::FlatpakApp {
        flatpak::FlatpakApp { app_id: app_id.to_string(), name: name.to_string(), description: String::new(), version: String::new(), remote: "flathub".to_string() }
    }

    #[test]
    fn exact_normalized_name_match_becomes_a_chip_not_a_duplicate_row() {
        let portage = vec![portage_pkg("media-gfx", "gimp")];
        let flatpak = vec![flatpak_app("org.gimp.GIMP", "GIMP")];
        let merged = merge_search_results(&portage, &flatpak);
        assert!(merged.flatpak_only.is_empty());
        assert_eq!(merged.chips.get("media-gfx/gimp").unwrap()[0].app_id, "org.gimp.GIMP");
    }

    #[test]
    fn id_tail_carries_the_match_when_the_display_name_does_not() {
        // Real `flatpak search` output: `org.gimp.GIMP`'s own display name
        // is "GNU Image Manipulation Program", nothing like "gimp" — the
        // id tail is what actually has to carry this match.
        let portage = vec![portage_pkg("media-gfx", "gimp")];
        let flatpak = vec![flatpak_app("org.gimp.GIMP", "GNU Image Manipulation Program")];
        let merged = merge_search_results(&portage, &flatpak);
        assert!(merged.flatpak_only.is_empty());
        assert!(merged.chips.contains_key("media-gfx/gimp"));
    }

    #[test]
    fn unmatched_flatpak_hit_goes_to_flatpak_only() {
        let portage = vec![portage_pkg("media-gfx", "gimp")];
        let flatpak = vec![flatpak_app("org.upscayl.Upscayl", "Upscayl")];
        let merged = merge_search_results(&portage, &flatpak);
        assert!(merged.chips.is_empty());
        assert_eq!(merged.flatpak_only.len(), 1);
        assert_eq!(merged.flatpak_only[0].app_id, "org.upscayl.Upscayl");
    }

    #[test]
    fn duplicate_remotes_for_the_same_app_id_collapse_to_one_entry() {
        let portage = vec![];
        let flatpak = vec![flatpak_app("org.gimp.GIMP", "GIMP"), flatpak_app("org.gimp.GIMP", "GIMP")];
        let merged = merge_search_results(&portage, &flatpak);
        assert_eq!(merged.flatpak_only.len(), 1);
    }

    #[test]
    fn gentoo_packaging_suffixes_do_not_block_a_name_match() {
        let portage = vec![portage_pkg("www-client", "zen-bin")];
        let flatpak = vec![flatpak_app("app.zen_browser.zen", "zen")];
        let merged = merge_search_results(&portage, &flatpak);
        assert!(merged.flatpak_only.is_empty());
        assert!(merged.chips.contains_key("www-client/zen-bin"));
    }

    #[test]
    fn caps_reflect_the_real_differences_between_sources() {
        assert!(SourceId::Portage.caps().needs_root);
        assert!(!SourceId::Flatpak.caps().needs_root);
        assert!(!SourceId::Flatpak.caps().use_flags);
        assert!(!SourceId::Flatpak.caps().shows_build_time);
    }
}
