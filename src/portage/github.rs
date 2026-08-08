use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

const SEARCH_URL: &str = "https://api.github.com/search/repositories";
const USER_AGENT: &str = "portage-store (https://github.com/, best-effort CLI enrichment)";

/// Fallback presentation data for command-line tools, which AppStream and
/// Flathub both have essentially no coverage of — a terminal utility has no
/// desktop entry and nothing to submit to a Linux app store. GitHub is
/// where most of them actually live, and its API exposes exactly the
/// pieces a one-line DESCRIPTION can't: the README's own explanation,
/// topics, star count, and a rendered social-preview card usable as a
/// stand-in illustration.
///
/// Like the Flathub data this sits next to, it describes the upstream
/// *project*, not this exact ebuild — used for prose and artwork only,
/// never for versions or sizes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubRepo {
    pub full_name: String,
    pub description: String,
    pub stars: u64,
    pub topics: Vec<String>,
    /// First couple of real prose paragraphs pulled out of the README,
    /// with badges, headings and markup stripped.
    pub readme_summary: String,
    /// The README's prose in full — headings and short instructional
    /// lines kept, unlike `readme_summary`, since a "Learn More" reader
    /// asking for everything there is to know wants a "Usage" heading
    /// followed by its bullet points, not just enough text to decide
    /// whether to install. `#[serde(default)]` so cache entries written
    /// before this field existed still deserialize (as empty rather than
    /// failing the whole cache read).
    #[serde(default)]
    pub readme_full: String,
    /// Actual screenshots the README embeds — the same demo images a
    /// visitor to the repo's own page would see — with CI/coverage/release
    /// badges and other repos' artwork filtered out. Empty when the README
    /// had no usable images of its own.
    pub readme_screenshots: Vec<String>,
    /// The repo's social-preview image — always present for a public
    /// repo, but not necessarily a screenshot: it's whatever GitHub
    /// auto-generates (a stats card) *or* whatever the maintainer chose to
    /// upload in their repo settings, which can be anything at all and is
    /// not distinguishable from the API alone. Kept separate from
    /// `readme_screenshots` rather than merged in as a fallback, so the
    /// caller can decide — via a user preference, since this is the one
    /// piece of GitHub artwork whose reliability can't be judged here —
    /// whether showing it is worth the risk of it being unrelated.
    pub fallback_card: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum CacheEntry {
    Found(GithubRepo),
    Missing,
}

