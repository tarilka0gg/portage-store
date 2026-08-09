use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// Package context beyond what the tree itself carries — a wiki page, if
/// one exists, and how many Bugzilla reports mention this exact atom.
/// Neither is guessed: the wiki URL comes straight back from MediaWiki's
/// own search API (so it's never a 404), and the Bugzilla figure is a
/// live count from its REST API, not an assumption that reports exist.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PackageContext {
    pub wiki_url: Option<String>,
    pub bugzilla_count: Option<usize>,
}

fn curl_json(url: &str) -> Option<serde_json::Value> {
    let output = Command::new("curl").args(["--silent", "--show-error", "--max-time", "10", url]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

/// MediaWiki's `opensearch` action, asked for the single best match —
/// returns `[query, [titles], [descriptions], [urls]]`; the URL at
/// index 3 is the real, canonical page, not something reconstructed from
/// the package name (which would risk linking a page that doesn't exist,
/// or missing one that exists under different capitalization).
fn fetch_wiki_url(display_name: &str) -> Option<String> {
    let query = urlencoding_lite(display_name);
    let url = format!("https://wiki.gentoo.org/api.php?action=opensearch&search={query}&limit=1&format=json");
    let json = curl_json(&url)?;
    json.get(3)?.as_array()?.first()?.as_str().map(String::from)
}

/// The browsable (not REST) search page for `atom` — always a valid page
/// regardless of how many reports it finds, so unlike `wiki_url` this
/// never needs an existence check before being shown.
pub fn bugzilla_search_url(atom: &str) -> String {
    format!("https://bugs.gentoo.org/buglist.cgi?quicksearch={}", urlencoding_lite(atom))
}

/// Gentoo's own Bugzilla `quicksearch` is atom-aware — searching for
/// `category/name` matches reports filed against that exact package,
/// not a fuzzy text search. Counts every report regardless of status
/// (not just open ones): a REST call scoped to just open statuses would
/// need several repeated `bug_status` params, and "how many reports
/// exist at all" is still a meaningful signal on its own.
fn fetch_bugzilla_count(atom: &str) -> Option<usize> {
    let query = urlencoding_lite(atom);
    let url = format!("https://bugs.gentoo.org/rest/bug?quicksearch={query}&include_fields=id");
    let json = curl_json(&url)?;
    json.get("bugs")?.as_array().map(|bugs| bugs.len())
}

/// Minimal percent-encoding for the handful of characters that actually
/// show up in a package name or atom (`/`, spaces) — not a general URL
/// encoder, since neither endpoint here is fed arbitrary user text.
fn urlencoding_lite(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '/' => "%2F".to_string(),
            ' ' => "%20".to_string(),
            c if c.is_ascii_alphanumeric() || "-_.".contains(c) => c.to_string(),
            c => format!("%{:02X}", c as u32),
        })
        .collect()
}

fn cache_path(atom: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("portage-store/gentoo-web");
    std::fs::create_dir_all(&dir).ok()?;
    let safe: String = atom.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    Some(dir.join(format!("{safe}.json")))
}

/// Looks up wiki + Bugzilla context for `atom` (`category/name`), caching
/// the result — both are network calls, and this is exactly the kind of
/// data that doesn't change between one visit to a detail page and the
/// next. Blocking; call off the GTK main thread.
pub fn lookup(atom: &str, display_name: &str) -> PackageContext {
    if let Some(path) = cache_path(atom)
        && let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(cached) = serde_json::from_str::<PackageContext>(&text)
    {
        return cached;
    }

    let context = PackageContext { wiki_url: fetch_wiki_url(display_name), bugzilla_count: fetch_bugzilla_count(atom) };

    if let Some(path) = cache_path(atom)
        && let Ok(text) = serde_json::to_string(&context)
    {
        let _ = std::fs::write(path, text);
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slashes_and_spaces_are_encoded() {
        assert_eq!(urlencoding_lite("media-gfx/gimp"), "media-gfx%2Fgimp");
        assert_eq!(urlencoding_lite("GNU Image"), "GNU%20Image");
    }

    #[test]
    fn plain_names_are_left_alone() {
        assert_eq!(urlencoding_lite("neovim"), "neovim");
    }
}
