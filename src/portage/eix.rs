use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Default, Deserialize)]
struct EixDump {
    #[serde(rename = "category", default)]
    categories: Vec<EixCategory>,
}

/// Parses one `eix --xml` invocation's output. `eix` prints genuinely
/// empty stdout (not even an `<eixdump/>` wrapper) when a query matches
/// nothing — every call site here used to feed that straight into
/// `quick_xml`, which fails on an empty string ("unexpected Event::Eof"),
/// turning every single zero-match search into an error instead of the
/// empty result it actually is. Zero-match searches are the *common* case
/// for anything typo'd, so this wasn't a rare edge case.
fn parse_eix_xml(xml: &str) -> Result<EixDump> {
    if xml.trim().is_empty() {
        return Ok(EixDump::default());
    }
    quick_xml::de::from_str(xml).context("failed to parse eix XML output")
}

#[derive(Debug, Deserialize)]
struct EixCategory {
    #[serde(rename = "@name")]
    name: String,
    #[serde(rename = "package", default)]
    packages: Vec<EixPackage>,
}

#[derive(Debug, Deserialize)]
struct EixPackage {
    #[serde(rename = "@name")]
    name: String,
    description: Option<String>,
    homepage: Option<String>,
    licenses: Option<String>,
    #[serde(rename = "version", default)]
    versions: Vec<EixVersion>,
}

#[derive(Debug, Deserialize)]
struct EixVersion {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@repository", default)]
    repository: Option<String>,
    #[serde(rename = "iuse", default)]
    iuse: Vec<IuseEntry>,
    #[serde(rename = "mask", default)]
    masks: Vec<MaskEntry>,
}

/// eix emits one `<mask type="hard"/>` for a `package.mask` entry and one
/// `<mask type="keyword"/>` for a version that's merely keyword-masked
/// (not in `ACCEPT_KEYWORDS`, e.g. `~amd64` without testing enabled) — the
/// type string itself is never used, presence of the element is all that
/// matters here.
#[derive(Debug, Deserialize)]
struct MaskEntry {
    #[serde(rename = "@type", default)]
    #[allow(dead_code)]
    kind: String,
}

/// eix splits IUSE into one or more `<iuse>` elements: flags default-enabled
/// by the ebuild (originally `+flag` in IUSE) get their own element with
/// `default="1"`, everything else lands in a plain `<iuse>` with no
/// attribute.
#[derive(Debug, Deserialize)]
struct IuseEntry {
    #[serde(rename = "@default", default)]
    default: Option<String>,
    #[serde(rename = "$text", default)]
    text: String,
}

/// A single IUSE entry as advertised by the ebuild: the flag name and
/// whether it defaults to enabled (prefixed with `+` in IUSE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseFlag {
    pub name: String,
    pub default_enabled: bool,
}

