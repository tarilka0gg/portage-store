use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Papirus ships ~4000 app icons under common/well-known names (GPL-3,
/// packaged for Gentoo as x11-themes/papirus-icon-theme) — unlike hicolor,
/// which only has icons for apps actually installed on this machine,
/// Papirus lets us show a real logo for popular packages even before
/// they're installed.
const ICON_THEME_DIRS: &[&str] = &[
    "/usr/share/icons/hicolor",
    "/usr/share/icons/Papirus",
    "/usr/share/icons/Adwaita",
];
/// Larger sizes first: we downscale for the 40px avatar, but starting from
/// something crisp beats upscaling a 16x16 icon.
const SIZE_DIRS: &[&str] = &[
    "128x128", "96x96", "84x84", "64x64", "256x256", "48x48", "42x42", "scalable",
    "512x512", "32x32", "24x24", "22x22", "18x18", "16x16", "8x8",
];

fn find_desktop_file(package_name: &str, contents: &str) -> Option<PathBuf> {
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("obj") {
            continue;
        }
        let Some(path) = parts.next() else { continue };
        if path.ends_with(".desktop") && path.contains("/applications/") {
            return Some(PathBuf::from(path));
        }
    }
    // CONTENTS parsing found nothing (headless package, or a desktop file
    // portage didn't record as `obj`): fall back to guessing by name, since
    // most desktop files are named after the package.
    let guess = PathBuf::from(format!("/usr/share/applications/{package_name}.desktop"));
    guess.exists().then_some(guess)
}

fn read_icon_name(desktop_file: &Path) -> Option<String> {
    let content = fs::read_to_string(desktop_file).ok()?;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("Icon=") {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// `find_icon_file`'s own result cache, keyed by `icon_name` — icon theme
/// contents don't change over the process's lifetime, but `find_icon_file`
/// runs once per candidate name for *every* package card built (search
/// results alone put up to 300 on screen at once, per `show_results`), each
/// a triple-nested walk over every theme/size/extension combination (up to
/// 3 × 14 × 2 = 84 `Path::exists()` calls). Without this, re-searching the
/// same term or just switching tabs back and forth re-pays that full stat
/// storm from scratch every single time, for icon names that were already
/// resolved moments earlier. A plain `Mutex`, not thread-local, because
/// this is called both directly on the GTK main thread (`widgets::package_image`)
/// and from background lookups (`resolve_cached` et al.) via `spawn_blocking`.
fn icon_file_cache() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Finds an actual icon file for `icon_name` in the installed icon themes,
/// preferring PNG (cheap to display) over SVG.
fn find_icon_file(icon_name: &str) -> Option<PathBuf> {
    // Icon= can already be an absolute path — never cached, since it's
    // already a single `exists()` call and every package's is different,
    // so caching it would only grow the map for no repeat benefit.
    if icon_name.starts_with('/') {
        let path = PathBuf::from(icon_name);
        return path.exists().then_some(path);
    }

    if let Some(cached) = icon_file_cache().lock().unwrap().get(icon_name) {
        return cached.clone();
    }

    let mut svg_fallback = None;
    let mut found = None;
    'search: for theme_dir in ICON_THEME_DIRS {
        for size in SIZE_DIRS {
            for ext in ["png", "svg"] {
                let candidate = PathBuf::from(format!("{theme_dir}/{size}/apps/{icon_name}.{ext}"));
                if candidate.exists() {
                    if ext == "png" {
                        found = Some(candidate);
                        break 'search;
                    } else if svg_fallback.is_none() {
                        svg_fallback = Some(candidate);
                    }
                }
            }
        }
    }
    let result = found.or(svg_fallback);
    icon_file_cache().lock().unwrap().insert(icon_name.to_string(), result.clone());
    result
}

fn icon_rank(path: &Path) -> (u8, usize) {
    let ext_rank = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => 0,
        Some("svg") => 1,
        _ => 2,
    };
    let path_str = path.to_string_lossy();
    let size_rank = SIZE_DIRS
        .iter()
        .position(|size| path_str.contains(size))
        .unwrap_or(SIZE_DIRS.len());
    (ext_rank, size_rank)
}

