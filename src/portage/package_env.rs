use super::package_list_file;
use super::priv_write::{remove_file_as_root, write_file_as_root};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;

/// Where the environment-override files themselves live — arbitrary
/// shell variable assignments (`CFLAGS`, `FEATURES`, `CC`, ...) applied
/// only to whichever atoms reference a given file by name via
/// `package.env`. Unlike `package.use`/`package.accept_keywords`, these
/// aren't atom+token lines at all; a file's content is a small shell
/// fragment, so it's edited as free text, not structured fields.
const ENV_DIR: &str = "/etc/portage/env";

/// `package.env` itself, same dual "single file or directory of files"
/// convention every other `package.*` location in Gentoo follows (see
/// `binrepos.rs`'s doc comment for the general pattern). This app's own
/// associations live in one managed file inside it, exactly like
/// `package_use.rs`/`package_keywords.rs`/`package_license.rs` already
/// do for their own directories.
const PACKAGE_ENV_DIR: &str = "/etc/portage/package.env";
const MANAGED_ASSOCIATIONS_FILE: &str = "/etc/portage/package.env/zz-portage-store";

/// One named override file and its raw content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvFile {
    pub name: String,
    pub content: String,
}

/// Every file directly under `/etc/portage/env`, sorted by name.
pub fn list_env_files() -> Result<Vec<EnvFile>> {
    let dir = Path::new(ENV_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<EnvFile> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read {ENV_DIR}"))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
            EnvFile { name, content }
        })
        .collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(files)
}

/// Creates or overwrites `name` under `/etc/portage/env` with `content`.
/// As root, and tracked in `config_history` like every other write under
/// `/etc/portage` (see `priv_write::write_file_as_root`).
pub fn write_env_file(name: &str, content: &str) -> Result<()> {
    let path = format!("{ENV_DIR}/{name}");
    write_file_as_root(&path, content).with_context(|| format!("failed to write {path}"))
}

/// Removes an env file outright — note this doesn't also clean up any
/// `package.env` entries that reference it by name (portage itself just
/// silently ignores a reference to a file that no longer exists, so
/// there's no correctness bug here, just a dangling name someone would
/// need to notice and remove separately).
pub fn delete_env_file(name: &str) -> Result<()> {
    remove_file_as_root(Path::new(&format!("{ENV_DIR}/{name}")))
}

/// Every atom -> env-file-name(s) association currently in effect,
/// system-wide — not just this app's own — read the same way portage
/// itself would apply `package.env`: every file directly inside the
/// directory (or the single legacy file, if that's the form this system
/// uses), in name order, later files free to add more associations for
/// an atom already seen in an earlier one.
pub fn read_all_associations() -> Result<BTreeMap<String, Vec<String>>> {
    let path = Path::new(PACKAGE_ENV_DIR);
    let mut result = BTreeMap::new();
    if path.is_dir() {
        let mut entries: Vec<_> =
            std::fs::read_dir(path).with_context(|| format!("failed to read {PACKAGE_ENV_DIR}"))?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            if !entry.path().is_file() {
                continue;
            }
            let Some(path_str) = entry.path().to_str().map(String::from) else { continue };
            merge_associations(&mut result, &package_list_file::read(&path_str)?);
        }
    } else if path.is_file() {
        merge_associations(&mut result, &package_list_file::read(PACKAGE_ENV_DIR)?);
    }
    Ok(result)
}

fn merge_associations(base: &mut BTreeMap<String, Vec<String>>, extra: &BTreeMap<String, Vec<String>>) {
    for (atom, env_files) in extra {
        let entry = base.entry(atom.clone()).or_default();
        for name in env_files {
            if !entry.contains(name) {
                entry.push(name.clone());
            }
        }
    }
}

/// This app's own associations only (the `zz-portage-store` file) —
/// what `disassociate` below is actually able to remove, since removing
/// a line from some other tool's or hand-edited file isn't this app's
/// place to do.
pub fn read_managed_associations() -> Result<BTreeMap<String, Vec<String>>> {
    package_list_file::read(MANAGED_ASSOCIATIONS_FILE)
}

pub fn associate(atom: &str, env_file: &str) -> Result<()> {
    package_list_file::add(MANAGED_ASSOCIATIONS_FILE, atom, env_file)
}

pub fn disassociate(atom: &str, env_file: &str) -> Result<()> {
    package_list_file::remove(MANAGED_ASSOCIATIONS_FILE, atom, env_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_associations_across_files_without_duplicating_names() {
        let mut base = BTreeMap::new();
        base.insert("sys-devel/gcc".to_string(), vec!["gcc-build.conf".to_string()]);
        let mut extra = BTreeMap::new();
        extra.insert("sys-devel/gcc".to_string(), vec!["gcc-build.conf".to_string(), "no-lto.conf".to_string()]);
        extra.insert("net-print/cups".to_string(), vec!["no-lto.conf".to_string()]);

        merge_associations(&mut base, &extra);

        assert_eq!(base.get("sys-devel/gcc"), Some(&vec!["gcc-build.conf".to_string(), "no-lto.conf".to_string()]));
        assert_eq!(base.get("net-print/cups"), Some(&vec!["no-lto.conf".to_string()]));
    }
}
