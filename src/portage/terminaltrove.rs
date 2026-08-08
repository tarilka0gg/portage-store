use crate::portage::flathub::html_to_text;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// A browser UA is required — Terminal Trove's edge returns 403 to a bare
/// `curl` request with no User-Agent at all, evidently as basic bot
/// filtering rather than a paywall: `robots.txt` explicitly allows crawling
/// (`Allow: /`, `ai-input=yes`), so this is satisfying a heuristic, not
/// working around a restriction.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// A curated CLI-tool listing from terminaltrove.com.
///
/// This exists specifically because AppStream and Flathub have essentially
/// no coverage of command-line tools — they cover *desktop* applications —
/// and reducing a project's own README to prose (see `github.rs`) is a
/// poor substitute for what a site built to showcase terminal tools
/// already has: a human-written summary and, unlike a repo's generic
/// social-preview card, an actual screenshot or demo of the tool running.
/// Tried before falling back further to GitHub for exactly that reason:
/// it's the better source whenever it has an entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalTroveEntry {
    pub description: String,
    /// A real demo image/GIF hosted on their CDN — not the placeholder
    /// image the page's own JSON-LD `image` field points at for tools
    /// that haven't had one uploaded, which is why this comes from the
    /// `og:image` meta tag instead.
    pub screenshot: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum CacheEntry {
    Found(TerminalTroveEntry),
    Missing,
}

fn cache_path(package_name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("portage-store/terminaltrove");
    std::fs::create_dir_all(&dir).ok()?;
    let safe: String = package_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    Some(dir.join(format!("{safe}.json")))
}

fn read_cache(package_name: &str) -> Option<CacheEntry> {
    serde_json::from_str(&std::fs::read_to_string(cache_path(package_name)?).ok()?).ok()
}

fn write_cache(package_name: &str, entry: &CacheEntry) {
    if let Some(path) = cache_path(package_name)
        && let Ok(text) = serde_json::to_string(entry) {
            let _ = std::fs::write(path, text);
        }
}

/// The page slug to try. Unlike Flathub or GitHub, Terminal Trove's
/// listing pages are addressed directly by tool name
/// (`terminaltrove.com/<name>/`) rather than found through a search
/// endpoint, so there is no query to construct — only the Gentoo packaging
/// suffix to strip, since upstream never named a project `foo-bin`.
fn slug(package_name: &str) -> Option<String> {
    let base = package_name.trim_end_matches("-bin").trim_end_matches("-git");
    (base.len() >= 2).then(|| base.to_string())
}

