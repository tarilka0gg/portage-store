use super::priv_write::write_file_as_root;
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// portage-store keeps all of its own USE overrides in a single, clearly
/// named file inside the package.use directory. The `zz-` prefix makes it
/// sort last alphabetically, so our choices win over any conflicting entry
/// in the user's other package.use files (portage applies files in
/// directory order, last one wins for a given flag).
const MANAGED_FILE: &str = "/etc/portage/package.use/zz-portage-store";

/// flag name -> enabled/disabled, for one package atom.
pub type FlagOverrides = BTreeMap<String, bool>;

/// Reads every override this tool has previously written, keyed by atom
/// (`category/name`).
pub fn read_managed() -> Result<BTreeMap<String, FlagOverrides>> {
    let path = Path::new(MANAGED_FILE);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", MANAGED_FILE))?;

    let mut result = BTreeMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(atom) = tokens.next() else { continue };
        let flags: FlagOverrides = tokens
            .map(|tok| {
                if let Some(f) = tok.strip_prefix('-') {
                    (f.to_string(), false)
                } else {
                    (tok.to_string(), true)
                }
            })
            .collect();
        result.insert(atom.to_string(), flags);
    }
    Ok(result)
}

/// Sets a single USE flag override for a package atom and rewrites the
/// managed file. Requires root (call through the privileged helper).
pub fn set_flag(atom: &str, flag: &str, enabled: bool) -> Result<()> {
    let mut all = read_managed()?;
    let entry = all.entry(atom.to_string()).or_default();
    entry.insert(flag.to_string(), enabled);
    write_managed(&all)
}


fn write_managed(all: &BTreeMap<String, FlagOverrides>) -> Result<()> {
    let mut out = String::new();
    out.push_str("# Managed by portage-store. Do not edit by hand;\n");
    out.push_str("# changes made here will be overwritten by the GUI.\n");
    for (atom, flags) in all {
        if flags.is_empty() {
            continue;
        }
        out.push_str(atom);
        for (flag, enabled) in flags {
            out.push(' ');
            if !enabled {
                out.push('-');
            }
            out.push_str(flag);
        }
        out.push('\n');
    }

    write_file_as_root(MANAGED_FILE, &out).with_context(|| format!("failed to write {MANAGED_FILE}"))
}
