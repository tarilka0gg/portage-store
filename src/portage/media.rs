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
    path.exists().then_some(path)
}

/// Downloads one image and returns its cached path, reusing the cached copy
/// when there is one. Blocking — call it off the GTK main thread.
///
/// Shells out to curl rather than pulling in an HTTP stack: this is the only
/// network access the app makes, and curl already handles the redirects and
/// TLS that upstream image hosts need.
pub fn fetch(url: &str) -> Option<PathBuf> {
    let path = cache_dir()?.join(cache_name(url));
    if path.exists() {
        return Some(path);
    }

    let status = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
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

    if status.success() && path.exists() {
        Some(path)
    } else {
        let _ = std::fs::remove_file(&path);
        None
    }
}