fn parse_iuse_entries(entries: &[IuseEntry]) -> Vec<UseFlag> {
    let mut flags = Vec::new();
    for entry in entries {
        let default_enabled = entry.default.as_deref() == Some("1");
        for tok in entry.text.split_whitespace() {
            flags.push(UseFlag {
                name: tok.to_string(),
                default_enabled,
            });
        }
    }
    flags
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSummary {
    pub category: String,
    pub name: String,
    pub description: String,
    pub homepage: String,
    pub license: String,
    pub latest_version: String,
    pub iuse: Vec<UseFlag>,
    /// Whether the latest version is masked (hard `package.mask` or
    /// keyword-masked) — the same version the rest of the card/detail UI
    /// already treats as "the" version of this package.
    pub masked: bool,
    /// The overlay this package's latest version comes from, e.g. `"guru"`
    /// — `None` for anything in the main Gentoo tree.
    pub overlay: Option<String>,
}

impl PackageSummary {
    pub fn atom(&self) -> String {
        format!("{}/{}", self.category, self.name)
    }
}

fn run_eix_xml(args: &[&str]) -> Result<String> {
    let output = Command::new("eix")
        .arg("--xml")
        .args(args)
        .output()
        .context("failed to run eix (is app-portage/eix installed?)")?;

    // eix exits with status 1 when a search yields no results; that's not
    // an error condition for us, just an empty result set.
    if !output.status.success() && output.status.code() != Some(1) {
        anyhow::bail!(
            "eix exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Every atom ("category/name") in the portage tree, one per line — cheap
/// enough (under 200ms for the whole tree, measured directly) to fetch
/// fresh on every fuzzy search pass below rather than caching it and
/// risking a stale list after a sync.
fn all_atoms() -> Result<Vec<String>> {
    let output = Command::new("eix")
        .arg("--only-names")
        .output()
        .context("failed to run eix (is app-portage/eix installed?)")?;
    if !output.status.success() {
        anyhow::bail!(
            "eix --only-names exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).collect())
}

fn dump_to_summaries(dump: EixDump) -> Vec<PackageSummary> {
    dump.categories
        .into_iter()
        .flat_map(|cat| {
            cat.packages.into_iter().map(move |pkg| {
                let latest = pkg.versions.last();
                PackageSummary {
                    category: cat.name.clone(),
                    name: pkg.name,
                    description: pkg.description.unwrap_or_default(),
                    homepage: pkg.homepage.unwrap_or_default(),
                    license: pkg.licenses.unwrap_or_default(),
                    latest_version: latest.map(|v| v.id.clone()).unwrap_or_default(),
                    iuse: latest.map(|v| parse_iuse_entries(&v.iuse)).unwrap_or_default(),
                    masked: latest.is_some_and(|v| !v.masks.is_empty()),
                    overlay: latest.and_then(|v| v.repository.clone()),
                }
            })
        })
        .collect()
}

/// Search the portage tree by name/description substring.
pub fn search(query: &str) -> Result<Vec<PackageSummary>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    // eix's default search field is the package *name* only (`-s`), which
    // misses anything a user would recognise by what it does rather than
    // what it's called — searching "password" finds packages literally
    // named `1password`, but not `bitwarden-desktop-bin` or `keepass`,
    // whose names say nothing about passwords even though that's exactly
    // what they're for. Description search (`-S`) catches those; running
    // both and merging covers what either alone misses.
    let mut by_atom: std::collections::HashMap<String, PackageSummary> = std::collections::HashMap::new();
    for flag in ["-s", "-S"] {
        let xml = run_eix_xml(&[flag, query])?;
        let dump = parse_eix_xml(&xml)?;
        for pkg in dump_to_summaries(dump) {
            by_atom.entry(pkg.atom()).or_insert(pkg);
        }
    }

    let mut results: Vec<PackageSummary> = by_atom.into_values().collect();
    results.sort_by(|a, b| relevance_rank(a, query).cmp(&relevance_rank(b, query)).then_with(|| a.name.cmp(&b.name)));

    // Typo tolerance: only bother computing our own fuzzy matches when the
    // exact pass above came up empty, or its best hit is a weak one (rank
    // 2+ — a plain substring or description-only match, not a name match).
    // A strong hit already means the query wasn't a typo worth second-
    // guessing, and this pass is not cheap: `all_atoms()` shells out to
    // `eix --only-names` and Levenshtein-scores every one of the ~20k
    // results, on *every* search, typo or not, if run unconditionally.
    let best_rank = results.first().map(|pkg| relevance_rank(pkg, query));
    if best_rank.is_none_or(|rank| rank >= 2) {
        let query_lower = query.to_lowercase();
        if let Ok(names) = all_atoms() {
            let existing: std::collections::HashSet<String> = results.iter().map(PackageSummary::atom).collect();
            // Scales with query length rather than a flat cutoff: 3 stray
            // characters ruin a 6-character query far more than an
            // 18-character one, so the tolerance should grow with what's
            // actually being typed instead of penalizing longer names for
            // their length.
            let max_distance = (query_lower.chars().count() / 3).clamp(2, 5);
            let mut candidates: Vec<(usize, String)> = names
                .into_iter()
                .filter(|atom| !existing.contains(atom))
                .filter_map(|atom| {
                    let name = atom.split('/').next_back().unwrap_or(&atom);
                    let distance = levenshtein(&name.to_lowercase(), &query_lower);
                    (distance <= max_distance).then_some((distance, atom))
                })
                .collect();
            candidates.sort_by_key(|(distance, _)| *distance);
            // Nothing matched at all — most likely a typo, since a genuine
            // "no such tool in the tree" is comparatively rare — gets more
            // room than the "did you mean" hint appended below a page of
            // otherwise-weak real hits.
            candidates.truncate(if results.is_empty() { 15 } else { 5 });
            let fuzzy = candidates.into_iter().filter_map(|(_, atom)| lookup(&atom).ok().flatten());
            results.extend(fuzzy);
        }
    }

    Ok(results)
}

/// How well a package matches the typed query, lower is better. Plain
/// substring search (what `search` runs) finds a match anywhere in the
/// name or description with no sense of *how well* it matches — this is
/// what turns that into an actual ranking, so typing "fire" surfaces
/// `firefox` first instead of leaving it buried between `coldfire` and
/// `wayfire-plugins-extra` in whatever order eix happened to return.
fn relevance_rank(pkg: &PackageSummary, query: &str) -> u8 {
    let query = query.to_lowercase();
    let name = pkg.name.to_lowercase();
    if name == query {
        0
    } else if name.starts_with(&query) {
        1
    } else if name.contains(&query) {
        2
    } else {
        // Only reachable via a description-only match — the name doesn't
        // contain the query at all.
        3
    }
}

/// Levenshtein edit distance, for ranking eix's fuzzy-search results by
/// actual closeness to what was typed — eix's own output order for `-f`
/// doesn't reflect distance (a query for "dockr" returns `docker` second,
/// not first), so re-sorting here is what makes "closest match first"
/// true rather than incidental.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev_diag = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let cur = row[j];
            row[j] = if a[i - 1] == b[j - 1] {
                prev_diag
            } else {
                1 + prev_diag.min(row[j]).min(row[j - 1])
            };
            prev_diag = cur;
        }
    }
    row[b.len()]
}


/// How to order an already-filtered result list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    /// Whatever order the results arrived in — relevance rank for a text
    /// search, alphabetical for a category browse.
    #[default]
    Default,
    NameAsc,
    NameDesc,
    LicenseAsc,
}

/// A USE flag constraint: the package's IUSE must (or must not) contain
/// this exact flag name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UseConstraint {
    pub flag: String,
    pub must_be_set: bool,
}

