use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

const SEARCH_URL: &str = "https://flathub.org/api/v2/search";
const APPSTREAM_URL: &str = "https://flathub.org/api/v2/appstream";

/// Presentation data for an app, borrowed from Flathub's AppStream catalog.
///
/// Gentoo ships no catalog of its own: an ebuild has a one-line DESCRIPTION,
/// and icons or screenshots only exist once a package is installed and has
/// dropped its own files on disk. Flathub publishes exactly this data for
/// thousands of apps, so it fills in everything the tree can't.
///
/// It describes the *same upstream program*, not the same build — the
/// flatpak's version and packaging differ from the ebuild's, so this is only
/// used for artwork and prose, never for versions, sizes or dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlathubApp {
    pub app_id: String,
    pub name: String,
    pub summary: String,
    pub description: String,
    pub icon: Option<String>,
    pub screenshots: Vec<String>,
    // `#[serde(default)]` so a cache file written before these fields
    // existed still deserializes (as `None`) instead of erroring out and
    // forcing every previously-cached entry to look like a cache miss.
    #[serde(default)]
    pub project_license: Option<String>,
    #[serde(default)]
    pub developer_name: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
}

/// Cached lookups, including misses. A miss is by far the common case —
/// most of the tree is libraries that no app store has ever heard of — and
/// without recording it, every visit to those packages would re-query.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum CacheEntry {
    Found(FlathubApp),
    Missing,
}

fn cache_path(package_name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("portage-store/flathub");
    std::fs::create_dir_all(&dir).ok()?;
    let safe: String = package_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    Some(dir.join(format!("{safe}.json")))
}

