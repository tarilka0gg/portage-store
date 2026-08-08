use super::package_list_file;
use anyhow::Result;
use std::collections::BTreeMap;
use std::process::Command;

/// Same `zz-` naming/sorting convention as `package_use.rs`'s managed
/// file — sorts last, wins over any conflicting entry elsewhere.
const MANAGED_FILE: &str = "/etc/portage/package.accept_keywords/zz-portage-store";

pub fn read_managed() -> Result<BTreeMap<String, Vec<String>>> {
    package_list_file::read(MANAGED_FILE)
}

/// Accepts `keyword` (typically `~<arch>`, the unstable/testing marker)
/// for one exact atom — "I want the testing version of *this* package",
/// the single most common reason anyone ever touches
/// `package.accept_keywords` by hand.
pub fn accept(atom: &str, keyword: &str) -> Result<()> {
    package_list_file::add(MANAGED_FILE, atom, keyword)
}

pub fn remove(atom: &str, keyword: &str) -> Result<()> {
    package_list_file::remove(MANAGED_FILE, atom, keyword)
}

/// This system's own unstable keyword — `~amd64` on a normal x86_64
/// install, `~arm64` on arm64, etc. Read from `portageq`'s own `ARCH`
/// rather than hardcoded, since assuming amd64 would silently suggest the
/// wrong keyword on anything else.
pub fn unstable_keyword() -> Option<String> {
    let output = Command::new("portageq").args(["envvar", "ARCH"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let arch = String::from_utf8(output.stdout).ok()?;
    let arch = arch.trim();
    (!arch.is_empty()).then(|| format!("~{arch}"))
}
