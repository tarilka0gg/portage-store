use anyhow::{Context, Result};
use std::collections::HashMap;
use std::process::Command;

/// A single hit from `flatpak search` — one app on one remote. The same
/// app can appear more than once if it's on several remotes; callers that
/// want one row per app should dedupe on `app_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakApp {
    pub app_id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub remote: String,
}

/// An installed Flatpak app, as `flatpak list` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledFlatpak {
    pub app_id: String,
    pub name: String,
    pub version: String,
    pub installed_kib: Option<u64>,
    pub origin: String,
}

/// Whether the Flatpak backend should even offer itself. Per the "auto
/// detection" design: it turns itself on if the `flatpak` binary exists
/// *and* at least one remote is configured — a bare `flatpak` binary with
/// no remotes can't actually install anything, and showing Flatpak
/// results (or worse, a "set up Flatpak" prompt) in that state would be
/// exactly the kind of noise this feature exists to avoid.
pub fn is_available() -> bool {
    which("flatpak") && has_any_remote()
}

fn which(binary: &str) -> bool {
    Command::new("which").arg(binary).output().map(|o| o.status.success()).unwrap_or(false)
}

fn has_any_remote() -> bool {
    for scope in ["--user", "--system"] {
        if let Ok(output) = Command::new("flatpak").arg("remotes").arg(scope).output()
            && output.status.success()
            && !String::from_utf8_lossy(&output.stdout).trim().is_empty()
        {
            return true;
        }
    }
    false
}

/// Parses a human-readable size (`"99.6 MB"`, `"3.3 kB"`, `"1.2 GB"`,
/// as `flatpak remote-info`/`flatpak list` print them) into KiB — the
/// same unit the rest of the app already sizes downloads in (see
/// `emerge::format_size_kib`).
fn parse_size_kib(text: &str) -> Option<u64> {
    let text = text.trim();
    let split_at = text.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let (number, unit) = text.split_at(split_at);
    let value: f64 = number.parse().ok()?;
    let multiplier = match unit.trim().to_lowercase().as_str() {
        "b" | "bytes" => 1.0 / 1024.0,
        "kb" | "kib" => 1.0,
        "mb" | "mib" => 1024.0,
        "gb" | "gib" => 1024.0 * 1024.0,
        _ => return None,
    };
    Some((value * multiplier).round() as u64)
}

/// Searches every configured remote for `query`. Real output is
/// tab-separated: name, description, application id, version, branch,
/// remote — explicit `--columns` pins that shape rather than relying on
/// whatever `flatpak`'s own default happens to be.
pub fn search(query: &str) -> Result<Vec<FlatpakApp>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let output = Command::new("flatpak")
        .args(["search", "--columns=name,description,application,version,remotes"])
        .arg(query)
        .output()
        .context("failed to run flatpak (is it installed?)")?;
    // A genuine no-match search exits 1 with no stdout — same "not an
    // error" shape as eix's own zero-result exit code.
    if !output.status.success() && output.status.code() != Some(1) {
        anyhow::bail!("flatpak search exited with {:?}: {}", output.status.code(), String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().filter_map(parse_search_line).collect())
}

fn parse_search_line(line: &str) -> Option<FlatpakApp> {
    let mut cols = line.split('\t');
    let name = cols.next()?.to_string();
    let description = cols.next().unwrap_or_default().to_string();
    let app_id = cols.next()?.to_string();
    let version = cols.next().unwrap_or_default().to_string();
    let remote = cols.next().unwrap_or_default().to_string();
    (!app_id.is_empty()).then_some(FlatpakApp { app_id, name, description, version, remote })
}

/// Every currently installed Flatpak app (not runtimes), keyed by app id.
pub fn installed() -> Result<HashMap<String, InstalledFlatpak>> {
    let output = Command::new("flatpak")
        .args(["list", "--app", "--columns=application,name,version,size,origin"])
        .output()
        .context("failed to run flatpak (is it installed?)")?;
    if !output.status.success() {
        anyhow::bail!("flatpak list exited with {:?}: {}", output.status.code(), String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let app_id = cols.next()?.to_string();
            let name = cols.next().unwrap_or_default().to_string();
            let version = cols.next().unwrap_or_default().to_string();
            let installed_kib = cols.next().and_then(parse_size_kib);
            let origin = cols.next().unwrap_or_default().to_string();
            (!app_id.is_empty()).then(|| (app_id.clone(), InstalledFlatpak { app_id, name, version, installed_kib, origin }))
        })
        .collect())
}

