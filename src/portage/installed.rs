use anyhow::Result;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

const PKG_DB: &str = "/var/db/pkg";

#[derive(Debug, Clone)]
pub struct InstalledPackage {
    pub category: String,
    pub name: String,
    pub version: String,
    /// Flags actually compiled in, as recorded by portage.
    pub enabled_use: HashSet<String>,
}

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Splits a `PF` directory name like `firefox-140.12.0` into (name, version),
/// given the ebuild version is whatever follows the last `-` that looks like
/// a version (starts with a digit).
fn split_pf(pf: &str) -> (String, String) {
    let parts: Vec<&str> = pf.split('-').collect();
    for i in (1..parts.len()).rev() {
        if parts[i].starts_with(|c: char| c.is_ascii_digit()) {
            return (parts[..i].join("-"), parts[i..].join("-"));
        }
    }
    (pf.to_string(), String::new())
}

/// Scans `/var/db/pkg` for every installed package and its recorded USE flags.
pub fn scan() -> Result<Vec<InstalledPackage>> {
    let mut result = Vec::new();
    let root = Path::new(PKG_DB);
    if !root.exists() {
        return Ok(result);
    }

    for cat_entry in fs::read_dir(root)? {
        let cat_entry = cat_entry?;
        if !cat_entry.file_type()?.is_dir() {
            continue;
        }
        let category = cat_entry.file_name().to_string_lossy().into_owned();

        for pkg_entry in fs::read_dir(cat_entry.path())? {
            let pkg_entry = pkg_entry?;
            if !pkg_entry.file_type()?.is_dir() {
                continue;
            }
            let pf = pkg_entry.file_name().to_string_lossy().into_owned();
            let (name, version) = split_pf(&pf);

            let use_flags = read_trimmed(&pkg_entry.path().join("USE"))
                .map(|s| s.split_whitespace().map(String::from).collect())
                .unwrap_or_default();

            result.push(InstalledPackage {
                category: category.clone(),
                name,
                version,
                enabled_use: use_flags,
            });
        }
    }

    Ok(result)
}

