use std::path::PathBuf;
use std::process::Command;

/// Remote artwork — screenshots and icons — is cached on disk after the
/// first fetch, so a package's detail page only ever downloads once.
fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    let dir = base.join("portage-store/media");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// How large the on-disk media cache is allowed to grow before eviction
/// kicks in — screenshots run a few hundred KiB each, so this comfortably
/// holds artwork for several hundred packages' worth of browsing without
/// ever needing to think about it, while still bounding a cache that
/// otherwise had no ceiling at all and would grow for as long as the app
/// kept getting used.
const MAX_CACHE_BYTES: u64 = 200 * 1024 * 1024;

/// Evicted down to this fraction of `MAX_CACHE_BYTES` rather than exactly
/// to the limit, so a run of several fetches in a row (a detail page with
/// multiple screenshots) doesn't re-trigger eviction on every single one.
const EVICT_TO_BYTES: u64 = MAX_CACHE_BYTES * 9 / 10;

/// Marks `path` as just-used by bumping its mtime to now — the recency
/// signal `evict_lru` sorts on. Called on every cache hit (not just every
/// fetch), so an old screenshot that's still being looked at regularly
/// stays ahead of the eviction line instead of aging out just because it
/// was *downloaded* a while ago.
fn touch(path: &std::path::Path) {
    let now = std::time::SystemTime::now();
    let _ = filetime_touch(path, now);
}

/// Sets both atime and mtime to `when` — std has no direct API for this, so
/// it goes through a zero-byte-preserving reopen-and-set. Best-effort: a
/// failure here just means this file's recency tracking is a little
/// stale, not a correctness problem for anything that reads the image
/// itself.
fn filetime_touch(path: &std::path::Path, when: std::time::SystemTime) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new().write(true).open(path)?;
    file.set_modified(when)
}

/// Evicts the least-recently-used files (oldest mtime first — see `touch`)
/// until the cache is back at or under `EVICT_TO_BYTES`. Best-effort and
/// silent: run after every fetch, so a transient failure to stat/remove one
/// file just means eviction is a little behind, not that caching breaks.
fn evict_lru(dir: &std::path::Path) {
    evict_lru_to_budget(dir, MAX_CACHE_BYTES, EVICT_TO_BYTES);
}

/// The actual eviction logic, parameterized on its byte budgets so tests
/// can exercise it against a handful of small files instead of needing to
/// write out `MAX_CACHE_BYTES` worth of real ones.
fn evict_lru_to_budget(dir: &std::path::Path, max_bytes: u64, evict_to_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime, u64)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((entry.path(), meta.modified().ok()?, meta.len()))
        })
        .collect();

    let total: u64 = files.iter().map(|(_, _, size)| size).sum();
    if total <= max_bytes {
        return;
    }

    files.sort_by_key(|(_, modified, _)| *modified);
    let mut remaining = total;
    for (path, _, size) in files {
        if remaining <= evict_to_bytes {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            remaining = remaining.saturating_sub(size);
        }
    }
}

fn cache_name(url: &str) -> String {
    let hash = url
        .bytes()
        .fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211));
    let extension = if url.ends_with(".jpg") || url.ends_with(".jpeg") {
        "jpg"
    } else {
        "png"
    };
    format!("{hash:016x}.{extension}")
}

/// The cached path for a URL if it has already been downloaded, without
/// touching the network.
pub fn cached(url: &str) -> Option<PathBuf> {
    let path = cache_dir()?.join(cache_name(url));
    if !path.exists() {
        return None;
    }
    touch(&path);
    Some(path)
}

