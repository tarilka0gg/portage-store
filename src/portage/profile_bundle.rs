use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A portable snapshot of "everything that makes this system's package
/// selection what it is": the `@world` set (what to install) plus every
/// file under `/etc/portage` (how to build/select it — USE flags, masks,
/// keywords, overlays, `make.conf`). The Gentoo-native way to move a
/// system's package configuration to a second machine is to copy these
/// two things by hand; this bundles them into one file instead.
///
/// `/etc/portage` is world-readable on a standard install (0755 dirs,
/// 0644 files), so exporting needs no privilege — only *importing* writes
/// as root. This app's own `.git` history under `/etc/portage` (see
/// `priv_write`) is excluded from the bundle: it's this app's local
/// change log, not portage configuration, and has no meaning on another
/// machine.
const WORLD_ENTRY: &str = "world";
const CONFIG_ENTRY: &str = "portage";

/// Writes a `.tar.gz` to `dest` containing the current `@world` set and
/// `/etc/portage`.
pub fn export(dest: &Path) -> Result<()> {
    let atoms = super::world::read().context("failed to read @world")?;

    let staging = unique_temp_dir("portage-store-export");
    std::fs::create_dir_all(&staging).context("failed to create a staging directory")?;
    let result = (|| -> Result<()> {
        std::fs::write(staging.join(WORLD_ENTRY), atoms.join("\n"))
            .context("failed to write the staged world file")?;

        let status = Command::new("tar")
            .arg("czf")
            .arg(dest)
            .arg("--exclude=.git")
            .arg("-C")
            .arg(&staging)
            .arg(WORLD_ENTRY)
            .arg("-C")
            .arg("/etc")
            .arg(CONFIG_ENTRY)
            .status()
            .context("failed to run tar")?;
        if !status.success() {
            bail!("tar exited with {:?}", status.code());
        }
        Ok(())
    })();

    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// A bundle, already extracted to a scratch directory — `atoms` is read
/// straight off disk for preview before anything is applied; `dir` is
/// what `import` below actually copies from.
pub struct ExtractedBundle {
    pub dir: PathBuf,
    pub atoms: Vec<String>,
}

impl Drop for ExtractedBundle {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Extracts `bundle` to a scratch directory and reads back the `@world`
/// atom list it contains, without touching the live system — the caller
/// gets a chance to show the user what an import would do before
/// `import` actually does it.
pub fn extract(bundle: &Path) -> Result<ExtractedBundle> {
    let dir = unique_temp_dir("portage-store-import");
    std::fs::create_dir_all(&dir).context("failed to create a scratch directory")?;

    let status = Command::new("tar")
        .arg("xzf")
        .arg(bundle)
        .arg("-C")
        .arg(&dir)
        .status()
        .context("failed to run tar")?;
    if !status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        bail!("tar exited with {:?} — is this a Portage Store profile bundle?", status.code());
    }

    let world_path = dir.join(WORLD_ENTRY);
    let config_path = dir.join(CONFIG_ENTRY);
    if !world_path.is_file() || !config_path.is_dir() {
        let _ = std::fs::remove_dir_all(&dir);
        bail!("not a Portage Store profile bundle (missing '{WORLD_ENTRY}' or '{CONFIG_ENTRY}')");
    }

    let atoms = std::fs::read_to_string(&world_path)
        .context("failed to read the bundled world file")?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    Ok(ExtractedBundle { dir, atoms })
}

/// Applies a bundle's `/etc/portage` files to the live system, as root.
/// Copies over (`cp -a`, preserving permissions/symlinks) rather than
/// replacing the directory outright — anything already on this machine
/// that the bundle doesn't mention (e.g. a `binrepos.conf` entry specific
/// to this box) is left alone instead of being deleted. Actually
/// installing the bundled `@world` atoms is a separate step (an ordinary
/// `emerge` job, enqueued by the caller), since that can take hours and
/// belongs in the app's normal job queue, not blocking on this call.
pub fn import(bundle: &ExtractedBundle) -> Result<()> {
    let source = bundle.dir.join(CONFIG_ENTRY);
    let status = Command::new("pkexec")
        .arg("cp")
        .arg("-a")
        .arg(format!("{}/.", source.display()))
        .arg("/etc/portage/")
        .status()
        .context("failed to launch pkexec")?;
    if !status.success() {
        bail!("failed to copy the bundled configuration into /etc/portage");
    }
    Ok(())
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("{prefix}-{unique}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_world_atoms_and_config_files() {
        let dest = unique_temp_dir("profile-bundle-test").with_extension("tar.gz");

        // Fabricate a small "system" to export instead of touching the
        // real /etc/portage: point CONFIG_ENTRY at a scratch dir sibling
        // via a symlink swap isn't worth it here — instead this test
        // exercises `extract` directly against a hand-built tarball,
        // which is what `import` actually consumes and is the part with
        // real parsing logic worth covering.
        let staging = unique_temp_dir("profile-bundle-staging");
        std::fs::create_dir_all(staging.join(CONFIG_ENTRY).join("package.use")).unwrap();
        std::fs::write(staging.join(WORLD_ENTRY), "www-client/firefox\napp-editors/neovim\n").unwrap();
        std::fs::write(staging.join(CONFIG_ENTRY).join("make.conf"), "COMMON_FLAGS=\"-O2\"\n").unwrap();

        let status = Command::new("tar")
            .arg("czf")
            .arg(&dest)
            .arg("-C")
            .arg(&staging)
            .arg(WORLD_ENTRY)
            .arg(CONFIG_ENTRY)
            .status()
            .unwrap();
        assert!(status.success());

        let extracted = extract(&dest).unwrap();
        assert_eq!(extracted.atoms, vec!["www-client/firefox".to_string(), "app-editors/neovim".to_string()]);
        assert!(extracted.dir.join(CONFIG_ENTRY).join("make.conf").is_file());

        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn rejects_a_tarball_that_is_not_a_bundle() {
        let dest = unique_temp_dir("profile-bundle-not-a-bundle-test").with_extension("tar.gz");
        let staging = unique_temp_dir("profile-bundle-not-a-bundle-staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("readme.txt"), "not a bundle").unwrap();
        Command::new("tar").arg("czf").arg(&dest).arg("-C").arg(&staging).arg("readme.txt").status().unwrap();

        assert!(extract(&dest).is_err());

        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_file(&dest);
    }
}