fn fetch_page(slug: &str) -> Option<String> {
    let output = Command::new("curl")
        .args([
            "--silent",
            "--fail", // turns a 404 into a plain error instead of an HTML page to parse
            "--show-error",
            "--location",
            "--max-time",
            "15",
            "-A",
            USER_AGENT,
        ])
        .arg(format!("https://terminaltrove.com/{slug}/"))
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Pulls the `description` out of the page's `SoftwareApplication` JSON-LD
/// block. A tiny hand-rolled extraction rather than a full JSON parse of
/// the block, because the block also nests unrelated `BreadcrumbList` data
/// and the description is the only field worth the trouble of getting out
/// correctly (it's HTML, so it needs proper JSON string-escape handling,
/// which regex can't do safely).
fn extract_description(html: &str) -> Option<String> {
    let script_start = html.find(r#"<script type="application/ld+json">"#)?;
    let json_start = html[script_start..].find('{')? + script_start;
    let json_end = html[json_start..].find("</script>")? + json_start;
    let json: serde_json::Value = serde_json::from_str(&html[json_start..json_end]).ok()?;

    let graph = json.get("@graph")?.as_array()?;
    let app = graph.iter().find(|node| node.get("@type").and_then(|t| t.as_str()) == Some("SoftwareApplication"))?;
    let description_html = app.get("description")?.as_str()?;
    let text = html_to_text(description_html);
    (!text.is_empty()).then_some(text)
}

fn extract_og_image(html: &str) -> Option<String> {
    let marker = r#"property="og:image" content=""#;
    let start = html.find(marker)? + marker.len();
    let end = html[start..].find('"')? + start;
    let url = &html[start..end];
    // Tools without a submitted screenshot don't fall back to the JSON-LD
    // `image` field's `/static/placeholder.png` here — the `og:image` meta
    // tag instead falls back to a *different* image, the site's own
    // generic branding card at `terminaltrove.com/og/og.png`. An actual
    // per-tool screenshot is always served from their media CDN, so that's
    // the only thing accepted: allowlisting the one host that's real
    // rather than blocklisting placeholder URLs one at a time.
    url.starts_with("https://cdn.terminaltrove.com/").then(|| url.to_string())
}

/// Looks the package up on Terminal Trove, caching whatever comes back
/// (including a miss — most packages, being libraries or GUI apps rather
/// than showcase-worthy terminal tools, won't be listed). Blocking — call
/// it off the GTK main thread.
pub fn lookup(package_name: &str) -> Option<TerminalTroveEntry> {
    if let Some(entry) = read_cache(package_name) {
        return match entry {
            CacheEntry::Found(entry) => Some(entry),
            CacheEntry::Missing => None,
        };
    }

    let entry = fetch(package_name);
    write_cache(
        package_name,
        &match &entry {
            Some(entry) => CacheEntry::Found(entry.clone()),
            None => CacheEntry::Missing,
        },
    );
    entry
}

fn fetch(package_name: &str) -> Option<TerminalTroveEntry> {
    let html = fetch_page(&slug(package_name)?)?;
    let description = extract_description(&html).unwrap_or_default();
    let screenshot = extract_og_image(&html).unwrap_or_default();
    (!description.is_empty() || !screenshot.is_empty())
        .then_some(TerminalTroveEntry { description, screenshot })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gentoo_packaging_suffixes_are_stripped_from_the_slug() {
        assert_eq!(slug("neovim-git"), Some("neovim".to_string()));
        assert_eq!(slug("zen-bin"), Some("zen".to_string()));
        assert_eq!(slug("htop"), Some("htop".to_string()));
    }

    #[test]
    fn single_character_names_are_too_ambiguous_to_guess() {
        assert_eq!(slug("a-bin"), None);
    }

    #[test]
    fn extracts_description_from_the_software_application_node() {
        // Trimmed down real response shape from terminaltrove.com/htop/.
        let html = r#"<html><head>
<script type="application/ld+json">{"@context":"https://schema.org","@graph":[{"@type":"SoftwareApplication","@id":"https://terminaltrove.com/htop/#software","name":"htop","description":"<p>htop is a widely used cross-platform interactive process viewer.</p>","url":"https://terminaltrove.com/htop/"},{"@type":"BreadcrumbList","itemListElement":[]}]}</script>
<meta property="og:image" content="https://cdn.terminaltrove.com/m/fac3e763.png">
</head></html>"#;
        assert_eq!(
            extract_description(html).as_deref(),
            Some("htop is a widely used cross-platform interactive process viewer.")
        );
    }

    #[test]
    fn only_the_media_cdn_counts_as_a_real_screenshot() {
        let real = r#"<meta property="og:image" content="https://cdn.terminaltrove.com/m/abc.png">"#;
        assert_eq!(extract_og_image(real).as_deref(), Some("https://cdn.terminaltrove.com/m/abc.png"));

        // Two different generic fallbacks exist in the wild: the JSON-LD
        // `image` field's placeholder, and this one — the site's own
        // branding card, served for tools with no screenshot uploaded
        // (observed on terminaltrove.com/yazi/).
        let generic_card = r#"<meta property="og:image" content="https://terminaltrove.com/og/og.png">"#;
        assert_eq!(extract_og_image(generic_card), None);

        let placeholder = r#"<meta property="og:image" content="https://terminaltrove.com/static/placeholder.png">"#;
        assert_eq!(extract_og_image(placeholder), None);
    }

    #[test]
    fn breadcrumb_nodes_in_the_graph_are_not_mistaken_for_the_app() {
        let html = r#"<script type="application/ld+json">{"@graph":[{"@type":"BreadcrumbList","description":"should not be picked"}]}</script>"#;
        assert_eq!(extract_description(html), None);
    }

    #[test]
    fn missing_ld_json_block_is_not_an_error() {
        assert_eq!(extract_description("<html><body>no data here</body></html>"), None);
        assert_eq!(extract_og_image("<html><body>no data here</body></html>"), None);
    }
}