/// Fallback for packages that install icon files directly (icon themes,
/// or apps whose `.desktop` `Icon=` name doesn't match what we guessed):
/// scan the package's own file list for anything living under an icon
/// theme's `apps/` directory or the legacy `/usr/share/pixmaps`.
fn find_icon_in_contents(contents: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("obj") {
            continue;
        }
        let Some(path) = parts.next() else { continue };
        let is_icon_location = (path.contains("/icons/") && path.contains("/apps/")) || path.contains("/pixmaps/");
        let has_icon_ext = path.ends_with(".png") || path.ends_with(".svg") || path.ends_with(".xpm");
        if is_icon_location && has_icon_ext {
            candidates.push(PathBuf::from(path));
        }
    }
    candidates.sort_by_key(|p| icon_rank(p));
    candidates.into_iter().find(|p| p.exists())
}

/// Resolves the real desktop icon for an *installed* package. Tries, in
/// order: the package's `.desktop` file `Icon=` key resolved through the
/// system icon theme, then any icon file the package installed directly.
/// Portage doesn't have per-package artwork of its own — this only works
/// for packages that ship *some* icon and are actually installed (so
/// there's something on disk to find).
/// `resolve_installed_icon`'s own result cache, keyed by
/// `category/name-version` — a version already installed never gets its
/// `CONTENTS` file rewritten under it (a reinstall at the same version is
/// rare, and would just mean a stale cache entry until the app restarts,
/// not a wrong *icon*). Every `rescan_installed()` — triggered by install,
/// uninstall, and each periodic update check — otherwise re-reads and
/// re-scans *every* installed package's `CONTENTS` from scratch, most of
/// which haven't changed since the last scan.
fn installed_icon_cache() -> &'static Mutex<HashMap<String, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn resolve_installed_icon(category: &str, name: &str, version: &str) -> Option<PathBuf> {
    let cache_key = format!("{category}/{name}-{version}");
    if let Some(cached) = installed_icon_cache().lock().unwrap().get(&cache_key) {
        return cached.clone();
    }

    let contents_path = format!("/var/db/pkg/{cache_key}/CONTENTS");
    let contents = fs::read_to_string(&contents_path).ok().unwrap_or_default();

    let result = if let Some(desktop_file) = find_desktop_file(name, &contents)
        && let Some(icon_name) = read_icon_name(&desktop_file)
        && let Some(icon) = find_icon_file(&icon_name)
    {
        Some(icon)
    } else {
        find_icon_in_contents(&contents)
    };

    installed_icon_cache().lock().unwrap().insert(cache_key, result.clone());
    result
}

/// Candidate icon names to try for a raw ebuild package name, most exact
/// match first. Ebuild names often line up with Papirus's icon names
/// directly (`firefox`, `gimp`, `blender`); a few common Gentoo naming
/// conventions (`-bin` binary packages, prefixed sub-projects) get a
/// second chance.
fn candidate_names(package_name: &str) -> Vec<String> {
    let mut candidates = vec![package_name.to_string()];
    if let Some(stripped) = package_name.strip_suffix("-bin") {
        candidates.push(stripped.to_string());
    }
    if let Some((prefix, _)) = package_name.split_once('-')
        && prefix.len() >= 4 {
            candidates.push(prefix.to_string());
        }
    candidates
}

/// Looks up an icon purely by package name, independent of install state —
/// this is what makes icons show up for search results too, not just
/// already-installed packages. Best-effort and occasionally wrong (a short
/// prefix match can land on an unrelated icon of the same name), but a
/// generic-looking real icon beats a letter badge for anything Papirus
/// happens to recognize.
pub fn resolve_by_name(package_name: &str) -> Option<PathBuf> {
    candidate_names(package_name)
        .into_iter()
        .find_map(|name| find_icon_file(&name))
}

/// Adds one more source to `resolve_by_name`: an icon Flathub supplied for
/// this package on a previous visit to its detail page.
///
/// Cache-only and never networked, because this runs once per card and a
/// search can put hundreds on screen. Coverage therefore fills in as pages
/// get opened, rather than all at once.
pub fn resolve_cached(package_name: &str) -> Option<PathBuf> {
    if let Some(path) = resolve_by_name(package_name) {
        return Some(path);
    }
    let app = crate::portage::flathub::cached(package_name)?;
    let url = app.icon?;
    crate::portage::media::cached(&url)
}
