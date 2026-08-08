use super::priv_write::write_file_as_root;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Shared mechanics behind every `zz-portage-store`-managed file this app
/// keeps under `/etc/portage`: `package.use` (see `package_use.rs`,
/// which predates this and keeps its own richer per-flag +/- API),
/// `package.accept_keywords`, and `package.license`. All three share the
/// exact same on-disk shape — one atom per line, followed by
/// whitespace-separated tokens — so the read/write mechanics live here
/// once, in `Vec<String>` per atom, and each domain-specific module
/// (`package_keywords`, `package_license`) just adds the vocabulary for
/// what a "token" means in its own file.
pub fn read(path: &str) -> Result<BTreeMap<String, Vec<String>>> {
    let p = Path::new(path);
    if !p.exists() {
        return Ok(BTreeMap::new());
    }
    let content = fs::read_to_string(p).with_context(|| format!("failed to read {path}"))?;

    let mut result = BTreeMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(atom) = tokens.next() else { continue };
        result.insert(atom.to_string(), tokens.map(str::to_string).collect());
    }
    Ok(result)
}

/// Adds `token` to `atom`'s line, if it isn't already there.
pub fn add(path: &str, atom: &str, token: &str) -> Result<()> {
    let mut all = read(path)?;
    let entry = all.entry(atom.to_string()).or_default();
    if !entry.iter().any(|t| t == token) {
        entry.push(token.to_string());
    }
    write(path, &all)
}

/// Removes `token` from `atom`'s line — the atom's own line is dropped
/// entirely once it has no tokens left, rather than kept around empty.
pub fn remove(path: &str, atom: &str, token: &str) -> Result<()> {
    let mut all = read(path)?;
    if let Some(entry) = all.get_mut(atom) {
        entry.retain(|t| t != token);
        if entry.is_empty() {
            all.remove(atom);
        }
    }
    write(path, &all)
}

fn write(path: &str, all: &BTreeMap<String, Vec<String>>) -> Result<()> {
    let mut out = String::from("# Managed by portage-store. Do not edit by hand;\n# changes made here will be overwritten by the GUI.\n");
    for (atom, tokens) in all {
        if tokens.is_empty() {
            continue;
        }
        out.push_str(atom);
        for token in tokens {
            out.push(' ');
            out.push_str(token);
        }
        out.push('\n');
    }
    write_file_as_root(path, &out).with_context(|| format!("failed to write {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_real_managed_file() {
        let dir = std::env::temp_dir().join(format!("portage-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("zz-portage-store");
        std::fs::write(
            &path,
            "# Managed by portage-store. Do not edit by hand;\n\
             # changes made here will be overwritten by the GUI.\n\
             www-client/firefox ~amd64\n\
             sys-kernel/linux-firmware linux-fw-redistributable no-source-code\n",
        )
        .unwrap();

        let result = read(path.to_str().unwrap()).unwrap();
        assert_eq!(result.get("www-client/firefox"), Some(&vec!["~amd64".to_string()]));
        assert_eq!(
            result.get("sys-kernel/linux-firmware"),
            Some(&vec!["linux-fw-redistributable".to_string(), "no-source-code".to_string()])
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_missing_file_reads_as_empty_not_an_error() {
        assert_eq!(read("/nonexistent/zz-portage-store-test-path").unwrap(), BTreeMap::new());
    }
}
