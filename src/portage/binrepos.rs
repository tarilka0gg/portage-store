use super::priv_write;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Standard location — either a single file or (on most modern stage3
/// images, this one included) a directory of `*.conf` files, the same
/// dual-form convention `package.use`/`repos.conf`/etc. all follow.
pub const BINREPOS_CONF: &str = "/etc/portage/binrepos.conf";

/// The file this app's own entries live in — kept separate from whatever
/// a stage3 or the user already configured (typically `gentoo.conf`,
/// pointing at the official binhost) so adding a repo here can never
/// clobber that.
const MANAGED_FILE_NAME: &str = "zz-portage-store.conf";

/// One `[name]` section of a `binrepos.conf` file — a remote repo of
/// prebuilt packages `emerge --getbinpkg` can pull from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinRepo {
    pub name: String,
    pub sync_uri: String,
    pub priority: Option<i32>,
    /// Whether this entry lives in this app's own managed file — only
    /// these are safe to remove from the GUI; anything else (a stage3's
    /// own `gentoo.conf`, a hand-edited file) is shown but left alone.
    pub managed: bool,
}

/// Parses one `binrepos.conf`-format file's text into its `[name]`
/// sections. Unknown keys within a section are ignored rather than
/// rejected — this only needs the handful of fields the GUI actually
/// shows, not full fidelity with every key portage understands.
fn parse(text: &str, managed: bool) -> Vec<BinRepo> {
    let mut repos = Vec::new();
    let mut current: Option<BinRepo> = None;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            if let Some(repo) = current.take() {
                repos.push(repo);
            }
            current = Some(BinRepo { name: name.to_string(), sync_uri: String::new(), priority: None, managed });
            continue;
        }
        let Some(repo) = current.as_mut() else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "sync-uri" => repo.sync_uri = value.to_string(),
            "priority" => repo.priority = value.parse().ok(),
            _ => {}
        }
    }
    if let Some(repo) = current.take() {
        repos.push(repo);
    }
    repos
}

fn managed_path() -> PathBuf {
    Path::new(BINREPOS_CONF).join(MANAGED_FILE_NAME)
}

/// Every configured binary repo — from whatever `binrepos.conf` already
/// has (typically a stage3-provided `gentoo.conf` pointing at Gentoo's
/// official binhost) plus this app's own managed entries, if any.
pub fn read() -> Result<Vec<BinRepo>> {
    let path = Path::new(BINREPOS_CONF);
    if path.is_dir() {
        let mut repos = Vec::new();
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(path).context("failed to read binrepos.conf directory")?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for file in entries {
            if file.extension().and_then(|e| e.to_str()) != Some("conf") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            let managed = file.file_name().and_then(|n| n.to_str()) == Some(MANAGED_FILE_NAME);
            repos.extend(parse(&text, managed));
        }
        Ok(repos)
    } else if path.is_file() {
        let text = std::fs::read_to_string(path).context("failed to read binrepos.conf")?;
        Ok(parse(&text, false))
    } else {
        Ok(Vec::new())
    }
}

fn render_managed(repos: &[BinRepo]) -> String {
    let mut out = String::from("# Managed by portage-store. Do not edit by hand;\n# changes made here will be overwritten by the GUI.\n");
    for repo in repos {
        out.push_str(&format!("\n[{}]\n", repo.name));
        out.push_str(&format!("sync-uri = {}\n", repo.sync_uri));
        if let Some(priority) = repo.priority {
            out.push_str(&format!("priority = {priority}\n"));
        }
    }
    out
}

/// Adds one repo to this app's own managed file — never touches whatever
/// else is already configured. Requires root since `/etc/portage` isn't
/// user-writable.
pub fn add(name: &str, sync_uri: &str, priority: Option<i32>) -> Result<()> {
    let mut managed: Vec<BinRepo> =
        read()?.into_iter().filter(|r| r.managed).collect();
    managed.retain(|r| r.name != name);
    managed.push(BinRepo { name: name.to_string(), sync_uri: sync_uri.to_string(), priority, managed: true });
    write_file_ensuring_dir(&render_managed(&managed))
}

/// Removes one of this app's own managed repos by name. A no-op (not an
/// error) if it's already gone, or if it belongs to some other file this
/// app doesn't own — callers only offer this for entries `read()` marked
/// `managed`, so that second case shouldn't come up in practice.
pub fn remove(name: &str) -> Result<()> {
    let managed: Vec<BinRepo> = read()?.into_iter().filter(|r| r.managed && r.name != name).collect();
    write_file_ensuring_dir(&render_managed(&managed))
}

/// `binrepos.conf` may not exist as a directory yet on a system that's
/// never had a binary repo configured — created (as root, alongside the
/// managed file itself) rather than requiring one to already be there.
fn write_file_ensuring_dir(content: &str) -> Result<()> {
    if !Path::new(BINREPOS_CONF).exists() {
        let out = std::process::Command::new("pkexec").args(["mkdir", "-p", BINREPOS_CONF]).output().context("failed to launch pkexec")?;
        if !out.status.success() {
            anyhow::bail!("failed to create {BINREPOS_CONF}: {}", String::from_utf8_lossy(&out.stderr));
        }
    }
    priv_write::write_file_as_root(managed_path().to_string_lossy().as_ref(), content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_one_section() {
        let text = "\
[gentoo]
priority = 1
sync-uri = https://distfiles.gentoo.org/releases/amd64/binpackages/23.0/x86-64
location = /var/cache/binhost/gentoo
verify-signature = true
";
        let repos = parse(text, false);
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].name, "gentoo");
        assert_eq!(repos[0].priority, Some(1));
        assert_eq!(repos[0].sync_uri, "https://distfiles.gentoo.org/releases/amd64/binpackages/23.0/x86-64");
    }

    #[test]
    fn parses_multiple_sections() {
        let text = "[a]\nsync-uri = https://a.example\n\n[b]\nsync-uri = https://b.example\npriority = 5\n";
        let repos = parse(text, false);
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].name, "a");
        assert_eq!(repos[1].name, "b");
        assert_eq!(repos[1].priority, Some(5));
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let text = "# a comment\n\n[gentoo]\n# another comment\nsync-uri = https://x.example\n";
        let repos = parse(text, false);
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].sync_uri, "https://x.example");
    }

    #[test]
    fn rendered_managed_file_round_trips() {
        let repos = vec![
            BinRepo { name: "mine".to_string(), sync_uri: "https://mine.example".to_string(), priority: Some(2), managed: true },
        ];
        let rendered = render_managed(&repos);
        let parsed = parse(&rendered, true);
        assert_eq!(parsed, repos);
    }
}