/// Whether `path`'s first few bytes are a recognized image format's magic
/// number — PNG, JPEG, GIF, or WebP (a RIFF container tagged `WEBP`),
/// covering everything this app actually fetches (screenshots, social-
/// preview cards, app icons). `curl --fail` alone isn't enough: some hosts
/// return HTTP 200 with an HTML error page, a JSON error body, or a
/// zero-byte response for a broken/missing image, none of which `curl`
/// itself treats as a failure — this is what actually caught that in
/// practice (an empty screenshot panel with no visible image, traced back
/// to exactly this: a "successful" download that wasn't real image data).
fn looks_like_an_image(path: &std::path::Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    if bytes.len() < 12 {
        return false;
    }
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
        || (bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
}

/// Downloads one image and returns its cached path, reusing the cached copy
/// when there is one. Blocking — call it off the GTK main thread.
///
/// Shells out to curl rather than pulling in an HTTP stack: this is the only
/// network access the app makes, and curl already handles the redirects and
/// TLS that upstream image hosts need.
pub fn fetch(url: &str) -> Option<PathBuf> {
    let dir = cache_dir()?;
    let path = dir.join(cache_name(url));
    if path.exists() {
        touch(&path);
        return Some(path);
    }

    let status = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "20",
            "--max-filesize",
            "8000000",
            "--output",
        ])
        .arg(&path)
        .arg(url)
        .status()
        .ok()?;

    if status.success() && path.exists() && looks_like_an_image(&path) {
        evict_lru(&dir);
        Some(path)
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(bytes: &[u8]) -> std::path::PathBuf {
        let unique =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        let path = std::env::temp_dir().join(format!("portage-store-media-test-{unique}"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_real_png_looks_like_an_image() {
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&[0u8; 8]);
        let path = temp_file(&bytes);
        assert!(looks_like_an_image(&path));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_real_jpeg_looks_like_an_image() {
        let mut bytes = b"\xff\xd8\xff".to_vec();
        bytes.extend_from_slice(&[0u8; 12]);
        let path = temp_file(&bytes);
        assert!(looks_like_an_image(&path));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_webp_looks_like_an_image() {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0u8; 4]);
        bytes.extend_from_slice(b"WEBP");
        let path = temp_file(&bytes);
        assert!(looks_like_an_image(&path));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_rate_limit_error_page_saved_as_png_is_rejected() {
        // Exactly what was found cached on a real system, from a GitHub
        // API rate-limit response `curl` had (before `--fail`/this check)
        // happily saved to disk as if it were the requested image.
        let path = temp_file(b"Too many requests, please try again later.");
        assert!(!looks_like_an_image(&path));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn an_empty_file_is_rejected() {
        let path = temp_file(b"");
        assert!(!looks_like_an_image(&path));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_nonexistent_file_is_rejected() {
        assert!(!looks_like_an_image(std::path::Path::new("/nonexistent/portage-store-media-test")));
    }

    fn temp_dir() -> std::path::PathBuf {
        let unique =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("portage-store-media-evict-test-{unique}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes `name` with `size` bytes, `age_secs` in the past — older
    /// files get evicted first, so tests build a known oldest-to-newest
    /// order by giving each file a different age.
    fn aged_file(dir: &std::path::Path, name: &str, size: u64, age_secs: u64) {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; size as usize]).unwrap();
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[test]
    fn under_budget_evicts_nothing() {
        let dir = temp_dir();
        aged_file(&dir, "a.png", 10, 100);
        aged_file(&dir, "b.png", 10, 50);
        evict_lru_to_budget(&dir, 1000, 900);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn over_budget_evicts_the_oldest_files_first() {
        let dir = temp_dir();
        aged_file(&dir, "oldest.png", 40, 300);
        aged_file(&dir, "middle.png", 40, 200);
        aged_file(&dir, "newest.png", 40, 100);
        // Total is 120 bytes, over a 100-byte budget — evicting the oldest
        // single file (40 bytes) brings it to 80, under the 90-byte target.
        evict_lru_to_budget(&dir, 100, 90);
        let remaining: std::collections::HashSet<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(remaining, ["middle.png", "newest.png"].into_iter().map(String::from).collect());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn touch_moves_a_file_to_the_front_of_the_eviction_order() {
        let dir = temp_dir();
        aged_file(&dir, "old_but_touched.png", 40, 300);
        aged_file(&dir, "newer.png", 40, 200);
        touch(&dir.join("old_but_touched.png"));
        // Still over budget (80 > 50) — without the touch, "old_but_touched"
        // (originally the older file) would be evicted first; with it, its
        // mtime is now newest, so "newer.png" goes instead.
        evict_lru_to_budget(&dir, 50, 40);
        let remaining: std::collections::HashSet<String> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(remaining, ["old_but_touched.png"].into_iter().map(String::from).collect());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
