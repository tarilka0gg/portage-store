use std::process::Command;

/// Bytes free on the filesystem holding `path`, via `df` — no extra crate
/// needed for a single `statvfs` call, and shelling out matches how this
/// app already talks to every other system tool (`eix`, `emerge`, `qlop`).
/// `None` if `path` doesn't exist yet or `df` can't be run at all; callers
/// treat that as "couldn't tell" rather than "no space", since a warning
/// that's wrong more often than right is worse than no warning.
fn available_bytes(path: &str) -> Option<u64> {
    let output = Command::new("df").args(["--output=avail", "-B1", path]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    // `df`'s output is a header line followed by one value line.
    String::from_utf8_lossy(&output.stdout).lines().nth(1)?.trim().parse().ok()
}

/// Free space on whichever filesystem an install/update actually builds
/// on. `PORTAGE_TMPDIR` (default `/var/tmp/portage`) is where the real
/// build workspace — unpacked sources, object files, sometimes a full
/// staged install image — lives, which is usually a different filesystem
/// than `/` on a system that gives `/var/tmp` its own partition or tmpfs.
/// Falls back to `/` when that directory doesn't exist yet (nothing has
/// ever built there), since an empty `/var/tmp/portage` still reports the
/// space of whatever filesystem it *would* land on.
fn build_workspace_free_bytes() -> Option<u64> {
    available_bytes("/var/tmp/portage").or_else(|| available_bytes("/"))
}

/// Whether free space looks too tight for a job that needs to download
/// `download_kib` — and, unlike a plain download, also unpack and build
/// it. Portage doesn't expose a real "how much build workspace will this
/// need" number ahead of time, so this uses a conservative multiple of the
/// download size as a rule of thumb (unpacked source, object files, and a
/// staged install image can easily add up to several times the tarball's
/// own size) rather than an exact figure. Returns `None` when it looks
/// fine, or when the free space couldn't be determined at all — silence
/// is better than a warning that might just be wrong.
const BUILD_WORKSPACE_MULTIPLE: u64 = 3;

pub fn low_space_warning(download_kib: u64) -> Option<String> {
    if download_kib == 0 {
        return None;
    }
    let available = build_workspace_free_bytes()?;
    let needed = download_kib.saturating_mul(BUILD_WORKSPACE_MULTIPLE).saturating_mul(1024);
    if available >= needed {
        return None;
    }
    Some(format!(
        "Only {} free, but this needs roughly {} for the download and build workspace. \
         It may fail partway through.",
        super::emerge::format_size_kib(available / 1024),
        super::emerge::format_size_kib(needed / 1024),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_download_never_warns() {
        assert_eq!(low_space_warning(0), None);
    }
}