fn cache_path(package_name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("portage-store/github");
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

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Query terms to try, in order. Same reasoning as Flathub's: Gentoo's
/// `-bin`/`-git` suffixes and hyphenated multi-word names mean nothing to
/// an upstream search index.
fn search_terms(package_name: &str) -> Vec<String> {
    let base = package_name.trim_end_matches("-bin").trim_end_matches("-git");
    let mut terms = vec![base.to_string()];
    if let Some((head, _)) = base.split_once('-') {
        terms.push(base.replace('-', " "));
        terms.push(head.to_string());
    }
    terms
}

/// Whether a search hit is confidently the right project.
///
/// Held to the repo's own name (the part after the last `/`) matching
/// exactly: convention on GitHub is that a project's repo is named after
/// the project, and unlike Flathub's reverse-DNS ids there is no
/// meaningful "tail segment" to fall back on — `owner/repo` only has the
/// one segment that's ever the tool's name.
fn is_confident_match(package_name: &str, full_name: &str) -> bool {
    let wanted = normalize(package_name.trim_end_matches("-bin").trim_end_matches("-git"));
    if wanted.len() < 3 {
        return false;
    }
    let repo = full_name.rsplit('/').next().unwrap_or_default();
    normalize(repo) == wanted
}

fn curl(args: &[&str]) -> Option<String> {
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--location", "--max-time", "15", "-H"])
        .arg(format!("User-Agent: {USER_AGENT}"))
        .args(args)
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn curl_json(args: &[&str]) -> Option<serde_json::Value> {
    serde_json::from_str(&curl(args)?).ok()
}

/// Strips Markdown down to the prose a paragraph body wants: no headings,
/// badge/image walls, code fences, or link syntax — just the sentences.
///
/// READMEs open with a badge row and a title almost universally; skipping
/// any line that is mostly punctuation/markup rather than words is what
/// keeps those out without a full Markdown parser.
fn readme_to_summary(markdown: &str) -> String {
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    let mut in_code_fence = false;

    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }
        if trimmed.is_empty() {
            if !current.is_empty() {
                paragraphs.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(trimmed);
    }
    if !current.is_empty() {
        paragraphs.push(current);
    }

    paragraphs
        .into_iter()
        // Rejected on the *raw* paragraph, before Markdown stripping: a raw
        // HTML block — the centred sponsor/badge tables READMEs routinely
        // open with (`<div align="center"><sup>...</sup>...</div>`) — has
        // none of the Markdown link/image punctuation `is_prose` screens
        // for, so it would otherwise sail through as if it were a written
        // sentence, tags and all.
        .filter(|p| !contains_html_tag(p))
        .map(|p| strip_markdown(&p))
        .filter(|p| is_prose(p))
        .take(2)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Heading text (lowercased substring match) that marks a README section as
/// project-housekeeping rather than documentation about the software
/// itself — Contributing guidelines, license blurbs, sponsor/funding
/// call-outs, changelogs, build-from-source instructions, and the like.
/// `readme_to_full` drops every paragraph under one of these until the next
/// heading, since a "Learn More" reader wants to know what the program does
/// and how to use it, not how to set up a dev environment for it or which
/// company sponsors the maintainer.
const BOILERPLATE_HEADING_MARKERS: &[&str] = &[
    "contribut",
    "license",
    "licence",
    "sponsor",
    "funding",
    "donate",
    "backer",
    "code of conduct",
    "security",
    "acknowledg",
    "credit",
    "changelog",
    "release note",
    "history",
    "citation",
    "star history",
    "roadmap",
    "maintainer",
    "table of contents",
    "install",
    "uninstall",
    "packaging",
    "build",
    "prerequisite",
    "requirement",
    "dependenc",
    "development",
    "testing",
    "support",
];

fn is_boilerplate_heading(heading: &str) -> bool {
    let lower = heading.to_lowercase();
    BOILERPLATE_HEADING_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// As `readme_to_summary`, but not capped to the opening couple of
/// paragraphs and without the "must read like a full sentence" length
/// floor `is_prose` applies — a short heading ("Usage", "Options") or a
/// terse bullet point is exactly what's wanted here, not noise to filter
/// out the way it is in a one-line summary. Unlike `readme_to_summary`,
/// this is heading-aware: an ATX heading (`#`.. `######`) opens a new
/// section, and everything under one matching `BOILERPLATE_HEADING_MARKERS`
/// is dropped wholesale until the next heading, rather than judged
/// paragraph by paragraph — a Contributing section's own prose reads as
/// perfectly good writing on its own, so nothing short of knowing which
/// heading it's under would catch it.
fn readme_to_full(markdown: &str) -> String {
    let mut out = String::new();
    let mut current = String::new();
    let mut in_code_fence = false;
    let mut skip_section = false;

    let flush = |current: &mut String, out: &mut String, skip: bool| {
        if current.is_empty() {
            return;
        }
        let raw = std::mem::take(current);
        if !skip && !contains_html_tag(&raw) {
            let cleaned = strip_markdown(&raw);
            let cleaned = cleaned.trim();
            // A badge/link row strips down to almost nothing once its
            // markup is gone — that emptiness is what marks it as noise
            // here, since `readme_to_summary`'s length floor would also
            // reject genuine short headings like "Usage".
            if cleaned.chars().count() >= 3 {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(cleaned);
            }
        }
    };

    'lines: for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }

        // An ATX heading (`## Usage`) starts a new section — flush
        // whatever the previous one collected, decide whether this one is
        // boilerplate, then let the heading text itself become the first
        // "paragraph" of the new section (still subject to the same
        // skip/strip treatment) so it reads as a section title above its
        // own content.
        if let Some(rest) = trimmed.strip_prefix('#') {
            let hashes = 1 + rest.chars().take_while(|c| *c == '#').count();
            if hashes <= 6 {
                flush(&mut current, &mut out, skip_section);
                let heading_text = trimmed.trim_start_matches('#').trim();
                skip_section = is_boilerplate_heading(heading_text);
                if !skip_section && !heading_text.is_empty() {
                    current.push_str(heading_text);
                    flush(&mut current, &mut out, false);
                }
                continue;
            }
        }

        if trimmed.is_empty() {
            flush(&mut current, &mut out, skip_section);
            continue;
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(trimmed);

        // A hard cap, not a paragraph count: some READMEs are one giant
        // wall of prose with barely any blank lines, which a paragraph
        // limit alone wouldn't catch.
        if out.chars().count() > 6000 {
            break 'lines;
        }
    }
    flush(&mut current, &mut out, skip_section);
    out
}

/// Whether a paragraph contains what looks like an HTML opening tag
/// (`<div`, `<a href=...>`, `<img ...>`) rather than merely a stray `<`
/// (as in "value < threshold", which real prose does occasionally use).
fn contains_html_tag(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.windows(2).any(|w| w[0] == '<' && (w[1].is_ascii_alphabetic() || w[1] == '/'))
}

/// Hosts and path fragments that mean "this image is a badge, not a
/// screenshot" — CI status, coverage, downloads, license, version. READMEs
/// draw these from a handful of well-known services, so an allowlist of
/// what a *real* screenshot looks like would be far less reliable than
/// blocklisting the badge generators themselves.
const BADGE_MARKERS: &[&str] = &[
    "shields.io",
    "badge.fury.io",
    "badgen.net",
    "coveralls.io",
    "codecov.io",
    "travis-ci.",
    "circleci.com",
    "sonarcloud.io",
    "codefactor.io",
    "deepsource.io",
    "snyk.io",
    "opencollective.com",
    "goreportcard.com",
    "pepy.tech",
    "img.shields",
    "visitor-badge",
    "hits.seeyoufarm.com",
    "wakatime.com/badge",
    "/badge/",
    "/badges/",
    "/workflows/",
    "actions/workflows/",
];

fn is_probably_badge(url: &str) -> bool {
    let lower = url.to_lowercase();
    // Badge generators render SVG almost without exception; an actual
    // screenshot or demo is raster (PNG/GIF/JPEG/WebP) essentially always.
    lower.ends_with(".svg") || BADGE_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// Resolves a README-relative image path (`docs/images/screenshot.png`,
/// the way `htop`'s README references its own screenshot) against the
/// repo, using `raw.githubusercontent.com/<repo>/HEAD/<path>` — `HEAD`
/// being GitHub's own alias for "whatever the default branch is", so this
/// needs no separate lookup of what that branch is called.
///
/// A `github.com/{owner}/{repo}/blob/...` link — the *viewer page* for a
/// file, not its bytes — is rewritten to the matching `raw.githubusercontent.com`
/// URL when it points at this same repo. `blob` URLs otherwise pass
/// straight through the badge/ownership filters unrecognised and, since
/// fetching one downloads an HTML page rather than image data, render as
/// nothing at all — which is silent enough to look like a formatting bug
/// rather than the wrong-URL bug it actually is.
fn resolve_readme_image(url: &str, full_name: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://github.com/")
        && let Some(path) = rest.strip_prefix(&format!("{full_name}/blob/")) {
            return format!("https://raw.githubusercontent.com/{full_name}/{path}");
        }
    if url.starts_with("http://") || url.starts_with("https://") {
        return url.to_string();
    }
    let path = url.trim_start_matches("./").trim_start_matches('/');
    format!("https://raw.githubusercontent.com/{full_name}/HEAD/{path}")
}

/// Whether a (post-`resolve_readme_image`) URL is plausibly *this* repo's
/// own artwork rather than someone else's — a sponsor logo, an unrelated
/// project's badge, a tracking pixel. READMEs routinely embed images that
/// have nothing to do with the tool itself (yazi's, for instance, opens
/// with a sponsor banner hosted under a completely different repo), and a
/// URL merely failing the badge-host blocklist doesn't mean it's a
/// screenshot — only that it isn't a *recognised* badge generator.
///
/// Held to: this repo's own raw content, this repo's GitHub-hosted
/// attachments (the CDN behind pasting an image into an issue/PR/README
/// via the web UI), or GitHub's generic user-upload CDN. Everything else —
/// including plain `github.com/{other-owner}/...` links — is rejected.
fn is_own_repo_image(url: &str, full_name: &str) -> bool {
    url.starts_with(&format!("https://raw.githubusercontent.com/{full_name}/"))
        || url.starts_with(&format!("https://github.com/{full_name}/assets/"))
        || url.starts_with("https://user-images.githubusercontent.com/")
        || url.starts_with("https://private-user-images.githubusercontent.com/")
        || url.starts_with("https://github.com/user-attachments/")
}

/// Pulls every image URL out of the README, in a single left-to-right
/// pass so results come back in actual document order — a Markdown
/// `![alt](url)` and an HTML `<img src="...">` are two different syntaxes
/// for the same thing, and READMEs mix both (the HTML form shows up
/// specifically where the author wanted centring/sizing Markdown can't
/// express, which is exactly where a deliberately-showcased demo image
/// tends to live), so scanning them separately and concatenating the
/// results would reorder images relative to how the README reads.
fn markdown_image_urls(markdown: &str) -> Vec<String> {
    let chars: Vec<char> = markdown.chars().collect();
    let mut urls = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            let (_, after_bracket) = read_bracketed(&chars, i + 1);
            if chars.get(after_bracket) == Some(&'(') {
                let paren_start = after_bracket + 1;
                let mut j = paren_start;
                while j < chars.len() && chars[j] != ')' {
                    j += 1;
                }
                let inside: String = chars[paren_start..j].iter().collect();
                // A Markdown image target can carry a trailing title —
                // `(url "caption")` — that isn't part of the URL.
                if let Some(url) = inside.split_whitespace().next() {
                    urls.push(url.to_string());
                }
                i = (j + 1).min(chars.len());
                continue;
            }
            i = after_bracket;
            continue;
        }
        if chars[i] == '<' && chars[i..].iter().take(4).collect::<String>() == "<img" {
            let tag_end = chars[i..].iter().position(|&c| c == '>').map(|p| i + p);
            if let Some(tag_end) = tag_end {
                let tag: String = chars[i..tag_end].iter().collect();
                if let Some(src) = extract_attr(&tag, "src") {
                    urls.push(src);
                }
                i = tag_end + 1;
                continue;
            }
        }
        i += 1;
    }
    urls
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    for quote in ['"', '\''] {
        let marker = format!("{name}={quote}");
        if let Some(i) = tag.find(&marker) {
            let start = i + marker.len();
            let end = tag[start..].find(quote)? + start;
            return Some(tag[start..end].to_string());
        }
    }
    None
}

/// The README's own screenshots, badges filtered out and relative paths
/// resolved to fetchable URLs, in document order with duplicates removed.
fn readme_screenshots(markdown: &str, full_name: &str) -> Vec<String> {
    let mut urls = markdown_image_urls(markdown);
    urls.retain(|url| !is_probably_badge(url));
    let mut urls: Vec<String> =
        urls.into_iter().map(|url| resolve_readme_image(&url, full_name)).collect();
    urls.retain(|url| is_own_repo_image(url, full_name));
    urls.dedup();
    urls.truncate(6);
    urls
}

/// A line is worth showing if it isn't dense with link/image punctuation —
/// a badge row like `[![CI](url)](url) [![Downloads](url)](url)` or a link
/// list like `[Docs](x) [CI](y) [License](z)` is mostly `[`, `]`, `(`, `)`
/// even though the visible words look sentence-like; real prose barely
/// touches those characters.
fn is_prose(text: &str) -> bool {
    let len = text.chars().count();
    if len < 30 {
        return false;
    }
    let markup = text.chars().filter(|c| matches!(c, '[' | ']' | '(' | ')' | '!')).count();
    markup * 6 < len
}

/// Reads a `[...]` span starting at `chars[start]`, tracking bracket depth
/// so a nested pair — the badge pattern `[![alt](img)](url)` nests an
/// image inside a link's label — closes on its *matching* `]`, not the
/// first one. Returns the inner content and the index just past it.
fn read_bracketed(chars: &[char], start: usize) -> (String, usize) {
    let mut depth = 0i32;
    let mut i = start;
    let mut content = String::new();
    while i < chars.len() {
        match chars[i] {
            '[' => {
                depth += 1;
                if depth > 1 {
                    content.push('[');
                }
            }
            ']' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    break;
                }
                content.push(']');
                continue;
            }
            c => content.push(c),
        }
        i += 1;
    }
    (content, i)
}

/// Skips a `(...)` span (a link/image URL) if one starts at `chars[start]`.
/// Not depth-aware — URLs don't nest parentheses in Markdown syntax — so
/// this stops at the first `)`.
fn skip_optional_paren(chars: &[char], start: usize) -> usize {
    if chars.get(start) != Some(&'(') {
        return start;
    }
    let mut i = start + 1;
    while i < chars.len() && chars[i] != ')' {
        i += 1;
    }
    if i < chars.len() {
        i + 1
    } else {
        i
    }
}

/// Removes the Markdown syntax that would otherwise render literally in a
/// plain `GtkLabel`: heading hashes, emphasis markers, and image/link
/// syntax. Links keep their visible text; images — and links whose entire
/// label is itself an image, the `[![alt](img)](url)` badge pattern —
/// are dropped whole, since alt text on a badge is never prose worth
/// keeping.
fn strip_markdown(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '!' if chars.get(i + 1) == Some(&'[') => {
                let (_, after_bracket) = read_bracketed(&chars, i + 1);
                i = skip_optional_paren(&chars, after_bracket);
            }
            '[' => {
                let (label, after_bracket) = read_bracketed(&chars, i);
                let has_url = chars.get(after_bracket) == Some(&'(');
                let after = if has_url { skip_optional_paren(&chars, after_bracket) } else { after_bracket };
                if label.trim_start().starts_with('!') {
                    // Badge disguised as a link.
                } else if has_url {
                    out.push_str(label.trim());
                } else {
                    out.push('[');
                    out.push_str(&label);
                }
                i = after;
            }
            '#' | '`' | '*' | '_' => i += 1,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Looks the package up on GitHub, caching whatever comes back (including
/// a miss). Blocking — call it off the GTK main thread.
///
/// GitHub's unauthenticated search API allows 60 requests/hour, which the
/// on-disk cache is what keeps this workable: each package is looked up at
/// most once, ever, from this machine.
pub fn lookup(package_name: &str) -> Option<GithubRepo> {
    if let Some(entry) = read_cache(package_name) {
        return match entry {
            CacheEntry::Found(repo) => Some(repo),
            CacheEntry::Missing => None,
        };
    }
    let repo = fetch(package_name);
    write_cache(
        package_name,
        &match &repo {
            Some(repo) => CacheEntry::Found(repo.clone()),
            None => CacheEntry::Missing,
        },
    );
    repo
}

/// Looks up a repo we already know the exact `owner/repo` of — read from
/// the ebuild's own `metadata.xml` — skipping the fuzzy name search
/// entirely. Cached the same way as `lookup`, but keyed by the repo's full
/// name rather than the package name, since the same repo backs a single
/// upstream project regardless of which distro's package happens to point
/// at it.
pub fn lookup_known(full_name: &str) -> Option<GithubRepo> {
    let cache_key = format!("_known_{}", full_name.replace('/', "_"));
    if let Some(entry) = read_cache(&cache_key) {
        return match entry {
            CacheEntry::Found(repo) => Some(repo),
            CacheEntry::Missing => None,
        };
    }
    let item = curl_json(&[&format!("https://api.github.com/repos/{full_name}")]);
    let repo = item.and_then(|item| build_repo(&item));
    write_cache(
        &cache_key,
        &match &repo {
            Some(repo) => CacheEntry::Found(repo.clone()),
            None => CacheEntry::Missing,
        },
    );
    repo
}

fn find_repo(package_name: &str, term: &str) -> Option<serde_json::Value> {
    let query = format!("{term} in:name");
    let results = curl_json(&[&format!(
        "{SEARCH_URL}?q={}&sort=stars&order=desc&per_page=10",
        urlencode(&query)
    )])?;
    results.get("items")?.as_array()?.iter().find_map(|item| {
        let full_name = item.get("full_name")?.as_str()?;
        is_confident_match(package_name, full_name).then(|| item.clone())
    })
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

fn fetch(package_name: &str) -> Option<GithubRepo> {
    let item = search_terms(package_name).into_iter().find_map(|t| find_repo(package_name, &t))?;
    build_repo(&item)
}

/// Assembles a `GithubRepo` from a GitHub repo API response — shared by
/// the search-based lookup and the metadata.xml-authoritative one, since
/// both `/search/repositories` items and `/repos/{full_name}` responses
/// carry the same fields.
fn build_repo(item: &serde_json::Value) -> Option<GithubRepo> {
    let full_name = item.get("full_name")?.as_str()?.to_string();
    let description = item.get("description").and_then(|d| d.as_str()).unwrap_or_default().to_string();
    let stars = item.get("stargazers_count").and_then(|s| s.as_u64()).unwrap_or(0);
    let topics = item
        .get("topics")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let readme = curl(&[
        &format!("https://api.github.com/repos/{full_name}/readme"),
        "-H",
        "Accept: application/vnd.github.raw",
    ]);
    let readme_summary = readme.as_deref().map(readme_to_summary).unwrap_or_default();
    let readme_full = readme.as_deref().map(readme_to_full).unwrap_or_default();
    let readme_screenshots = readme.as_deref().map(|md| readme_screenshots(md, &full_name)).unwrap_or_default();
    let fallback_card = format!("https://opengraph.githubassets.com/1/{full_name}");

    Some(GithubRepo {
        full_name,
        description,
        stars,
        topics,
        readme_summary,
        readme_full,
        readme_screenshots,
        fallback_card,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boilerplate_sections_are_dropped_but_usage_content_survives() {
        let readme = "\
# mytool

[![CI](https://img.shields.io/badge/build-passing-green)](https://ci.example)

mytool is a small utility that does exactly one thing well.

## Usage

Run `mytool --help` to see every available flag.

- Press `q` to quit
- Press `r` to refresh

## Installation

### Prerequisites

You'll need a C99 compiler and autotools installed first.

```
./configure && make && make install
```

## Contributing

We welcome pull requests! Please read our contributor guide before opening one.

## Sponsors

Special thanks to our sponsors for funding this project's development.

## License

Released under the MIT license, see LICENSE for details.
";
        let full = readme_to_full(readme);
        assert!(full.contains("mytool is a small utility"));
        assert!(full.contains("Usage"));
        assert!(full.contains("mytool --help"));
        assert!(full.contains("Press q to quit"));
        for junk in ["Prerequisites", "autotools", "Contributing", "pull requests", "Sponsors", "funding", "License", "MIT license"] {
            assert!(!full.contains(junk), "full text still contains boilerplate {junk:?}: {full:?}");
        }
    }

    #[test]
    fn matches_on_the_repo_name_segment_only() {
        assert!(is_confident_match("htop", "htop-dev/htop"));
        assert!(is_confident_match("fastfetch", "fastfetch-cli/fastfetch"));
        assert!(!is_confident_match("htop", "htop-dev/htop-classic"));
    }

    #[test]
    fn suffixes_are_stripped_before_matching() {
        assert!(is_confident_match("neovim-git", "neovim/neovim"));
    }

    #[test]
    fn hyphenated_terms_get_a_fallback_query() {
        assert_eq!(search_terms("ripgrep-all"), vec!["ripgrep-all", "ripgrep all", "ripgrep"]);
    }

    #[test]
    fn readme_badges_and_headings_are_skipped() {
        let readme = "\
# htop

[![Build](https://img.shields.io/badge/build-passing-green)](https://ci.example/build)
[![Downloads](https://img.shields.io/badge/downloads-1M-blue)](https://example.com)

htop is an interactive process viewer for Unix systems. It aims to be a \
better alternative to the ubiquitous top program.

## Installation

See the wiki for platform-specific instructions.
";
        let summary = readme_to_summary(readme);
        assert!(summary.starts_with("htop is an interactive process viewer"));
        assert!(!summary.contains("Build"));
        assert!(!summary.contains("Installation"));
    }

    #[test]
    fn markdown_links_keep_their_text_and_drop_the_url() {
        assert_eq!(
            strip_markdown("See the [official docs](https://example.com/docs) for more."),
            "See the official docs for more."
        );
    }

    #[test]
    fn html_blocks_are_not_mistaken_for_prose() {
        // The exact shape of yazi's actual README opener: a centred
        // sponsor table, one giant HTML paragraph with no Markdown link
        // punctuation for `is_prose` to catch.
        let md = "\
<div align=\"center\"> <sup>Special thanks to:</sup><br>

| <a href=\"https://go.warp.dev/yazi\"><img src=\"https://x.example/warp.png\"></a> |
|---|

</div>

Yazi is a terminal file manager with a TUI written in Rust.
";
        assert_eq!(readme_to_summary(md), "Yazi is a terminal file manager with a TUI written in Rust.");
    }

    #[test]
    fn a_stray_angle_bracket_does_not_trigger_the_html_filter() {
        assert!(!contains_html_tag("Runs when load < threshold, otherwise waits."));
    }

    #[test]
    fn sponsor_banners_from_other_repos_are_rejected() {
        // The real bug: yazi's README embeds a Warp sponsorship banner
        // hosted under an unrelated repo, as a `blob` viewer link (HTML
        // page, not image bytes) — both reasons to reject it on their own.
        let md = r#"<img src="https://github.com/warpdotdev/brand-assets/blob/main/logo.png">
![Real screenshot](https://raw.githubusercontent.com/sxyazi/yazi/main/screenshot.png)"#;
        assert_eq!(
            readme_screenshots(md, "sxyazi/yazi"),
            vec!["https://raw.githubusercontent.com/sxyazi/yazi/main/screenshot.png"]
        );
    }

    #[test]
    fn same_repo_blob_links_are_rewritten_to_raw_and_kept() {
        let md = r#"<img src="https://github.com/sxyazi/yazi/blob/main/docs/demo.gif">"#;
        assert_eq!(
            readme_screenshots(md, "sxyazi/yazi"),
            vec!["https://raw.githubusercontent.com/sxyazi/yazi/main/docs/demo.gif"]
        );
    }

    #[test]
    fn badge_hosts_and_svg_are_rejected() {
        assert!(is_probably_badge("https://img.shields.io/badge/build-passing-green"));
        assert!(is_probably_badge("https://github.com/htop-dev/htop/workflows/CI/badge.svg"));
        assert!(is_probably_badge("https://example.com/logo.svg"));
        assert!(!is_probably_badge("https://user-images.githubusercontent.com/1/demo.gif"));
    }

    #[test]
    fn relative_readme_paths_resolve_against_the_default_branch() {
        assert_eq!(
            resolve_readme_image("docs/images/screenshot.png?raw=true", "htop-dev/htop"),
            "https://raw.githubusercontent.com/htop-dev/htop/HEAD/docs/images/screenshot.png?raw=true"
        );
        assert_eq!(
            resolve_readme_image("https://example.com/shot.png", "owner/repo"),
            "https://example.com/shot.png"
        );
    }

    #[test]
    fn markdown_image_targets_are_extracted_in_order() {
        let md = "![One](https://a.example/1.png)\ntext\n![Two](https://a.example/2.png \"caption\")";
        assert_eq!(
            markdown_image_urls(md),
            vec!["https://a.example/1.png", "https://a.example/2.png"]
        );
    }

    #[test]
    fn html_img_tags_are_extracted() {
        let html = r#"<p align="center"><img src="https://a.example/demo.gif" width="600"></p>"#;
        assert_eq!(markdown_image_urls(html), vec!["https://a.example/demo.gif"]);
    }

    #[test]
    fn mixed_markdown_and_html_images_stay_in_document_order() {
        let md = r#"<img src="https://a.example/first.png"> then ![second](https://a.example/second.png)"#;
        assert_eq!(
            markdown_image_urls(md),
            vec!["https://a.example/first.png", "https://a.example/second.png"]
        );
    }

    #[test]
    fn readme_screenshots_drop_badges_and_keep_real_images() {
        let md = "\
[![CI](https://github.com/o/r/workflows/CI/badge.svg)](https://github.com/o/r/actions)

<p align=\"center\"><img src=\"https://user-images.githubusercontent.com/1/demo.gif\"></p>

![Screenshot](docs/screenshot.png?raw=true)
";
        let shots = readme_screenshots(md, "o/r");
        assert_eq!(
            shots,
            vec![
                "https://user-images.githubusercontent.com/1/demo.gif",
                "https://raw.githubusercontent.com/o/r/HEAD/docs/screenshot.png?raw=true",
            ]
        );
    }

    #[test]
    fn real_htop_readme_yields_clean_prose() {
        let readme: &str = "# [![htop logo](htop-logo.png)](https://htop.dev)\n\n[![CI](https://github.com/htop-dev/htop/workflows/CI/badge.svg)](https://github.com/htop-dev/htop/actions)\n[![Coverity Scan Build Status](https://scan.coverity.com/projects/21665/badge.svg)](https://scan.coverity.com/projects/21665)\n[![Mailing List](https://img.shields.io/badge/Mailing%20List-htop-blue.svg)](https://groups.io/g/htop)\n[![IRC #htop](https://img.shields.io/badge/IRC-htop-blue.svg)](https://web.libera.chat/#htop)\n[![GitHub Release](https://img.shields.io/github/release/htop-dev/htop.svg)](https://github.com/htop-dev/htop/releases/latest)\n[![Packaging status](https://repology.org/badge/tiny-repos/htop.svg)](https://repology.org/project/htop/versions)\n[![License: GPL v2+](https://img.shields.io/badge/License-GPL%20v2+-blue.svg)](COPYING?raw=true)\n\n![Screenshot of htop](docs/images/screenshot.png?raw=true)\n\n## Introduction\n\n`htop` is a cross-platform interactive process viewer.\n\n`htop` allows scrolling the list of processes vertically and horizontally to see their full command lines and related information like memory and CPU consumption.\nAlso system wide information, like load average or swap usage, is shown.\n\nThe information displayed is configurable through a graphical setup and can be sorted and filtered interactively.\n\nTasks related to processes (e.g. killing and renicing) can be done without entering their PIDs.\n\nRunning `htop` requires `ncurses` libraries, typically named libncurses(w).\n\n`htop` is written in C.\n\nFor more information and details visit [htop.dev](https://htop.dev).\n\n## Usage\nSee the manual page (`man htop`) or the help menu (`h` or `F1` inside `htop`) for a list of supported key commands.\n\n### Quick Start\n\nSome common actions to get you started with `htop`\n\n- Search processes: press `/`\n- Filter processes: press `\\`\n- Toggle tree view: press `t`\n- Change process sort column: press `.`\n- Kill a process: select the process and press `k`\n\n## Build instructions\n\n### Prerequisite\nList of build-time dependencies:\n  * standard GNU autotools-based C toolchain\n    - C99 compliant compiler\n    - `autoconf`\n    - `automake`\n    - `autotools`\n  * `ncurses`\n\n**Note about `ncurses`:**\n> `htop` requires `ncurses` 6.0. Be aware the appropriate package is sometimes still called libncurses5 (on Debian/Ubuntu). Also `ncurses` usually comes in two flavours:\n>* With Unicode support.\n>* Without Unicode support.\n>\n> This is also something that is reflected in the package name on Debian/Ubuntu (via the additional 'w' - 'w'ide character support).\n\nList of additional build-time dependencies (based on feature flags):\n*  `pkg-config`\n*  `sensors`\n*  `hwloc`\n*  `libcap` (v2.21 or later)\n*  `libnl-3` and `libnl-genl-3`\n\n`pkg-config` is optional but recommended. The configure script of `htop` might utilize `pkg-config` to obtain the compiler and linker flags required for a library. Some OS distributions provide `pkg-config` functionalities through an alternative implementation such as `pkgconf`. Look for both names in your package manager.\n\nInstall these and other required packages for C development from your package manager.\n\n**Debian/Ubuntu**\n~~~ shell\nsudo apt install libncursesw5-dev autotools-dev autoconf automake build-essential\n~~~\n\n**Fedora/RHEL**\n~~~ shell\nsudo dnf install ncurses-devel automake autoconf gcc\n~~~\n\n**OpenSUSE/SLES**\n~~~ shell\nsudo zypper install ncurses-devel ncurses-devel-static automake autoconf gcc make glibc-devel glibc-devel-static\n~~~\n\n**Archlinux/Manjaro**\n~~~ shell\nsudo pacman -S --needed base-devel ncurses\n~~~\n\n**macOS**\n~~~ shell\nbrew install ncurses automake autoconf gcc\n~~~\n\n### Compile from source:\nTo compile from source, download from the Git repository (`git clone` or downloads from [GitHub releases](https://github.com/htop-dev/htop/releases/)), then run:\n~~~ shell\n./autogen.sh && ./configure && make\n~~~\n\n### Install\nTo install on the local system run `make install`. By default `make install` installs into `/usr/local`. To change this path use `./configure --prefix=/some/path`.\n\n### Build Options\n\n`htop` has several build-time options to enable/disable additional features.\n\n#### Generic\n\n  * `--enable-unicode`:\n    enable Unicode support\n    - dependency: *libncursesw*\n    - default: *yes*\n  * `--enable-affinity`:\n    enable `sched_setaffinity(2)` and `sched_getaffinity(2)` for affinity support; conflicts with hwloc\n    - default: *check*\n  * `--enable-hwloc`:\n    enable hwloc support for CPU affinity; disables affinity support\n    - dependency: *libhwloc*\n    - default: *no*\n  * `--enable-backtrace`:\n    enable showing backtraces of a process\n    - default: *no*\n    - possible values:\n      - unwind-ptrace: use **libunwind-ptrace** to get backtraces\n  * `--enable-demangling`:\n    enable demangling support for backtraces\n    - default: *check*\n    - possible values:\n      - libiberty: use **libiberty** (GNU) to demangle function names\n      - libdemangle: use **libdemangle** (Solaris) to demangle function names\n  * `--enable-static`:\n    build a static htop binary; hwloc and delay accounting are not supported\n    - default: *no*\n  * `--enable-debug`:\n    Enable asserts and internal sanity checks; implies a performance penalty\n    - default: *no*\n\n#### Performance Co-Pilot\n\n  * `--enable-pcp`:\n    enable Performance Co-Pilot support via a new pcp-htop utility\n    - dependency: *libpcp*\n    - default: *no*\n\n#### Linux\n\n  * `--enable-sensors`:\n    enable libsensors(3) support for reading temperature data\n    - dependencies: *libsensors-dev*(build-time), at runtime *libsensors* is loaded via `dlopen(3)` if available\n    - default: *check*\n  * `--enable-capabilities`:\n    enable Linux capabilities support\n    - dependency: *libcap*\n    - default: *check*\n  * `--with-proc`:\n    location of a Linux-compatible proc filesystem\n    - default: */proc*\n  * `--enable-delayacct`:\n    enable Linux delay accounting support\n    - dependencies: *libnl-3-dev*(build-time) and *libnl-genl-3-dev*(build-time), at runtime *libnl-3* and *libnl-genl-3* are loaded via `dlopen(3)` if available and requested\n    - default: *check*\n\n\n## Runtime dependencies:\n`htop` has a set of fixed minimum runtime dependencies, which is kept as minimal as possible:\n* `ncurses` libraries for terminal handling (wide character support).\n\n### Runtime optional dependencies:\n`htop` has a set of fixed optional dependencies, depending on build/configure option used:\n\n#### Linux\n* `libdl`, if not building a static binary, is always required when support for optional dependencies (i.e. `libsensors`, `libsystemd`) is present.\n* `libcap`, user-space interfaces to POSIX 1003.1e capabilities, is always required when `--enable-capabilities` was used to configure `htop`.\n* `libsensors`, readout of temperatures and CPU speeds, is optional even when `--enable-sensors` was used to configure `htop`.\n* `libsystemd` is optional when `--enable-static` was not used to configure `htop`. If building statically and `libsystemd` is not found by `configure`, support for the systemd meter is disabled entirely.\n* `libnl-3` and `libnl-genl-3`, if `htop` was configured with `--enable-delayacct` and delay accounting process fields are active.\n* I/O counters are available when the kernel is compiled with `CONFIG_TASK_IO_ACCOUNTING=Y`.\n\n`htop` checks for the availability of the actual runtime libraries as `htop` runs.\n\n#### BSD\nOn most BSD systems `kvm` is a requirement to read kernel information.\n\nMore information on required and optional dependencies can be found in [configure.ac](configure.ac).\n\n## Support\n\nIf you have trouble running `htop` please consult your operating system / Linux distribution documentation for getting support and filing bugs.\n\n## Bugs, development feedback\n\nWe have a [development mailing list](https://htop.dev/mailinglist.html). Feel free to subscribe for release announcements or asking questions on the development of `htop`.\n\nYou can also join our IRC channel [#htop on Libera.Chat](https://web.libera.chat/#htop) and talk to the developers there.\n\nIf you have found an issue within the source of `htop`, please check whether this has already been reported in our [GitHub issue tracker](https://github.com/htop-dev/htop/issues).\nIf not, please file a new issue describing the problem you have found, the potential location in the source code you are referring to and a possible fix if available.\n\n## History\n\n`htop` was invented, developed and maintained by [Hisham Muhammad](https://hisham.hm/) from 2004 to 2019. His [legacy repository](https://github.com/hishamhm/htop/) has been archived to preserve the history.\n\nIn 2020 a [team](https://github.com/orgs/htop-dev/people) took over the development amicably and continues to maintain `htop` collaboratively.\n\n## License\n\nGNU General Public License, version 2 (GPL-2.0) or, at your option, any later version.\n";
        let summary = readme_to_summary(readme);
        assert!(
            summary.starts_with("htop is a cross-platform interactive process viewer"),
            "got: {summary:?}"
        );
        for junk in ["![", "](", "workflows/CI", "Coverity", "License"] {
            assert!(!summary.contains(junk), "summary still contains {junk:?}: {summary:?}");
        }
    }

    #[test]
    fn markdown_images_are_dropped_entirely() {
        assert_eq!(strip_markdown("![Screenshot](https://example.com/shot.png) Some text."), "Some text.");
    }

    #[test]
    fn short_or_link_heavy_lines_are_not_prose() {
        assert!(!is_prose("[Docs](x) [CI](y) [License](z)"));
        assert!(!is_prose("Too short"));
        assert!(is_prose("A genuinely descriptive sentence about what this tool actually does."));
    }
}
