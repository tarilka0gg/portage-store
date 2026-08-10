use super::emerge::Job;
use anyhow::{Context, Result};
use std::process::Command;

/// One version of `atom` this system has a locally cached binary package
/// for — installable via `downgrade_job` without touching the network or
/// recompiling.
///
/// Deliberately holds `version` as a plain, unparsed `String` — this app
/// has no real Gentoo-version-ordering logic anywhere (no `_pre`/`_rc`/
/// `_p`/`-r`N-aware comparator), so `cached_versions` can list what's
/// available but nothing here can yet say "is this actually older than
/// what's installed," only "is it different." A crate called `pkgcraft`
/// (github.com/pkgcraft/pkgcraft, MIT, real and actively developed) was
/// evaluated for exactly this — its `Version` type parses and compares
/// Gentoo version strings correctly — but its own maintainers describe
/// the project as "highly experimental," and it hasn't left the `0.0.x`
/// version range across 30+ published releases since 2021, with no
/// confirmed production adoption found anywhere. Not pulled in now.
/// Revisit here specifically once either pkgcraft reaches a stabler
/// release line, or this feature's version list actually needs real
/// ordering (e.g. to sort it or to warn "this isn't actually older") —
/// whichever comes first is a good trigger to look again, not before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedVersion {
    pub version: String,
}

fn pkgdir() -> Result<String> {
    let output = Command::new("portageq").args(["envvar", "PKGDIR"]).output().context("failed to run portageq envvar PKGDIR")?;
    if !output.status.success() {
        anyhow::bail!("portageq envvar PKGDIR exited with {:?}: {}", output.status.code(), String::from_utf8_lossy(&output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Parses portage's own `${PKGDIR}/Packages` index — a plain-text,
/// blank-line-separated set of blocks, the first being tree-wide metadata
/// (no `CPV:` field) and every one after that describing a single cached
/// binary package. Real shape (captured from this system after a
/// `--buildpkg` install):
///
/// ```text
/// CPV: acct-group/avahi-0-r3
/// PATH: acct-group/avahi/avahi-0-r3-1.gpkg.tar
/// ...
/// ```
///
/// Only `CPV:` is read — the rest of each block (USE, KEYWORDS, BUILD_ID,
/// ...) isn't needed for "which versions exist", just "does one exist".
fn parse_packages_index(text: &str, atom: &str) -> Vec<CachedVersion> {
    let prefix = format!("{atom}-");
    let mut versions = Vec::new();
    for block in text.split("\n\n") {
        for line in block.lines() {
            if let Some(cpv) = line.strip_prefix("CPV: ")
                && let Some(version) = cpv.strip_prefix(&prefix)
            {
                versions.push(CachedVersion { version: version.to_string() });
            }
        }
    }
    versions
}

/// Every version of `atom` with a locally cached binary package —
/// installable via `downgrade_job` with zero network/compile time. Reads
/// `${PKGDIR}/Packages` (portage's own maintained index, authoritative
/// when `FEATURES=pkgdir-index-trusted` is set, which it is by default on
/// current portage) rather than walking `PKGDIR` and re-parsing
/// filenames by hand.
pub fn cached_versions(atom: &str) -> Result<Vec<CachedVersion>> {
    let dir = pkgdir()?;
    let index_path = format!("{dir}/Packages");
    let Ok(text) = std::fs::read_to_string(&index_path) else {
        return Ok(Vec::new());
    };
    Ok(parse_packages_index(&text, atom))
}

/// Reinstalls `atom` at exactly `version` from the local binpkg cache —
/// `--usepkgonly` refuses to fall back to source if the binpkg somehow
/// isn't there by the time this actually runs (confirmed via `man
/// emerge`: "All the binary packages must be available at the time of
/// dependency calculation or emerge will simply abort"), and
/// `--getbinpkg` is deliberately omitted — this is a purely local
/// reinstall, not a remote binhost fetch.
pub fn downgrade_job(atom: &str, version: &str) -> Job {
    Job {
        privileged: true,
        binary: "emerge".into(),
        args: vec!["--ask=n".into(), "--usepkgonly".into(), format!("={atom}-{version}")],
        jobs_override: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_packages_index() {
        // Captured verbatim (trimmed) from this system's own
        // /var/cache/binpkgs/Packages after a real --buildpkg install.
        let text = "\
ACCEPT_KEYWORDS: amd64
FEATURES: buildpkg buildpkg-live pkgdir-index-trusted
PACKAGES: 1
TIMESTAMP: 1786369059

BUILD_ID: 1
BUILD_TIME: 1786369058
CPV: acct-group/avahi-0-r3
DEFINED_PHASES: install preinst pretend
EAPI: 8
PATH: acct-group/avahi/avahi-0-r3-1.gpkg.tar
SIZE: 20480
";
        let versions = parse_packages_index(text, "acct-group/avahi");
        assert_eq!(versions, vec![CachedVersion { version: "0-r3".to_string() }]);
    }

    #[test]
    fn only_matching_atoms_are_returned() {
        let text = "\
PACKAGES: 2

CPV: acct-group/avahi-0-r3
PATH: acct-group/avahi/avahi-0-r3-1.gpkg.tar

CPV: dev-libs/openssl-3.5.0
PATH: dev-libs/openssl/openssl-3.5.0-1.gpkg.tar
";
        let versions = parse_packages_index(text, "acct-group/avahi");
        assert_eq!(versions, vec![CachedVersion { version: "0-r3".to_string() }]);
    }

    #[test]
    fn multiple_cached_versions_of_the_same_atom_are_all_returned() {
        let text = "\
PACKAGES: 2

CPV: dev-libs/openssl-3.5.0
PATH: dev-libs/openssl/openssl-3.5.0-1.gpkg.tar

CPV: dev-libs/openssl-3.4.1
PATH: dev-libs/openssl/openssl-3.4.1-1.gpkg.tar
";
        let versions = parse_packages_index(text, "dev-libs/openssl");
        assert_eq!(versions, vec![
            CachedVersion { version: "3.5.0".to_string() },
            CachedVersion { version: "3.4.1".to_string() },
        ]);
    }

    #[test]
    fn an_empty_index_yields_nothing() {
        assert_eq!(parse_packages_index("", "dev-libs/openssl"), Vec::new());
    }
}
