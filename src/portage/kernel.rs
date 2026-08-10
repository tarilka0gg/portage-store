use super::priv_write::HELPER_PATH;
use anyhow::{Context, Result};
use std::process::Command;

/// One entry from `eselect kernel list` — a `/usr/src/linux` symlink
/// target, whether or not it's the one currently selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelTarget {
    pub index: String,
    pub label: String,
    pub selected: bool,
}

/// Parses one line: `  [1]   linux-7.1.3-cachyos0 *` — same
/// `[N]  label [marker]` shape `overlays::parse_line`/`profile::parse_line`
/// already use, just without a URL or parenthesized status to strip.
fn parse_line(line: &str) -> Option<KernelTarget> {
    let rest = line.trim_start().strip_prefix('[')?;
    let (index, rest) = rest.split_once(']')?;
    let rest = rest.trim();
    let (label, selected) = match rest.strip_suffix('*') {
        Some(label) => (label.trim(), true),
        None => (rest, false),
    };
    if label.is_empty() {
        return None;
    }
    Some(KernelTarget { index: index.trim().to_string(), label: label.to_string(), selected })
}

/// Every kernel symlink target `eselect` knows about — unprivileged,
/// synchronous, local (no network round trip the way `overlays::list` can
/// have).
pub fn list_targets() -> Result<Vec<KernelTarget>> {
    let output = Command::new("eselect").args(["kernel", "list"]).output().context("failed to run eselect kernel list")?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_line).collect())
}

/// Switches the selected `/usr/src/linux` symlink target — a direct,
/// blocking privileged call (mirrors `profile::apply`'s pattern) rather
/// than a queued `Job`: this is an instant symlink flip, not a build
/// worth a progress bar. `eselect` is already in the priv-helper's `run`
/// binary allowlist, so this needs no helper script changes.
pub fn select(index: &str) -> Result<()> {
    let output = Command::new("doas")
        .arg(HELPER_PATH)
        .arg("run")
        .arg("--")
        .arg("eselect")
        .arg("kernel")
        .arg("set")
        .arg(index)
        .output()
        .context("failed to launch doas")?;
    if !output.status.success() {
        anyhow::bail!("failed to select kernel: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// One installed, bootable kernel in `/boot` — a `vmlinuz-<version>` file.
/// Deliberately *not* cross-referenced against `KernelTarget`'s own
/// `label`: on a system with a locally renamed release (a custom
/// `EXTRAVERSION`/build config), the source-directory name and the
/// installed kernel's own version string don't string-match at all
/// (confirmed on a real system: `linux-7.1.3-cachyos0` as the source
/// symlink target vs. `vmlinuz-7.1.3-cachyos-trim10-tuned` in `/boot`) —
/// claiming a match this code can't actually verify would be worse than
/// not claiming one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootEntry {
    pub version: String,
}

const BOOT_DIR: &str = "/boot";
const VMLINUZ_PREFIX: &str = "vmlinuz-";

/// Every installed kernel found in `/boot` — read-only, unprivileged.
pub fn boot_entries() -> Result<Vec<BootEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(BOOT_DIR).with_context(|| format!("failed to read {BOOT_DIR}"))? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
        if let Some(version) = name.strip_prefix(VMLINUZ_PREFIX) {
            entries.push(BootEntry { version: version.to_string() });
        }
    }
    entries.sort_by(|a, b| a.version.cmp(&b.version));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_selected_target() {
        let target = parse_line("  [1]   linux-7.1.3-cachyos0 *").unwrap();
        assert_eq!(target, KernelTarget { index: "1".to_string(), label: "linux-7.1.3-cachyos0".to_string(), selected: true });
    }

    #[test]
    fn parses_an_unselected_target() {
        let target = parse_line("  [2]   linux-6.12.0-gentoo").unwrap();
        assert!(!target.selected);
        assert_eq!(target.label, "linux-6.12.0-gentoo");
    }

    #[test]
    fn non_entry_lines_are_ignored() {
        assert_eq!(parse_line("Available kernel symlink targets:"), None);
    }
}
