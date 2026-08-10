use super::emerge::Job;
use anyhow::Result;
use std::path::Path;

/// The one pristine, shared build root — a disposable, self-contained
/// Gentoo install used only to build a package the *live* system's
/// resolver won't touch (a hard mask, a keyword mask, a circular/blocking
/// conflict). Fetched and bootstrapped once; every sandbox instance below
/// is an overlayfs layer on top of this exact same base, so adding another
/// instance costs disk space only for what that instance builds on top —
/// never a second stage3 download or a second copy of the base system.
pub const SANDBOX_BASE: &str = "/var/lib/portage-store/sandbox-base";

/// How many overlay instances the build script will try before giving up.
/// Bounded on purpose — "a package that doesn't fit joins another
/// container" should mean a handful of reused, long-lived instances, not
/// one spun up per package ever built. In the common case only instance
/// `0` is ever touched; a second or third only comes into play when
/// something already merged into an earlier instance actually conflicts
/// with the package currently being built there.
pub const MAX_SANDBOX_INSTANCES: u32 = 3;

/// Whether the shared base has already been bootstrapped (stage3
/// extracted) — used to warn the user, before they click through, whether
/// this run will also pay the first-time stage3 download/extract cost.
pub fn is_set_up() -> bool {
    Path::new(SANDBOX_BASE).join("usr/bin/emerge").exists()
}

/// The build script itself now lives only as `resources/sandbox-build.sh`
/// in the repo (the reviewable source of truth) and, installed root-owned,
/// at `/usr/local/libexec/portage-store/sandbox-build.sh` (see
/// `priv_write::HELPER_PATH`'s doc comment) — not duplicated here to avoid
/// the two copies drifting. Under the old `pkexec` design this script got
/// written to the user's own cache directory and executed from there,
/// which a passwordless escalation rule can't afford to trust (a tampered
/// cache file would be silent, zero-friction root code execution); now the
/// helper's own `sandbox-build <atom>` subcommand execs the installed copy
/// directly, and an edit to `resources/sandbox-build.sh` needs a manual
/// re-install to actually take effect (`install -o root -g root -m 0755
/// resources/sandbox-build.sh /usr/local/libexec/portage-store/sandbox-build.sh`).
///
/// Safety invariants that script is written to make structurally hard to
/// violate, because it bind-mounts and overlay-mounts real host
/// directories into roots that could otherwise be torn down carelessly:
/// - The portage tree is mounted **read-only** into every instance; only
///   distfiles/binpkgs (intentionally shared caches) are writable.
/// - Teardown only ever unmounts (each guarded by `mountpoint -q` first)
///   — it never deletes anything. The one destructive step in the whole
///   script (extracting a fresh stage3 into the base) only runs before
///   anything is mounted at all, and only ever touches `SANDBOX_BASE`,
///   never an instance directory.
/// - Every mount this script makes gets torn down in a `trap ... EXIT`,
///   including when a build attempt fails and the script moves on to try
///   the next pool instance — so a failed attempt in instance 0 never
///   leaves it mounted while instance 1 is tried.
///
/// Returns a `Job` whose synthetic `binary` (`"sandbox-build"`) the helper
/// recognizes and dispatches to that installed script, with `atom` as its
/// one argument — reusing the exact same privileged-job queue, progress
/// bar, and streaming-output machinery `emerge::install_job` and friends
/// already use, rather than a second parallel mechanism just for this one
/// kind of job.
pub fn build_job(atom: &str) -> Result<Job> {
    Ok(Job { privileged: true, binary: "sandbox-build".into(), args: vec![atom.to_string()], jobs_override: None })
}

