use super::package_list_file;
use anyhow::Result;
use std::collections::BTreeMap;

/// Same `zz-` naming/sorting convention as `package_use.rs`'s managed
/// file — sorts last, wins over any conflicting entry elsewhere.
const MANAGED_FILE: &str = "/etc/portage/package.license/zz-portage-store";

pub fn read_managed() -> Result<BTreeMap<String, Vec<String>>> {
    package_list_file::read(MANAGED_FILE)
}

/// Accepts `license` for one exact atom — the step behind installing
/// anything under a restrictive license (firmware blobs, proprietary
/// fonts, `google-chrome-stable`'s EULA) that isn't `GPL`-alike, and
/// which otherwise everyone just googles the exact incantation for every
/// single time.
pub fn accept(atom: &str, license: &str) -> Result<()> {
    package_list_file::add(MANAGED_FILE, atom, license)
}

pub fn remove(atom: &str, license: &str) -> Result<()> {
    package_list_file::remove(MANAGED_FILE, atom, license)
}