fn read_cache(package_name: &str) -> Option<CacheEntry> {
    let path = cache_path(package_name)?;
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(package_name: &str, entry: &CacheEntry) {
    if let Some(path) = cache_path(package_name)
        && let Ok(text) = serde_json::to_string(entry) {
            let _ = std::fs::write(path, text);
        }
}

/// Reduces a name to comparable letters and digits, so `OBS Studio`,
/// `obs-studio` and `obs_studio` all match.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// The name to search Flathub for. Gentoo's packaging suffixes mean nothing
/// upstream — `zen-bin` is just Zen, `firefox-bin` is just Firefox.
fn search_term(package_name: &str) -> String {
    package_name
        .trim_end_matches("-bin")
        .trim_end_matches("-git")
        .to_string()
}

/// Queries to try, in order. Flathub's search treats a hyphenated name as
/// one token and finds nothing for it, so `telegram-desktop` has to be
/// retried as `telegram` before the official app shows up.
fn search_terms(package_name: &str) -> Vec<String> {
    let base = search_term(package_name);
    let mut terms = vec![base.clone()];
    if let Some((head, _)) = base.split_once('-') {
        terms.push(base.replace('-', " "));
        terms.push(head.to_string());
    }
    terms
}

/// Whether a search hit is confidently the same program.
///
/// Deliberately strict: Flathub's search is fuzzy and happily returns a
/// dozen loosely-related apps, so anything short of an exact match on the
/// app's name or on the last segment of its reverse-DNS id is rejected.
/// Showing the wrong app's screenshots would be worse than showing none.
fn is_confident_match(package_name: &str, hit_name: &str, app_id: &str) -> bool {
    let wanted = normalize(&search_term(package_name));
    if wanted.len() < 3 {
        return false;
    }
    let id_tail = app_id.rsplit('.').next().unwrap_or_default();
    if normalize(hit_name) == wanted || normalize(id_tail) == wanted {
        return true;
    }
    // Reverse-DNS ids often end on a generic segment — `org.telegram.desktop`
    // tails to "desktop", which matches nothing — so also accept when the id
    // as a whole ends with the package name. Held to four characters, since
    // a three-letter tail collides far too easily.
    wanted.len() >= 4 && normalize(app_id).ends_with(&wanted)
}

fn curl_json(args: &[&str]) -> Option<serde_json::Value> {
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--location", "--max-time", "15"])
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// Strips the HTML that Flathub returns descriptions in, keeping paragraph
/// breaks. A GTK label would otherwise render the tags literally.
pub fn html_to_text(html: &str) -> String {
    let mut text = String::new();
    let mut inside_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => inside_tag = true,
            '>' => {
                inside_tag = false;
                text.push('\n');
            }
            // HTML whitespace (including the source's own line-wrapping,
            // which is purely for readability of the markup and carries no
            // meaning) collapses to a single space — only a tag boundary
            // above is a real paragraph break. Without this, hand-wrapped
            // description HTML renders as one short "paragraph" per source
            // line instead of flowing text.
            '\n' | '\r' | '\t' if !inside_tag => text.push(' '),
            _ if !inside_tag => text.push(ch),
            _ => {}
        }
    }

    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        // Decoded to a plain space, not U+00A0: Terminal Trove writes
        // `<p>&nbsp;</p>` as a bare visual spacer between real paragraphs,
        // and a plain space collapses to nothing under the blank-line
        // filter below, which is what a spacer paragraph should do —
        // whereas a literal non-breaking space would survive as a
        // "paragraph" containing only invisible whitespace.
        .replace("&nbsp;", " ")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&hellip;", "…");

    // Collapse the runs of blank lines that stripped tags leave behind into
    // single paragraph breaks, and each paragraph's internal whitespace
    // (doubled up wherever a tag boundary met a source-formatting space)
    // down to single spaces.
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn parse_screenshots(appstream: &serde_json::Value) -> Vec<String> {
    let Some(shots) = appstream.get("screenshots").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    shots
        .iter()
        .filter_map(|shot| {
            let sizes = shot.get("sizes")?.as_array()?;
            // Widest available, so the carousel isn't showing a thumbnail.
            sizes
                .iter()
                .max_by_key(|s| {
                    s.get("width")
                        .and_then(|w| w.as_str())
                        .and_then(|w| w.parse::<u32>().ok())
                        .unwrap_or(0)
                })?
                .get("src")?
                .as_str()
                .map(String::from)
        })
        .collect()
}

/// Returns a previously cached lookup without touching the network.
///
/// Grids call this: a search can put hundreds of cards on screen at once,
/// and querying Flathub for each would be both slow and rude. Coverage
/// there grows as detail pages get visited.
pub fn cached(package_name: &str) -> Option<FlathubApp> {
    match read_cache(package_name)? {
        CacheEntry::Found(app) => Some(app),
        CacheEntry::Missing => None,
    }
}

/// Looks the package up on Flathub, caching whatever comes back. Blocking —
/// call it off the GTK main thread.
pub fn lookup(package_name: &str) -> Option<FlathubApp> {
    if let Some(entry) = read_cache(package_name) {
        return match entry {
            CacheEntry::Found(app) => Some(app),
            CacheEntry::Missing => None,
        };
    }

    let app = fetch(package_name);
    write_cache(
        package_name,
        &match &app {
            Some(app) => CacheEntry::Found(app.clone()),
            None => CacheEntry::Missing,
        },
    );
    app
}

/// Looks a Flatpak app up by its own exact `app_id` — used for a genuine
/// Flatpak search hit, which already carries one, rather than `lookup`'s
/// name-based fuzzy match (built for the opposite direction: enriching a
/// *Portage* package's page with Flathub's screenshots when there's no
/// app_id to go on at all, only a package name to guess from). Skips the
/// search step entirely: `{APPSTREAM_URL}/{app_id}` alone already carries
/// name/summary/description/icon/screenshots.
pub fn lookup_by_app_id(app_id: &str) -> Option<FlathubApp> {
    if let Some(entry) = read_cache(app_id) {
        return match entry {
            CacheEntry::Found(app) => Some(app),
            CacheEntry::Missing => None,
        };
    }
    let app = fetch_by_app_id(app_id);
    write_cache(
        app_id,
        &match &app {
            Some(app) => CacheEntry::Found(app.clone()),
            None => CacheEntry::Missing,
        },
    );
    app
}

/// License, developer, and homepage — the same per-app AppStream payload
/// both `fetch` and `fetch_by_app_id` already download for its
/// `screenshots` key carries all three, so reading them costs nothing
/// extra: no second request either caller doesn't already make.
fn appstream_extras(appstream: &serde_json::Value) -> (Option<String>, Option<String>, Option<String>) {
    let project_license = appstream.get("project_license").and_then(|v| v.as_str()).map(String::from);
    let developer_name = appstream.get("developer_name").and_then(|v| v.as_str()).map(String::from);
    let homepage = appstream.get("urls").and_then(|u| u.get("homepage")).and_then(|v| v.as_str()).map(String::from);
    (project_license, developer_name, homepage)
}

fn fetch_by_app_id(app_id: &str) -> Option<FlathubApp> {
    let appstream = curl_json(&[&format!("{APPSTREAM_URL}/{app_id}")])?;
    let (project_license, developer_name, homepage) = appstream_extras(&appstream);
    Some(FlathubApp {
        app_id: app_id.to_string(),
        name: appstream.get("name")?.as_str()?.to_string(),
        summary: appstream.get("summary").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
        description: html_to_text(appstream.get("description").and_then(|d| d.as_str()).unwrap_or_default()),
        icon: appstream.get("icon").and_then(|i| i.as_str()).map(String::from),
        screenshots: parse_screenshots(&appstream),
        project_license,
        developer_name,
        homepage,
    })
}

fn find_hit(package_name: &str, term: &str) -> Option<serde_json::Value> {
    let query = serde_json::json!({ "query": term }).to_string();
    let results = curl_json(&[
        "-X",
        "POST",
        SEARCH_URL,
        "-H",
        "Content-Type: application/json",
        "-d",
        &query,
    ])?;

    results.get("hits")?.as_array()?.iter().find_map(|hit| {
        let name = hit.get("name").and_then(|n| n.as_str()).unwrap_or_default();
        let app_id = hit.get("app_id").and_then(|a| a.as_str()).unwrap_or_default();
        is_confident_match(package_name, name, app_id).then(|| hit.clone())
    })
}

fn fetch(package_name: &str) -> Option<FlathubApp> {
    let hit = search_terms(package_name)
        .into_iter()
        .find_map(|term| find_hit(package_name, &term))?;

    let app_id = hit.get("app_id")?.as_str()?.to_string();
    let appstream = curl_json(&[&format!("{APPSTREAM_URL}/{app_id}")]);
    let screenshots = appstream.as_ref().map(parse_screenshots).unwrap_or_default();
    let (project_license, developer_name, homepage) =
        appstream.as_ref().map(appstream_extras).unwrap_or_default();

    Some(FlathubApp {
        name: hit.get("name")?.as_str()?.to_string(),
        summary: hit.get("summary").and_then(|s| s.as_str()).unwrap_or_default().to_string(),
        description: html_to_text(
            hit.get("description").and_then(|d| d.as_str()).unwrap_or_default(),
        ),
        icon: hit.get("icon").and_then(|i| i.as_str()).map(String::from),
        screenshots,
        app_id,
        project_license,
        developer_name,
        homepage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gentoo_suffixes_are_dropped_before_searching() {
        assert_eq!(search_term("zen-bin"), "zen");
        assert_eq!(search_term("firefox-bin"), "firefox");
        assert_eq!(search_term("obs-studio"), "obs-studio");
    }

    #[test]
    fn matches_on_name_or_app_id_tail() {
        assert!(is_confident_match("firefox", "Firefox", "org.mozilla.firefox"));
        assert!(is_confident_match("obs-studio", "OBS Studio", "com.obsproject.Studio"));
        // zen-bin -> "zen": the display name differs, the id tail carries it.
        assert!(is_confident_match("zen-bin", "Zen Browser", "app.zen_browser.zen"));
    }

    #[test]
    fn hyphenated_names_are_retried_as_separate_terms() {
        // Flathub finds nothing for "telegram-desktop" as one token.
        assert_eq!(
            search_terms("telegram-desktop"),
            vec!["telegram-desktop", "telegram desktop", "telegram"]
        );
        assert_eq!(search_terms("firefox"), vec!["firefox"]);
    }

    #[test]
    fn reverse_dns_ids_match_on_the_whole_id() {
        // Tail is the generic "desktop"; the full id carries the name.
        assert!(is_confident_match("telegram-desktop", "Telegram", "org.telegram.desktop"));
        assert!(is_confident_match("gnome-software", "Software", "org.gnome.Software"));
    }

    #[test]
    fn loosely_related_hits_are_rejected() {
        assert!(!is_confident_match("firefox", "Floorp", "one.ablaze.floorp"));
        assert!(!is_confident_match("firefox", "Mozilla VPN", "org.mozilla.vpn"));
        assert!(!is_confident_match("firefox", "Add Water", "dev.qwery.AddWater"));
    }

    #[test]
    fn very_short_names_never_match() {
        // "go" or "cc" would otherwise collide with half of Flathub.
        assert!(!is_confident_match("go", "Go", "org.example.go"));
    }

    #[test]
    fn html_descriptions_become_paragraphs() {
        let html = "<p>First &amp; foremost.</p><p>Second line.</p>";
        assert_eq!(html_to_text(html), "First & foremost.\n\nSecond line.");
    }

    #[test]
    fn nbsp_spacer_paragraphs_are_dropped_not_shown_literally() {
        // Terminal Trove's own description markup for yazi, verbatim.
        let html = "<p>One.</p><p>&nbsp;</p><p>Two.</p>";
        let text = html_to_text(html);
        assert_eq!(text, "One.\n\nTwo.");
        assert!(!text.contains("nbsp"));
    }

    #[test]
    fn widest_screenshot_size_is_chosen() {
        let json = serde_json::json!({
            "screenshots": [{
                "sizes": [
                    {"src": "small.png", "width": "624"},
                    {"src": "large.png", "width": "1248"}
                ]
            }]
        });
        assert_eq!(parse_screenshots(&json), vec!["large.png"]);
    }
}