/// Download and installed size for an app not yet installed — the
/// "honestly show what the first Flatpak install actually costs" figure
/// (a shared runtime alone can be hundreds of MB to a GB). `None` for
/// either field just means `flatpak remote-info` didn't print that line,
/// not an error — some entries (runtimes already satisfied locally)
/// legitimately have nothing to download.
pub fn remote_info_size(remote: &str, app_id: &str) -> Option<(Option<u64>, Option<u64>)> {
    let output = Command::new("flatpak").args(["remote-info", remote, app_id]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut download_kib = None;
    let mut installed_kib = None;
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("Download:") {
            download_kib = parse_size_kib(rest);
        } else if let Some(rest) = line.trim_start().strip_prefix("Installed:") {
            installed_kib = parse_size_kib(rest);
        }
    }
    Some((download_kib, installed_kib))
}

/// One line of a Flatpak job's progress, as streamed by `emerge`-style
/// consumers (see `ui::runtime::spawn_flatpak_job`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatpakProgress {
    pub percent: u32,
}

/// `flatpak install`/`update`, run with `-y` (not `--noninteractive`,
/// which suppresses the percentage entirely) print repeated redraws of
/// a line like `Installing… ████████            13%` — piped output has
/// no cursor control, so each redraw lands on its own line rather than
/// overwriting in place, which is what makes this parseable at all.
pub fn parse_progress(line: &str) -> Option<FlatpakProgress> {
    let trimmed = line.trim();
    if !(trimmed.starts_with("Installing") || trimmed.starts_with("Updating")) {
        return None;
    }
    let percent_text = trimmed.rsplit(' ').find(|tok| tok.ends_with('%'))?;
    let percent: u32 = percent_text.trim_end_matches('%').parse().ok()?;
    Some(FlatpakProgress { percent })
}

/// The `--user` installation, not `--system`: the whole point of a second
/// backend with `Caps { needs_root: false, .. }` is that it never has to
/// go through `pkexec` — a system-wide Flatpak install would defeat that.
const SCOPE: &str = "--user";

/// Every Flatpak job reuses `portage::emerge::Job`/`run` outright rather
/// than a parallel type — the mechanics (spawn a binary with some argv,
/// stream combined stdout/stderr line by line) are identical, and `Job`
/// was already general enough for `sandbox.rs` to reuse it for a
/// privileged shell script rather than one more copy of the same
/// plumbing. `privileged` is always `false` here: unlike a Portage job,
/// nothing a Flatpak job does ever needs root.
use crate::portage::emerge::Job;

pub fn install_job(remote: &str, app_id: &str) -> Job {
    Job {
        privileged: false,
        binary: "flatpak".into(),
        args: vec!["install".into(), SCOPE.into(), "-y".into(), remote.into(), app_id.into()],
        jobs_override: None,
    }
}

/// Adds Flathub as a `--user` remote if it isn't already one — the
/// one-time setup a fresh `--user` install needs, since this app never
/// touches the system-wide installation the distro may have configured.
pub fn ensure_user_flathub() -> Result<()> {
    let output = Command::new("flatpak").args(["remotes", "--user"]).output().context("failed to run flatpak")?;
    let already = String::from_utf8_lossy(&output.stdout).lines().any(|l| l.split_whitespace().next() == Some("flathub"));
    if already {
        return Ok(());
    }
    let status = Command::new("flatpak")
        .args(["remote-add", "--user", "--if-not-exists", "flathub", "https://dl.flathub.org/repo/flathub.flatpakrepo"])
        .status()
        .context("failed to run flatpak remote-add")?;
    if !status.success() {
        anyhow::bail!("failed to add the Flathub remote");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_megabytes_kilobytes_and_gigabytes() {
        assert_eq!(parse_size_kib("99.6 MB"), Some(101990));
        assert_eq!(parse_size_kib("3.3 kB"), Some(3));
        assert_eq!(parse_size_kib("1.2 GB"), Some(1258291));
        assert_eq!(parse_size_kib("512 bytes"), Some(1));
    }

    #[test]
    fn search_line_splits_on_tabs_in_order() {
        let app = parse_search_line("GNU Image Manipulation Program\tHigh-end image creation\torg.gimp.GIMP\t3.2.4\tflathub").unwrap();
        assert_eq!(app.app_id, "org.gimp.GIMP");
        assert_eq!(app.name, "GNU Image Manipulation Program");
        assert_eq!(app.version, "3.2.4");
        assert_eq!(app.remote, "flathub");
    }

    #[test]
    fn a_line_with_no_app_id_is_skipped() {
        assert!(parse_search_line("").is_none());
    }

    #[test]
    fn install_progress_percentage_is_extracted_from_the_redrawn_bar_line() {
        assert_eq!(parse_progress("Installing…").map(|p| p.percent), None);
        assert_eq!(parse_progress("Installing…                        0%  0 bytes/s").unwrap().percent, 0);
        assert_eq!(parse_progress("Installing… ████████            13%").unwrap().percent, 13);
        assert_eq!(parse_progress("Installing… ████████████████████████████ 100%").unwrap().percent, 100);
    }

    #[test]
    fn non_progress_lines_are_not_mistaken_for_progress() {
        assert!(parse_progress("Looking for matches…").is_none());
        assert!(parse_progress("Installation complete.").is_none());
    }
}