/// Search-results filters — applied in memory over an already-fetched
/// result list rather than folded into the `eix` query itself, so
/// changing a filter re-renders instantly instead of re-running a search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub use_flag: Option<UseConstraint>,
    /// `Some(true)` — masked only, `Some(false)` — unmasked only, `None`
    /// — either.
    pub masked: Option<bool>,
    /// Only packages that come from an overlay (not the main tree).
    pub overlay_only: bool,
    /// Case-insensitive substring match against the license field.
    pub license: Option<String>,
    pub sort: SortOrder,
}

impl SearchFilters {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

fn matches_filters(pkg: &PackageSummary, filters: &SearchFilters) -> bool {
    if let Some(constraint) = &filters.use_flag {
        let has_flag = pkg.iuse.iter().any(|f| f.name == constraint.flag);
        if has_flag != constraint.must_be_set {
            return false;
        }
    }
    if let Some(want_masked) = filters.masked
        && pkg.masked != want_masked
    {
        return false;
    }
    if filters.overlay_only && pkg.overlay.is_none() {
        return false;
    }
    if let Some(license) = &filters.license
        && !license.is_empty()
        && !pkg.license.to_lowercase().contains(&license.to_lowercase())
    {
        return false;
    }
    true
}

/// Filters and sorts a result list already produced by `search` or
/// `list_categories` — pure and synchronous, meant to be re-run on every
/// filter/sort change without touching `eix` again.
pub fn apply_filters(mut packages: Vec<PackageSummary>, filters: &SearchFilters) -> Vec<PackageSummary> {
    packages.retain(|pkg| matches_filters(pkg, filters));
    match filters.sort {
        SortOrder::Default => {}
        SortOrder::NameAsc => packages.sort_by(|a, b| a.name.cmp(&b.name)),
        SortOrder::NameDesc => packages.sort_by(|a, b| b.name.cmp(&a.name)),
        SortOrder::LicenseAsc => packages.sort_by(|a, b| a.license.cmp(&b.license).then_with(|| a.name.cmp(&b.name))),
    }
    packages
}

/// Lists every package across a set of portage categories in one shot, via
/// a `^(cat1|cat2|...)$` regex passed to `eix -C`. Used to power the
/// curated "browse by category" groups (games, desktop, dev tools, ...)
/// that map onto several raw portage categories at once.
pub fn list_categories(categories: &[&str]) -> Result<Vec<PackageSummary>> {
    if categories.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = format!("^({})$", categories.join("|"));
    let xml = run_eix_xml(&["-C", &pattern])?;
    let dump = parse_eix_xml(&xml)?;
    let mut summaries = dump_to_summaries(dump);
    summaries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(summaries)
}

/// Look up a single package by its full atom (category/name).
pub fn lookup(atom: &str) -> Result<Option<PackageSummary>> {
    let xml = run_eix_xml(&["-e", atom])?;
    let dump = parse_eix_xml(&xml)?;
    Ok(dump_to_summaries(dump).into_iter().next())
}

/// Every version of a package the tree currently offers, oldest first —
/// `dump_to_summaries` only keeps the last one (the one shown everywhere
/// else in the app), which is exactly what the version picker needs more
/// than.
pub fn list_versions(atom: &str) -> Result<Vec<String>> {
    let xml = run_eix_xml(&["-e", atom])?;
    let dump = parse_eix_xml(&xml)?;
    Ok(dump
        .categories
        .into_iter()
        .flat_map(|cat| cat.packages)
        .flat_map(|pkg| pkg.versions)
        .map(|v| v.id)
        .collect())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(category: &str, name: &str) -> PackageSummary {
        PackageSummary {
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

    #[test]
    fn use_flag_filter_keeps_only_packages_with_the_flag_set() {
        let mut with_flag = pkg("net-analyzer", "wireshark");
        with_flag.iuse.push(UseFlag { name: "gui".to_string(), default_enabled: true });
        let without_flag = pkg("net-analyzer", "tshark");

        let filters = SearchFilters {
            use_flag: Some(UseConstraint { flag: "gui".to_string(), must_be_set: true }),
            ..Default::default()
        };
        let result = apply_filters(vec![with_flag.clone(), without_flag], &filters);
        assert_eq!(result, vec![with_flag]);
    }

    #[test]
    fn use_flag_filter_can_require_absence() {
        let mut with_flag = pkg("net-analyzer", "wireshark");
        with_flag.iuse.push(UseFlag { name: "gui".to_string(), default_enabled: true });
        let without_flag = pkg("net-analyzer", "tshark");

        let filters = SearchFilters {
            use_flag: Some(UseConstraint { flag: "gui".to_string(), must_be_set: false }),
            ..Default::default()
        };
        let result = apply_filters(vec![with_flag, without_flag.clone()], &filters);
        assert_eq!(result, vec![without_flag]);
    }

    #[test]
    fn masked_filter_and_overlay_filter_and_license_filter_compose() {
        let mut masked_overlay = pkg("net-dns", "blocky");
        masked_overlay.masked = true;
        masked_overlay.overlay = Some("guru".to_string());
        masked_overlay.license = "GPL-3".to_string();

        let mut unmasked_tree = pkg("www-client", "firefox");
        unmasked_tree.license = "MPL-2.0".to_string();

        let packages = vec![masked_overlay.clone(), unmasked_tree.clone()];

        assert_eq!(apply_filters(packages.clone(), &SearchFilters { masked: Some(true), ..Default::default() }), vec![masked_overlay.clone()]);
        assert_eq!(apply_filters(packages.clone(), &SearchFilters { overlay_only: true, ..Default::default() }), vec![masked_overlay.clone()]);
        assert_eq!(
            apply_filters(packages.clone(), &SearchFilters { license: Some("mpl".to_string()), ..Default::default() }),
            vec![unmasked_tree]
        );
    }

    #[test]
    fn name_sort_orders_are_reversible() {
        let a = pkg("app-misc", "alpha");
        let b = pkg("app-misc", "beta");
        let asc = apply_filters(vec![b.clone(), a.clone()], &SearchFilters { sort: SortOrder::NameAsc, ..Default::default() });
        assert_eq!(asc.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
        let desc = apply_filters(vec![a, b], &SearchFilters { sort: SortOrder::NameDesc, ..Default::default() });
        assert_eq!(desc.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["beta", "alpha"]);
    }

    #[test]
    fn exact_name_ranks_above_prefix_which_ranks_above_substring() {
        assert!(relevance_rank(&pkg("www-client", "firefox"), "firefox") < relevance_rank(&pkg("app-misc", "firefox-decrypt"), "firefox"));
        assert!(relevance_rank(&pkg("app-misc", "firefox-decrypt"), "firefox") < relevance_rank(&pkg("games-misc", "wayfire"), "fire"));
    }

    #[test]
    fn name_match_always_beats_description_only_match() {
        let mut with_name_desc = pkg("app-misc", "coldfire");
        with_name_desc.description = "unrelated".to_string();
        let name_rank = relevance_rank(&with_name_desc, "fire");

        let description_only = pkg("app-crypt", "bitwarden-desktop-bin");
        let description_rank = relevance_rank(&description_only, "fire");

        assert!(name_rank < description_rank);
    }

    #[test]
    fn levenshtein_matches_known_distances() {
        assert_eq!(levenshtein("docker", "docker"), 0);
        assert_eq!(levenshtein("dockr", "docker"), 1);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn levenshtein_ranks_closer_typos_first() {
        let mut candidates = ["mock".to_string(), "docker".to_string(), "odoc".to_string()];
        candidates.sort_by_key(|c| levenshtein(c, "dockr"));
        assert_eq!(candidates[0], "docker");
    }

    #[test]
    fn list_versions_extracts_every_version_id() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<eixdump version="16">
  <category name="app-editors">
    <package name="neovim">
      <description>Vim-fork focused on extensibility and agility</description>
      <version id="0.11.6-r2" EAPI="8"/>
      <version id="0.11.7" EAPI="8" installed="1"/>
      <version id="0.12.3" EAPI="8"/>
      <version id="9999" EAPI="8"/>
    </package>
  </category>
</eixdump>"#;
        let dump: EixDump = quick_xml::de::from_str(xml).unwrap();
        let versions: Vec<String> = dump
            .categories
            .into_iter()
            .flat_map(|cat| cat.packages)
            .flat_map(|pkg| pkg.versions)
            .map(|v| v.id)
            .collect();
        assert_eq!(versions, vec!["0.11.6-r2", "0.11.7", "0.12.3", "9999"]);
    }

    #[test]
    fn empty_eix_output_parses_as_an_empty_dump_not_an_error() {
        // `eix --xml` prints genuinely empty stdout (not `<eixdump/>`) for
        // a zero-match query — every zero-match search used to error out
        // right here instead of just finding nothing, which meant a typo
        // could never even reach the fuzzy-matching fallback below it.
        let dump = parse_eix_xml("").unwrap();
        assert!(dump.categories.is_empty());
    }

    #[test]
    fn whitespace_only_eix_output_also_parses_as_empty() {
        let dump = parse_eix_xml("   \n").unwrap();
        assert!(dump.categories.is_empty());
    }

    #[test]
    fn real_eix_xml_still_parses_normally() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<eixdump version="16">
  <category name="mail-client">
    <package name="thunderbird">
      <description>Thunderbird Mail Client</description>
    </package>
  </category>
</eixdump>"#;
        let dump = parse_eix_xml(xml).unwrap();
        assert_eq!(dump.categories.len(), 1);
        assert_eq!(dump.categories[0].packages[0].name, "thunderbird");
    }
}
