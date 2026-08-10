use super::priv_write::HELPER_PATH;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[derive(Debug, Clone)]
pub enum EmergeEvent {
    Line(String),
    Finished { success: bool },
    FailedToStart(String),
}

/// One job invocation: which binary fronts it (root via the `priv-helper`
/// `run` subcommand, or the current user for read-only `--pretend` runs)
/// and its argv. `binary` is almost always `emerge` — the exception is
/// `sandbox::build_job`, which sets it to the synthetic name
/// `"sandbox-build"`, dispatched by the helper to the installed
/// `sandbox-build.sh` instead of through its `emerge`-only binary
/// allowlist; this queue/streaming/progress-bar machinery is otherwise
/// identical either way, rather than duplicating all of it for one more
/// kind of long-running privileged command.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Job {
    pub privileged: bool,
    pub binary: String,
    pub args: Vec<String>,
    /// Set by `resource_limits::throttled` — a validated job count the
    /// helper itself turns into a fixed `ionice/nice/env MAKEOPTS=...`
    /// wrap, built from scratch on the privileged side rather than
    /// assembled here and passed through as argv (which a passwordless
    /// rule can't afford to trust blindly). Ignored for unprivileged jobs.
    pub jobs_override: Option<u32>,
}

impl Job {
    /// Renders exactly the command line `run` would actually execute —
    /// the "escape hatch" for anyone who wants to run a queued job
    /// headless, or just double-check what the GUI is about to do. The
    /// `doas priv-helper run` prefix is spelled out explicitly here
    /// (rather than assumed by the reader) since a script meant to be
    /// read before running is exactly the place that should say so.
    pub fn to_shell_command(&self) -> String {
        let mut parts = Vec::new();
        if self.privileged {
            parts.push("doas".to_string());
            parts.push(HELPER_PATH.to_string());
            if is_synthetic_subcommand(&self.binary) {
                parts.push(self.binary.clone());
                parts.extend(self.args.iter().map(|arg| shell_quote(arg)));
                return parts.join(" ");
            }
            parts.push("run".to_string());
            if let Some(jobs) = self.jobs_override {
                parts.push("--jobs".to_string());
                parts.push(jobs.to_string());
            }
            parts.push("--".to_string());
        }
        parts.push(self.binary.clone());
        parts.extend(self.args.iter().map(|arg| shell_quote(arg)));
        parts.join(" ")
    }
}

/// A `Job::binary` that names a helper subcommand directly (dispatched
/// outside `cmd_run`'s binary allowlist) rather than a real executable on
/// `$PATH` — see `resources/priv-helper.sh`'s `sandbox-build` and
/// `enable-overlay` cases, each of which takes its own validated argument
/// shape instead of arbitrary trailing argv.
fn is_synthetic_subcommand(binary: &str) -> bool {
    matches!(binary, "sandbox-build" | "enable-overlay")
}

/// Quotes `arg` for a POSIX shell only if it actually needs it — plain
/// atoms/flags stay bare so a generated script reads like a human wrote
/// it, rather than every single token wrapped in quotes.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@+~".contains(c)) {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// Runs the command described by `job`, pushing its combined stdout/stderr
/// line by line into `output`. Privileged jobs go through the passwordless
/// `doas priv-helper run` path (see `priv_write::HELPER_PATH`) — no prompt
/// of any kind, by design; see `resources/priv-helper.sh` for the binary
/// allowlist and validation that stands in for it.
///
/// The channel is an `async_channel` one so the GTK side can await it
/// directly on the main context and update widgets as output arrives.
pub async fn run(job: Job, output: async_channel::Sender<EmergeEvent>) {
    let mut command = if job.privileged {
        let mut c = Command::new("doas");
        c.arg(HELPER_PATH);
        if is_synthetic_subcommand(&job.binary) {
            c.arg(&job.binary);
        } else {
            c.arg("run");
            if let Some(jobs) = job.jobs_override {
                c.arg("--jobs").arg(jobs.to_string());
            }
            c.arg("--").arg(&job.binary);
        }
        c
    } else {
        Command::new(&job.binary)
    };

    let mut child = match command
        .args(&job.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            let _ = output.send(EmergeEvent::FailedToStart(err.to_string())).await;
            return;
        }
    };

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();
    let mut stdout_done = false;
    let mut stderr_done = false;

    while !stdout_done || !stderr_done {
        tokio::select! {
            line = stdout_lines.next_line(), if !stdout_done => {
                match line {
                    Ok(Some(line)) => { let _ = output.send(EmergeEvent::Line(line)).await; }
                    _ => stdout_done = true,
                }
            }
            line = stderr_lines.next_line(), if !stderr_done => {
                match line {
                    Ok(Some(line)) => { let _ = output.send(EmergeEvent::Line(line)).await; }
                    _ => stderr_done = true,
                }
            }
        }
    }

    let status = child.wait().await;
    let success = status.map(|s| s.success()).unwrap_or(false);
    let _ = output.send(EmergeEvent::Finished { success }).await;
}

/// Appends `--getbinpkg` when `getbinpkg` is set — the one flag that
/// actually makes Gentoo's official binhost (or any configured
/// `binrepos.conf` remote) count towards what a run considers installable,
/// preferring a matching prebuilt package over building from source.
/// Without it, `emerge` never even asks a remote repo whether it has one.
fn binpkg_args(getbinpkg: bool) -> Vec<String> {
    if getbinpkg { vec!["--getbinpkg".into()] } else { Vec::new() }
}

/// Appends `--buildpkg` when `buildpkg` is set — caches a local binary
/// package of whatever gets merged (see `binpkg.rs`), so an older version
/// stays available to reinstall later (`binpkg::downgrade_job`) without
/// recompiling. Deliberately a per-job flag rather than a global
/// `FEATURES=buildpkg` in `make.conf`: the latter would cache *every*
/// merge forever with no way to scope it, growing `PKGDIR` unbounded for
/// packages nobody will ever want to downgrade.
fn buildpkg_args(buildpkg: bool) -> Vec<String> {
    if buildpkg { vec!["--buildpkg".into()] } else { Vec::new() }
}

/// Installs/updates the given atom.
pub fn install_job(atom: &str, getbinpkg: bool, buildpkg: bool) -> Job {
    let mut args = vec!["--ask=n".into(), "--verbose".into()];
    args.extend(binpkg_args(getbinpkg));
    args.extend(buildpkg_args(buildpkg));
    args.push(atom.into());
    Job { privileged: true, binary: "emerge".into(), args, jobs_override: None }
}

/// Installs/updates several atoms in one run — used for applying an
/// imported profile bundle's `@world` set, where "one emerge job per
/// atom" would mean queuing (and separately confirming) potentially
/// hundreds of jobs for what is really one logical operation.
pub fn install_many_job(atoms: &[String], getbinpkg: bool, buildpkg: bool) -> Job {
    let mut args = vec!["--ask=n".into(), "--verbose".into()];
    args.extend(binpkg_args(getbinpkg));
    args.extend(buildpkg_args(buildpkg));
    args.extend(atoms.iter().cloned());
    Job { privileged: true, binary: "emerge".into(), args, jobs_override: None }
}

/// Dry-run of an install, used to show the user what would happen (download
/// size, source build vs. prebuilt binary) before they commit. Runs
/// unprivileged since `--pretend` never touches the system.
pub fn pretend_install_job(atom: &str, getbinpkg: bool) -> Job {
    let mut args = vec!["--pretend".into(), "--verbose".into()];
    args.extend(binpkg_args(getbinpkg));
    args.push(atom.into());
    Job { privileged: false, binary: "emerge".into(), args, jobs_override: None }
}

/// Disk-backed cache of a `--pretend` run's raw output lines (plus whether
/// it succeeded), keyed by atom and the exact version it was run against.
/// `--pretend` does a full dependency resolution — often the single
/// slowest thing this app does — and re-opening a detail page (or coming
/// back to one already visited) re-ran it from scratch every time even
/// though the answer for the same atom/version pair can't have changed
/// since the last visit, absent the system's own installed packages or USE
/// flags changing in between. Backed by disk (not just memory) so that
/// survives a restart too — reopening the app right after closing it
/// shouldn't re-pay a resolution it just paid for.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PretendCacheEntry {
    version: String,
    success: bool,
    lines: Vec<String>,
    stored_unix_time: u64,
}

/// A safety net, not the primary invalidation path — a sync
/// (`invalidate on sync`, see `ui::queue::start_next`) is the actual event
/// that can change a `--pretend` answer, and clears the whole cache
/// outright. This bounds how stale an entry can get from anything *else*
/// that might shift dependency resolution outside this app's own view
/// (system packages changed from outside it, e.g. a terminal `emerge`).
const PRETEND_CACHE_TTL_SECONDS: u64 = 60 * 60;

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn pretend_cache_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))?;
    let dir = base.join("portage-store");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("pretend_cache.json"))
}

fn load_pretend_cache_from_disk() -> std::collections::HashMap<String, PretendCacheEntry> {
    let Some(path) = pretend_cache_path() else { return std::collections::HashMap::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return std::collections::HashMap::new() };
    let entries: std::collections::HashMap<String, PretendCacheEntry> = serde_json::from_str(&text).unwrap_or_default();
    let now = unix_now();
    entries.into_iter().filter(|(_, e)| now.saturating_sub(e.stored_unix_time) < PRETEND_CACHE_TTL_SECONDS).collect()
}

fn persist_pretend_cache(cache: &std::collections::HashMap<String, PretendCacheEntry>) {
    let Some(path) = pretend_cache_path() else { return };
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, text);
    }
}

fn pretend_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, PretendCacheEntry>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, PretendCacheEntry>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(load_pretend_cache_from_disk()))
}

/// Returns the cached `--pretend` result for `atom`, if it was last run
/// against exactly `version` within the last `PRETEND_CACHE_TTL_SECONDS` —
/// a version bump (or downgrade) invalidates it, since the dependency
/// graph, download size, and USE requirements can all be completely
/// different for a different version. Returns `(success, lines)`.
pub fn cached_pretend(atom: &str, version: &str) -> Option<(bool, Vec<String>)> {
    let cache = pretend_cache().lock().unwrap();
    let entry = cache.get(atom)?;
    if entry.version != version || unix_now().saturating_sub(entry.stored_unix_time) >= PRETEND_CACHE_TTL_SECONDS {
        return None;
    }
    Some((entry.success, entry.lines.clone()))
}

pub fn store_pretend(atom: &str, version: &str, success: bool, lines: Vec<String>) {
    let mut cache = pretend_cache().lock().unwrap();
    cache.insert(atom.to_string(), PretendCacheEntry { version: version.to_string(), success, lines, stored_unix_time: unix_now() });
    persist_pretend_cache(&cache);
}

/// Drops any cached `--pretend` result for `atom` — used when something
/// that would change the answer just happened outside of a version bump
/// (a USE flag flip), so the next lookup re-runs the resolver instead of
/// serving a now-stale cached result for the same atom/version pair.
pub fn invalidate_pretend(atom: &str) {
    let mut cache = pretend_cache().lock().unwrap();
    cache.remove(atom);
    persist_pretend_cache(&cache);
}

/// Drops every cached `--pretend` result — called once a sync completes,
/// since a sync can change the dependency graph for anything in the tree,
/// not just the one atom a USE-flag change (`invalidate_pretend`) affects.
pub fn clear_pretend_cache() {
    let mut cache = pretend_cache().lock().unwrap();
    cache.clear();
    persist_pretend_cache(&cache);
}

/// Whether pinning to this exact version resolves cleanly — no unmet
/// REQUIRED_USE, no mask, no block. Synchronous and blocking on purpose:
/// this exists to be called a few times in a row from inside a background
/// thread (probing the newest handful of versions for the version picker's
/// "Recommended" badge), where going through the async job-streaming
/// machinery for a single yes/no exit code would be pure overhead.
pub fn pretend_version_succeeds(atom: &str, version: &str) -> bool {
    std::process::Command::new("emerge")
        .args(["--pretend", &format!("={atom}-{version}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[derive(Debug, Clone, Default)]
pub struct InstallPreview {
    pub packages_to_build: usize,
    pub will_compile: bool,
    pub download_kib: Option<u64>,
}

/// Parses the human-readable output of `emerge --pretend --verbose` into a
/// plain-language summary. Best-effort: emerge's pretend format isn't a
/// stable machine interface, so this only looks for the lines that matter
/// for the "what's about to happen" summary.
///
/// Worth running even when emerge exited non-zero: a resolver failure late
/// in the run (an unmet REQUIRED_USE, a circular dependency) still leaves a
/// perfectly good package list and download total above it, and showing
/// those beats showing nothing.
pub fn parse_pretend_output(lines: &[String]) -> InstallPreview {
    let mut preview = InstallPreview::default();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("[ebuild") {
            preview.packages_to_build += 1;
            preview.will_compile = true;
        } else if trimmed.starts_with("[binary") {
            preview.packages_to_build += 1;
        } else if let Some((_, rest)) = line.split_once("Size of downloads:") {
            // The summary line reads
            // "Total: 2 packages (1 new), Size of downloads: 895,067 KiB"
            // — note it is *not* prefixed with the word "Total".
            preview.download_kib = parse_kib(rest);
        }
    }
    preview
}

/// Pulls the bare `category/name-version` atoms out of `emerge --pretend`
/// output lines, which look like `[ebuild   U  ] cat/name-1.2 [1.1]
/// USE="..."` — the "what's pending" list itself, as opposed to
/// `parse_pretend_output`'s totals-only summary.
pub fn parse_update_atoms(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if !trimmed.starts_with("[ebuild") && !trimmed.starts_with("[binary") {
                return None;
            }
            let after_bracket = trimmed.split_once(']')?.1.trim();
            let atom_with_version = after_bracket.split_whitespace().next()?;
            Some(atom_with_version.to_string())
        })
        .collect()
}

/// One line of `--pretend --verbose` output, broken down to a single
/// package rather than the run's totals — what the four fact tiles' expand
/// panels are built from: which packages are actually involved, and what
/// each one individually costs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPackage {
    pub atom: String,
    pub version: String,
    pub download_kib: Option<u64>,
    pub is_new: bool,
}

/// Parses every `[ebuild ...]`/`[binary ...]` line into its own package —
/// same input as `parse_pretend_output`, finer-grained output. A line
/// looks like:
/// `[ebuild   R   ~] media-video/obs-studio-32.1.2::gentoo  USE="..." 333,933 KiB`
/// or, with nothing left to fetch, without the trailing size at all.
pub fn parse_pretend_packages(lines: &[String]) -> Vec<PendingPackage> {
    let mut packages = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("[ebuild") && !trimmed.starts_with("[binary") {
            continue;
        }
        let Some(close) = trimmed.find(']') else { continue };
        let is_new = trimmed[1..close].contains('N');
        let rest = trimmed[close + 1..].trim_start();

        let Some(token) = rest.split_whitespace().next() else { continue };
        let token = token.split("::").next().unwrap_or(token);
        let Some((category, name_version)) = token.split_once('/') else { continue };
        let name_version = name_version.split(':').next().unwrap_or(name_version);
        let (name, version) = split_name_version(name_version);
        if name.is_empty() || version.is_empty() {
            continue;
        }

        let download_kib = rest
            .rsplit_once(" KiB")
            .and_then(|(before, _)| before.rsplit(char::is_whitespace).next())
            .and_then(parse_kib);

        packages.push(PendingPackage {
            atom: format!("{category}/{name}"),
            version,
            download_kib,
            is_new,
        });
    }
    packages
}

/// The packages whose own rebuild tends to cascade into rebuilding
/// everything else on the system (a `gcc`/`glibc` bump commonly triggers
/// a `@preserved-rebuild` of every package linked against the old
/// `libstdc++`/libc, on top of the toolchain package's own build time) —
/// confirmed against this tree's actual atoms rather than assumed, since
/// Gentoo has moved some of these between categories over the years
/// (`llvm-core/llvm`, not `sys-devel/llvm`, on a current profile).
const TOOLCHAIN_ATOMS: &[&str] =
    &["sys-devel/gcc", "sys-libs/glibc", "sys-devel/binutils", "sys-libs/musl", "llvm-core/llvm", "llvm-core/clang"];

/// Whether this run's package list includes a toolchain component — the
/// "why is it rebuilding my whole system" surprise a plain package count
/// doesn't warn about. Checked against the full pending list rather than
/// only `[ebuild ...]` lines: even a `[binary ...]` toolchain swap is
/// still the kind of change worth calling out before it happens, not
/// just a compile-from-source one.
pub fn touches_toolchain(pending: &[PendingPackage]) -> Option<&'static str> {
    pending.iter().find_map(|pkg| TOOLCHAIN_ATOMS.iter().find(|&&atom| atom == pkg.atom).copied())
}

/// As `touches_toolchain`, but against a bare atom list rather than full
/// `PendingPackage` data — for a pre-flight check at the moment "Update
/// All" is clicked, when only `known_atoms` (the atom list, not a fresh
/// `--pretend` run's parsed output) is available yet.
pub fn atoms_touch_toolchain(atoms: &[String]) -> Option<&'static str> {
    atoms.iter().find_map(|atom| TOOLCHAIN_ATOMS.iter().find(|&&t| t == atom).copied())
}

/// Splits a `name-version` token (already stripped of its `category/`
/// prefix and `::repo`/`:slot` suffix) apart at the last `-` followed by a
/// digit — the same rule Portage itself uses, since package names can
/// contain digits and hyphens too (`dev-lang/python-3.14` vs.
/// `x11-terms/alacritty-0.16.1`).
fn split_name_version(name_version: &str) -> (String, String) {
    let parts: Vec<&str> = name_version.split('-').collect();
    for i in (1..parts.len()).rev() {
        if parts[i].starts_with(|c: char| c.is_ascii_digit()) {
            return (parts[..i].join("-"), parts[i..].join("-"));
        }
    }
    (name_version.to_string(), String::new())
}

/// Reads emerge's `88,689 KiB` into a number. Portage groups thousands with
/// commas, which no numeric parser accepts as-is.
fn parse_kib(text: &str) -> Option<u64> {
    let digits: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Renders a size the way a download dialog would, rather than passing
/// portage's raw "88,689 KiB" through to the user.
pub fn format_size_kib(kib: u64) -> String {
    const MIB: f64 = 1024.0;
    const GIB: f64 = 1024.0 * 1024.0;
    let kib = kib as f64;
    if kib >= GIB {
        format!("{:.1} GiB", kib / GIB)
    } else if kib >= MIB {
        format!("{:.0} MiB", kib / MIB)
    } else {
        format!("{kib:.0} KiB")
    }
}

/// Pulls `(atom, flag, enabled)` triples out of a failed run's output —
/// the same information `emerge --autounmask-write` would apply itself,
/// from a block shaped like:
///
/// ```text
/// The following USE changes are necessary to proceed:
///  (see "package.use" in the portage(5) man page for more details)
/// # required by games-emulation/ppsspp-1.20.4-r1::gentoo
/// # required by games-emulation/ppsspp (argument)
/// >=media-libs/libsdl2-2.32.8 X opengl
/// ```
///
/// Applied through `package_use::set_flag` instead of letting portage's own
/// `--autounmask-write` touch the file directly: CONFIG_PROTECT treats an
/// automated rewrite of a file that already has content as something
/// needing manual review (`etc-update`) rather than applying it — fine for
/// a file a human edits, but `zz-portage-store` only ever has GUI-written
/// content already, so the "needs manual review" step just means "install"
/// silently does nothing a second time in a row.
pub fn parse_required_use_changes(lines: &[String]) -> Vec<(String, String, bool)> {
    extract_change_block(lines, "The following USE changes are necessary")
        .into_iter()
        .flat_map(|(atom, tokens)| {
            tokens.into_iter().map(move |tok| match tok.strip_prefix('-') {
                Some(flag) => (atom.clone(), flag.to_string(), false),
                None => (atom.clone(), tok, true),
            })
        })
        .collect()
}

/// Pulls `(atom, keyword)` pairs out of a failed run's output — the
/// keyword-mask counterpart to `parse_required_use_changes`, from a block
/// shaped identically except for its header:
///
/// ```text
/// The following keyword changes are necessary to proceed:
///  (see "package.accept_keywords" in the portage(5) man page for more details)
/// # required by www-client/some-new-browser (argument)
/// >=www-client/some-new-browser-140.0 ~amd64
/// ```
///
/// Every line in this block carries exactly one keyword token (unlike the
/// USE and license blocks, which can list several), so each atom
/// contributes at most one pair.
pub fn parse_required_keyword_changes(lines: &[String]) -> Vec<(String, String)> {
    extract_change_block(lines, "The following keyword changes are necessary")
        .into_iter()
        .filter_map(|(atom, mut tokens)| (!tokens.is_empty()).then(|| (atom, tokens.remove(0))))
        .collect()
}

/// Pulls `(atom, licenses)` pairs out of a failed run's output — the
/// license-mask counterpart to `parse_required_use_changes`. Unlike
/// keywords, a package can need more than one license accepted at once
/// (e.g. a firmware package needing both `linux-fw-redistributable` and
/// `no-source-code`), so each atom keeps its full token list rather than
/// being flattened to one pair per token.
pub fn parse_required_license_changes(lines: &[String]) -> Vec<(String, Vec<String>)> {
    extract_change_block(lines, "The following license changes are necessary")
}

/// Shared mechanics behind `parse_required_use_changes`,
/// `parse_required_keyword_changes`, and `parse_required_license_changes`
/// — portage prints all three "you need to relax something to proceed"
/// blocks in the exact same shape, differing only in their header text:
///
/// ```text
/// The following <X> changes are necessary to proceed:
///  (see "<file>" in the portage(5) man page for more details)
/// # required by <dep chain, one #-prefixed line per link>
/// <atom> <token> [<token> ...]
/// ```
///
/// `header_marker` is matched as a substring (not the full line) since
/// portage colorizes this text in a real terminal, wrapping it in escape
/// codes this app's captured output already has stripped, but being
/// tolerant of variation here costs nothing and avoids depending on
/// exactly where the sentence is cut off.
fn extract_change_block(lines: &[String], header_marker: &str) -> Vec<(String, Vec<String>)> {
    let mut result = Vec::new();
    let mut in_block = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.contains(header_marker) {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("Use --autounmask") {
            break;
        }
        // `#` lines are the "required by ..." dependency-chain context
        // portage prints above each atom; the `(see "..." in ...)` line
        // is a fixed hint always printed right after the block's header
        // — neither is an atom+tokens line.
        if trimmed.starts_with('#') || trimmed.starts_with('(') {
            continue;
        }
        let mut tokens = trimmed.split_whitespace();
        let Some(atom) = tokens.next() else { continue };
        result.push((atom.to_string(), tokens.map(str::to_string).collect()));
    }
    result
}

/// Pulls `(atom, flag, enabled)` triples out of portage's own circular-
/// dependency solver output — the resolver already works out which USE
/// flag flip(s) on which package would break the cycle (see
/// `_emerge/resolver/circular_dependency.py`'s `circular_dependency_handler`
/// upstream) and prints them as suggestions rather than leaving a human to
/// puzzle it out; this reads those back out instead of re-deriving them.
/// One line per suggested change, shaped like:
///
/// ```text
/// It might be possible to break this cycle
/// by applying any of the following changes:
/// - kde-frameworks/kross-6.6.0 (Change USE: +qml -designer)
/// - kde-frameworks/threadweaver-6.6.0 (Change USE: +test)
/// ```
///
/// When portage offers more than one independent way to break the same
/// cycle (as above — either package's own change would do), only the
/// *first* is returned: same "just apply what's needed and retry once"
/// treatment as `parse_required_use_changes`, not a picker between
/// alternatives.
pub fn parse_circular_dependency_use_changes(lines: &[String]) -> Vec<(String, String, bool)> {
    for line in lines {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("- ") else { continue };
        let Some(paren) = rest.find(" (Change USE: ") else { continue };
        let cpv = &rest[..paren];
        let atom = strip_pkg_version(cpv);
        let Some(flags_start) = rest.find("Change USE: ") else { continue };
        let flags_text = rest[flags_start + "Change USE: ".len()..].trim_end_matches(')');
        let changes: Vec<(String, String, bool)> = flags_text
            .split_whitespace()
            .filter_map(|tok| match tok.strip_prefix('-') {
                Some(flag) => Some((atom.clone(), flag.to_string(), false)),
                None => tok.strip_prefix('+').map(|flag| (atom.clone(), flag.to_string(), true)),
            })
            .collect();
        if !changes.is_empty() {
            return changes;
        }
    }
    Vec::new()
}

/// `category/name-version` down to `category/name` — same "walk back from
/// the end until a component looks like a version" logic
/// `ui::strip_version_suffix` already uses for display names, duplicated
/// here in the parsing layer rather than made `pub` there since this is
/// feeding `package_use::set_flag`, not a label.
fn strip_pkg_version(cpv: &str) -> String {
    let Some((category, pf)) = cpv.split_once('/') else {
        return cpv.to_string();
    };
    let parts: Vec<&str> = pf.split('-').collect();
    for i in (1..parts.len()).rev() {
        if parts[i].starts_with(|c: char| c.is_ascii_digit()) {
            return format!("{category}/{}", parts[..i].join("-"));
        }
    }
    cpv.to_string()
}

/// Pulls the atoms (not full `category/name-version`, so they're directly
/// re-installable) out of `--keep-going`'s own end-of-run summary — the
/// list of everything that failed while the rest of a big update kept
/// going around it. Verified against portage's own source
/// (`_emerge/Scheduler.py`'s `_failed_pkg_msg`/`_choose_pkg` handling)
/// for the exact shape, since it's easy to get a rarely-triggered error
/// path wrong by guessing:
///
/// ```text
///  * The following 2 packages have failed to build, install, or execute postinst:
///  *
///  *  media-video/ffmpeg-6.0, Log file:
///  *   '/var/tmp/portage/media-video/ffmpeg-6.0/temp/build.log'
///  *  dev-libs/foo-1.0
///  *
/// ```
///
/// Every line here already carries portage's own `" * "` prefix
/// (colorized in a real terminal, plain here since output is piped) —
/// stripped before matching, same as `extract_change_block` does for its
/// own blocks.
pub fn parse_failed_packages(lines: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut in_block = false;
    for line in lines {
        if line.contains("have failed to build, install, or execute postinst")
            || line.contains("has failed to build, install, or execute postinst")
        {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        let content = line.trim_start().trim_start_matches('*').trim();
        if content.is_empty() {
            // The header itself is immediately followed by one blank
            // separator line before the package list even starts — only
            // a blank line *after* at least one package's been
            // collected actually marks the end of the block.
            if result.is_empty() {
                continue;
            }
            break;
        }
        // The log-file path continuation line, quoted and indented one
        // level deeper than the package line itself — not a package.
        if content.starts_with('\'') {
            continue;
        }
        let atom_version = content.split(',').next().unwrap_or(content).split(" (").next().unwrap_or(content).trim();
        result.push(strip_pkg_version(atom_version));
    }
    result
}

/// Boils a failed `--pretend` run down to one plain-language sentence.
/// Portage's own diagnostics are long and jargon-heavy; these are the three
/// causes a non-expert actually hits, and each has a different fix.
pub fn failure_reason(lines: &[String]) -> String {
    let joined = lines.join("\n");
    if joined.contains("REQUIRED_USE") {
        "USE flags need to be enabled".to_string()
    } else if joined.contains("USE changes are necessary") {
        "Dependencies need USE flag changes".to_string()
    } else if joined.contains("circular dependencies") {
        "Circular dependency".to_string()
    } else if joined.contains("keyword changes are necessary") {
        "Needs an unstable version allowed".to_string()
    } else if joined.contains("masked by") || joined.contains("have been masked") {
        "Package is masked".to_string()
    } else if joined.contains("Blocked") || joined.contains("blocks ") {
        "Conflicts with another package".to_string()
    } else {
        "Could not determine".to_string()
    }
}

/// One `[blocks B]` line from a `--pretend`/install run — a package
/// whose install/removal conflicts with another package already in the
/// merge list. Portage's own diagnostics for this are famously terse
/// ("blocks" and a bare atom, no explanation of what to actually do), so
/// this exists to turn the raw line into "A wants X, B wants Y — you
/// need to pick one" plain language instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockerInfo {
    /// The atom that's in conflict.
    pub atom: String,
    /// A hard block can't be worked around by any USE/keyword/mask
    /// change — the two packages are flatly incompatible and one has to
    /// go. A soft block can sometimes be resolved by portage itself
    /// (with `--backtrack`) or by adjusting what's asked for.
    pub hard: bool,
    /// The package(s) whose own dependency requirements are what's
    /// pulling `atom` into conflict.
    pub wanted_by: Vec<String>,
}

/// Parses every blocker line out of a run's output. Exact format
/// (confirmed against portage's own `_emerge/resolver/output.py`):
///
/// ```text
/// [blocks B      ] media-video/libav ("media-video/libav" is hard blocking media-video/ffmpeg-6.0)
/// ```
///
/// or, when the resolved atom matches the blocker atom exactly (the
/// common case), the quoted atom repetition is omitted:
///
/// ```text
/// [blocks B      ] media-video/libav (is hard blocking media-video/ffmpeg-6.0)
/// ```
pub fn parse_blockers(lines: &[String]) -> Vec<BlockerInfo> {
    lines.iter().filter_map(|line| parse_blocker_line(line.trim_start())).collect()
}

fn parse_blocker_line(trimmed: &str) -> Option<BlockerInfo> {
    let rest = trimmed.strip_prefix("[blocks")?;
    let (_, rest) = rest.split_once(']')?;
    let rest = rest.trim_start();
    let paren_start = rest.find('(')?;
    let atom = rest[..paren_start].trim();
    if atom.is_empty() {
        return None;
    }
    let paren_end = rest.rfind(')')?;
    let inner = &rest[paren_start + 1..paren_end];

    // `inner` is either `"<atom>" is <hard|soft> blocking <parents>` or
    // `is <hard|soft> blocking <parents>` (when the resolved atom matches
    // the blocker atom exactly, the quoted repetition is omitted, so
    // there's no leading `" is "` to split on — just an `"is "` prefix).
    let after_is = match inner.split_once(" is ") {
        Some((_, rest)) => rest,
        None => inner.strip_prefix("is ")?,
    };
    let hard = after_is.starts_with("hard blocking");
    let after_blocking = after_is.trim_start_matches("hard blocking").trim_start_matches("soft blocking").trim();
    if after_blocking.is_empty() {
        return None;
    }
    let wanted_by = after_blocking.split(", ").map(str::to_string).collect();

    Some(BlockerInfo { atom: atom.to_string(), hard, wanted_by })
}

/// What a real (not USE-flag-fixable, not a resolver failure) build
/// failure looks like — the package/phase that actually died and, if
/// portage said so, where the full build log lives. Parsed straight out
/// of the job's own captured output, which always ends with a fixed-shape
/// block once an ebuild phase dies:
///
/// ```text
///  * ERROR: dev-ruby/rbs-3.8.1::gentoo failed (compile phase):
///  *   emake failed
///  *
///  * The complete build log is located at '/var/tmp/portage/.../build.log'.
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildFailure {
    pub atom: String,
    pub phase: String,
    pub build_log_path: Option<String>,
}

/// Parses a `BuildFailure` out of a failed job's captured output lines.
/// Returns `None` for failures this doesn't recognize as "an ebuild phase
/// actually died" (a resolver failure, a USE-flag block, a network
/// error before any phase ran) — those are handled elsewhere
/// (`failure_reason`, `parse_required_use_changes`) or just don't have a
/// build log to point at.
pub fn parse_build_failure(lines: &[String]) -> Option<BuildFailure> {
    let error_line = lines.iter().find(|l| l.trim_start().starts_with("* ERROR:") || l.trim_start().starts_with("*ERROR:"))?;
    let rest = error_line.trim_start().trim_start_matches('*').trim_start().trim_start_matches("ERROR:").trim();
    // `<atom>::<repo> failed (<phase> phase):` — the repo suffix and
    // trailing colon aren't wanted in the atom shown to the user.
    let (atom_and_repo, phase) = rest.split_once("failed (")?;
    let atom = atom_and_repo.split_once("::").map(|(a, _)| a).unwrap_or(atom_and_repo).trim().to_string();
    let phase = phase.trim_end_matches([')', ':']).trim_end_matches(" phase").trim().to_string();

    let build_log_path = lines.iter().find_map(|l| {
        let l = l.trim_start().trim_start_matches('*').trim();
        let rest = l.strip_prefix("The complete build log is located at '")?;
        rest.strip_suffix("'.").map(str::to_string)
    });

    Some(BuildFailure { atom, phase, build_log_path })
}

/// Reads a build log at `path` — `PORTAGE_TMPDIR` (`/var/tmp/portage` by
/// default) is `portage:portage`-owned and not world-readable, so a plain
/// read fails for a user who isn't in that group. Falls back to
/// `doas priv-helper cat-log` (scoped by the helper to `/var/tmp/portage`)
/// rather than silently giving up and showing nothing.
pub fn read_build_log(path: &str) -> Result<String, String> {
    if let Ok(text) = std::fs::read_to_string(path) {
        return Ok(text);
    }
    let output = std::process::Command::new("doas")
        .arg(HELPER_PATH)
        .arg("cat-log")
        .arg(path)
        .output()
        .map_err(|e| format!("failed to launch doas: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Removes the given atom.
pub fn uninstall_job(atom: &str) -> Job {
    Job {
        privileged: true,
        binary: "emerge".into(),
        args: vec!["--ask=n".into(), "--depclean".into(), atom.into()],
        jobs_override: None,
    }
}

/// A full `@world` update. Runs with `--keep-going`: without it, one
/// package failing partway through a long update aborts the entire
/// remaining merge list — everything already resolved and ready to go
/// gets thrown away over a single unrelated failure. With it, portage
/// recalculates around the failure and keeps merging everything else,
/// reporting which package(s) failed at the end (see
/// `parse_failed_packages`) instead of stopping cold.
pub fn update_world_job(getbinpkg: bool) -> Job {
    let mut args = vec!["--ask=n".into(), "--update".into(), "--deep".into(), "--newuse".into(), "--keep-going".into()];
    args.extend(binpkg_args(getbinpkg));
    args.push("@world".into());
    Job { privileged: true, binary: "emerge".into(), args, jobs_override: None }
}

/// Dry-run `@world` update, used to compute the pending-updates list. Runs
/// unprivileged since `--pretend` never touches the system.
pub fn pretend_world_job(getbinpkg: bool) -> Job {
    let mut args = vec!["--pretend".into(), "--update".into(), "--deep".into(), "--newuse".into()];
    args.extend(binpkg_args(getbinpkg));
    args.push("@world".into());
    Job { privileged: false, binary: "emerge".into(), args, jobs_override: None }
}

/// Whether the package ships an already-compiled binary rather than sources.
/// Portage still lists these as `[ebuild]` because they are ordinary ebuilds
/// — they just unpack an upstream build — so calling them "compiled from
/// source" in the UI would be wrong.
pub fn is_prebuilt(name: &str) -> bool {
    name.ends_with("-bin")
}

/// Extracts "how far along is this emerge" from its output. Portage
/// reports progress in two shapes depending on whether the merge runs
/// jobs in parallel, and a run emits both, so both are recognised:
/// `>>> Jobs: 10 of 28 complete` and `>>> Emerging (3 of 28) cat/pkg` —
/// the second of which also carries a package name, which is what turns
/// "3 / 28" into "3 of 28 — dev-libs/boost", the difference between a
/// bare progress bar and something that reads like a store's own install
/// screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepProgress {
    pub done: u32,
    pub total: u32,
    /// `None` for the `>>> Jobs: N of M` summary line, which reports no
    /// package of its own — only the `>>> Emerging`/`>>> Installing`
    /// per-package lines do.
    pub atom: Option<String>,
}

pub fn parse_step_progress(line: &str) -> Option<StepProgress> {
    let counts = |text: &str| -> Option<(u32, u32)> {
        let mut parts = text.split_whitespace();
        let done = parts.next()?.parse().ok()?;
        if parts.next()? != "of" {
            return None;
        }
        let total: u32 = parts.next()?.parse().ok()?;
        (total > 0).then_some((done, total))
    };

    if let Some(rest) = line.trim_start().strip_prefix(">>> Jobs:") {
        let (done, total) = counts(rest.trim_start())?;
        return Some(StepProgress { done, total, atom: None });
    }

    // ">>> Emerging (3 of 28) www-client/firefox-140.12.0"
    let trimmed = line.trim_start();
    if trimmed.starts_with(">>>") {
        let open = trimmed.find('(')?;
        let close = trimmed[open..].find(')')? + open;
        let (done, total) = counts(&trimmed[open + 1..close])?;
        let atom = trimmed[close + 1..].trim();
        let atom = (!atom.is_empty()).then(|| atom.to_string());
        return Some(StepProgress { done, total, atom });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidating_a_cached_pretend_clears_it() {
        store_pretend("dev-test/invalidate-me", "1.0", true, vec!["line".to_string()]);
        assert!(cached_pretend("dev-test/invalidate-me", "1.0").is_some());
        invalidate_pretend("dev-test/invalidate-me");
        assert!(cached_pretend("dev-test/invalidate-me", "1.0").is_none());
    }

    #[test]
    fn privileged_jobs_render_with_doas_priv_helper_run_first() {
        let job = Job {
            privileged: true,
            binary: "emerge".to_string(),
            args: vec!["--ask=n".to_string(), "www-client/firefox".to_string()],
            jobs_override: None,
        };
        assert_eq!(job.to_shell_command(), format!("doas {HELPER_PATH} run -- emerge --ask=n www-client/firefox"));
    }

    #[test]
    fn a_jobs_override_renders_as_a_run_flag() {
        let job = Job { privileged: true, binary: "emerge".to_string(), args: vec!["@world".to_string()], jobs_override: Some(4) };
        assert_eq!(job.to_shell_command(), format!("doas {HELPER_PATH} run --jobs 4 -- emerge @world"));
    }

    #[test]
    fn sandbox_build_jobs_render_with_their_own_subcommand() {
        let job = Job { privileged: true, binary: "sandbox-build".to_string(), args: vec!["dev-libs/foo".to_string()], jobs_override: None };
        assert_eq!(job.to_shell_command(), format!("doas {HELPER_PATH} sandbox-build dev-libs/foo"));
    }

    #[test]
    fn unprivileged_jobs_have_no_doas_prefix() {
        let job = Job {
            privileged: false,
            binary: "emerge".to_string(),
            args: vec!["--pretend".to_string(), "www-client/firefox".to_string()],
            jobs_override: None,
        };
        assert_eq!(job.to_shell_command(), "emerge --pretend www-client/firefox");
    }

    #[test]
    fn args_needing_quoting_are_quoted_but_plain_ones_are_not() {
        let job = Job {
            privileged: false,
            binary: "flatpak".to_string(),
            args: vec!["install".to_string(), "--user".to_string(), "MAKEOPTS=-j4 -l4".to_string()],
            jobs_override: None,
        };
        assert_eq!(job.to_shell_command(), "flatpak install --user 'MAKEOPTS=-j4 -l4'");
    }

    #[test]
    fn a_literal_single_quote_in_an_argument_is_escaped_correctly() {
        let job = Job { privileged: false, binary: "echo".to_string(), args: vec!["it's here".to_string()], jobs_override: None };
        assert_eq!(job.to_shell_command(), r"echo 'it'\''s here'");
    }

    #[test]
    fn parses_a_hard_blocker_with_the_quoted_atom_repeated() {
        let lines = vec![
            "[ebuild  N     ] media-video/ffmpeg-6.0::gentoo".to_string(),
            "[blocks B      ] media-video/libav (\"media-video/libav\" is hard blocking media-video/ffmpeg-6.0)".to_string(),
        ];
        let blockers = parse_blockers(&lines);
        assert_eq!(
            blockers,
            vec![BlockerInfo {
                atom: "media-video/libav".to_string(),
                hard: true,
                wanted_by: vec!["media-video/ffmpeg-6.0".to_string()],
            }]
        );
    }

    #[test]
    fn parses_a_blocker_without_the_quoted_atom_repeated() {
        let lines = vec!["[blocks B      ] media-video/libav (is soft blocking media-video/ffmpeg-6.0)".to_string()];
        let blockers = parse_blockers(&lines);
        assert_eq!(blockers, vec![BlockerInfo {
            atom: "media-video/libav".to_string(),
            hard: false,
            wanted_by: vec!["media-video/ffmpeg-6.0".to_string()],
        }]);
    }

    #[test]
    fn multiple_parents_are_all_captured() {
        let lines = vec![
            "[blocks B      ] dev-libs/foo (is hard blocking dev-libs/bar-1.0, dev-libs/baz-2.0)".to_string(),
        ];
        let blockers = parse_blockers(&lines);
        assert_eq!(blockers[0].wanted_by, vec!["dev-libs/bar-1.0".to_string(), "dev-libs/baz-2.0".to_string()]);
    }

    #[test]
    fn non_blocker_lines_are_ignored() {
        assert_eq!(parse_blockers(&["[ebuild  N     ] dev-libs/foo-1.0".to_string()]), Vec::new());
    }

    #[test]
    fn parses_a_real_build_failure() {
        let lines: Vec<String> = "\
 * ERROR: dev-ruby/rbs-3.8.1::gentoo failed (compile phase):
 *   emake failed
 *
 * If you need support, post the output of `emerge --info '=dev-ruby/rbs-3.8.1::gentoo'`,
 * the complete build log and the output of `emerge -pqv '=dev-ruby/rbs-3.8.1::gentoo'`.
 * The complete build log is located at '/var/tmp/portage/dev-ruby/rbs-3.8.1/temp/build.log'.
 * The ebuild environment file is located at '/var/tmp/portage/dev-ruby/rbs-3.8.1/temp/environment'.
"
        .lines()
        .map(str::to_string)
        .collect();

        let failure = parse_build_failure(&lines).unwrap();
        assert_eq!(failure.atom, "dev-ruby/rbs-3.8.1");
        assert_eq!(failure.phase, "compile");
        assert_eq!(failure.build_log_path.as_deref(), Some("/var/tmp/portage/dev-ruby/rbs-3.8.1/temp/build.log"));
    }

    #[test]
    fn no_error_marker_means_no_build_failure() {
        let lines = vec!["Calculating dependencies... done!".to_string(), "nothing to merge".to_string()];
        assert_eq!(parse_build_failure(&lines), None);
    }

    fn progress_counts(line: &str) -> Option<(u32, u32)> {
        parse_step_progress(line).map(|s| (s.done, s.total))
    }

    #[test]
    fn reads_parallel_job_progress() {
        assert_eq!(progress_counts(">>> Jobs: 10 of 28 complete, 1 running"), Some((10, 28)));
        assert_eq!(progress_counts(">>> Jobs: 0 of 28 complete"), Some((0, 28)));
    }

    #[test]
    fn reads_sequential_merge_progress() {
        assert_eq!(progress_counts(">>> Emerging (3 of 28) www-client/firefox-140.12.0::gentoo"), Some((3, 28)));
        assert_eq!(progress_counts(">>> Installing (7 of 28) dev-lang/rust-1.94.1"), Some((7, 28)));
    }

    #[test]
    fn step_progress_keeps_the_package_name() {
        let step = parse_step_progress(">>> Emerging (3 of 28) www-client/firefox-140.12.0::gentoo").unwrap();
        assert_eq!(step, StepProgress { done: 3, total: 28, atom: Some("www-client/firefox-140.12.0::gentoo".to_string()) });
    }

    #[test]
    fn the_jobs_summary_line_has_no_package_name() {
        let step = parse_step_progress(">>> Jobs: 10 of 28 complete, 1 running").unwrap();
        assert_eq!(step.atom, None);
    }

    #[test]
    fn ordinary_output_carries_no_progress() {
        assert_eq!(progress_counts("Calculating dependencies  ... done!"), None);
        assert_eq!(progress_counts(">>> Unpacking source..."), None);
        assert_eq!(progress_counts(""), None);
    }

    #[test]
    fn zero_total_is_rejected_not_divided_by() {
        assert_eq!(progress_counts(">>> Jobs: 0 of 0 complete"), None);
    }

    #[test]
    fn download_size_survives_the_thousands_separator() {
        let lines = vec!["Total: 2 packages (1 new), Size of downloads: 895,067 KiB".to_string()];
        assert_eq!(parse_pretend_output(&lines).download_kib, Some(895_067));
    }

    #[test]
    fn required_use_changes_are_parsed_into_atom_flag_pairs() {
        let lines = [
            "These are the packages that would be merged, in order:",
            "",
            "[ebuild  N     ] games-emulation/ppsspp-1.20.4-r1::gentoo  USE=\"wayland\"",
            "",
            "The following USE changes are necessary to proceed:",
            " (see \"package.use\" in the portage(5) man page for more details)",
            "# required by games-emulation/ppsspp-1.20.4-r1::gentoo",
            "# required by games-emulation/ppsspp (argument)",
            ">=media-libs/libsdl2-2.32.8 X opengl",
            "",
            "Use --autounmask-write to write changes to config files (honoring",
            "CONFIG_PROTECT). Carefully examine the list of proposed changes,",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

        assert_eq!(
            parse_required_use_changes(&lines),
            vec![
                (">=media-libs/libsdl2-2.32.8".to_string(), "X".to_string(), true),
                (">=media-libs/libsdl2-2.32.8".to_string(), "opengl".to_string(), true),
            ]
        );
    }

    #[test]
    fn required_keyword_changes_are_parsed() {
        let lines = [
            "The following keyword changes are necessary to proceed:",
            " (see \"package.accept_keywords\" in the portage(5) man page for more details)",
            "# required by www-client/some-new-browser (argument)",
            ">=www-client/some-new-browser-140.0 ~amd64",
            "",
            "Use --autounmask-write to write changes to config files (honoring",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

        assert_eq!(
            parse_required_keyword_changes(&lines),
            vec![(">=www-client/some-new-browser-140.0".to_string(), "~amd64".to_string())]
        );
    }

    #[test]
    fn required_license_changes_keep_every_token_per_atom() {
        let lines = [
            "The following license changes are necessary to proceed:",
            " (see \"package.license\" in the portage(5) man page for more details)",
            "# required by sys-kernel/linux-firmware-20260519 (argument)",
            ">=sys-kernel/linux-firmware-20260519 linux-fw-redistributable no-source-code",
            "",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

        assert_eq!(
            parse_required_license_changes(&lines),
            vec![(
                ">=sys-kernel/linux-firmware-20260519".to_string(),
                vec!["linux-fw-redistributable".to_string(), "no-source-code".to_string()]
            )]
        );
    }

    #[test]
    fn sequential_blocks_do_not_bleed_into_each_other() {
        // The real shape when more than one kind of change is needed at
        // once: each block is its own blank-line-separated section, in
        // keyword/mask/USE/license order — parsing one block's kind must
        // stop at the boundary, not swallow the next header as if it
        // were an atom line.
        let lines = [
            "The following keyword changes are necessary to proceed:",
            " (see \"package.accept_keywords\" in the portage(5) man page for more details)",
            ">=www-client/some-new-browser-140.0 ~amd64",
            "",
            "The following license changes are necessary to proceed:",
            " (see \"package.license\" in the portage(5) man page for more details)",
            ">=www-client/some-new-browser-140.0 EULA",
            "",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

        assert_eq!(
            parse_required_keyword_changes(&lines),
            vec![(">=www-client/some-new-browser-140.0".to_string(), "~amd64".to_string())]
        );
        assert_eq!(
            parse_required_license_changes(&lines),
            vec![(">=www-client/some-new-browser-140.0".to_string(), vec!["EULA".to_string()])]
        );
    }

    #[test]
    fn required_use_changes_handles_disabled_flags_and_multiple_atoms() {
        let lines = [
            "The following USE changes are necessary to proceed:",
            " (see \"package.use\" in the portage(5) man page for more details)",
            "# required by dev-libs/foo-1.0::gentoo",
            "media-libs/bar -static-libs",
            "dev-libs/baz X",
            "",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

        assert_eq!(
            parse_required_use_changes(&lines),
            vec![
                ("media-libs/bar".to_string(), "static-libs".to_string(), false),
                ("dev-libs/baz".to_string(), "X".to_string(), true),
            ]
        );
    }

    #[test]
    fn no_required_use_changes_block_yields_nothing() {
        let lines = vec!["Some unrelated failure.".to_string(), "!!! Blocked packages".to_string()];
        assert!(parse_required_use_changes(&lines).is_empty());
    }

    #[test]
    fn circular_dependency_suggestion_is_parsed_into_atom_flag_pairs() {
        // Verbatim shape (colors stripped) from portage's own
        // `circular_dependency_handler` — see
        // `_emerge/resolver/circular_dependency.py` upstream.
        let lines = vec![
            "It might be possible to break this cycle".to_string(),
            "by applying any of the following changes:".to_string(),
            "- kde-frameworks/kross-6.6.0 (Change USE: +qml -designer)".to_string(),
            "- kde-frameworks/threadweaver-6.6.0 (Change USE: +test)".to_string(),
        ];
        assert_eq!(
            parse_circular_dependency_use_changes(&lines),
            vec![
                ("kde-frameworks/kross".to_string(), "qml".to_string(), true),
                ("kde-frameworks/kross".to_string(), "designer".to_string(), false),
            ]
        );
    }

    #[test]
    fn a_single_suggested_change_is_parsed_the_same_way() {
        let lines = vec![
            "It might be possible to break this cycle".to_string(),
            "by applying the following change:".to_string(),
            "- dev-libs/foo-1.0 (Change USE: +bar)".to_string(),
        ];
        assert_eq!(
            parse_circular_dependency_use_changes(&lines),
            vec![("dev-libs/foo".to_string(), "bar".to_string(), true)]
        );
    }

    #[test]
    fn no_circular_dependency_suggestion_yields_nothing() {
        let lines = vec!["Some unrelated failure.".to_string()];
        assert!(parse_circular_dependency_use_changes(&lines).is_empty());
    }

    #[test]
    fn strips_versions_with_hyphens_in_the_package_name() {
        assert_eq!(strip_pkg_version("kde-frameworks/kross-6.6.0"), "kde-frameworks/kross");
        assert_eq!(strip_pkg_version("dev-libs/some-lib-r1-2.0-r3"), "dev-libs/some-lib-r1");
    }

    #[test]
    fn keep_going_failure_summary_is_parsed_into_atoms() {
        // Verbatim shape (colors stripped) from portage's own
        // `_emerge/Scheduler.py` end-of-run `--keep-going` summary.
        let lines = [
            " * The following 2 packages have failed to build, install, or execute postinst:",
            " * ",
            " *  media-video/ffmpeg-6.0, Log file:",
            " *   '/var/tmp/portage/media-video/ffmpeg-6.0/temp/build.log'",
            " *  dev-libs/foo-1.0",
            " * ",
        ]
        .into_iter()
        .map(String::from)
        .collect::<Vec<_>>();

        assert_eq!(parse_failed_packages(&lines), vec!["media-video/ffmpeg".to_string(), "dev-libs/foo".to_string()]);
    }

    #[test]
    fn a_single_failed_package_uses_the_singular_header() {
        let lines = [" * The following package has failed to build, install, or execute postinst:", " * ", " *  dev-libs/foo-1.0", " * "]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        assert_eq!(parse_failed_packages(&lines), vec!["dev-libs/foo".to_string()]);
    }

    #[test]
    fn a_postinst_failure_marker_does_not_get_included_in_the_atom() {
        let lines = [" * The following package has failed to build, install, or execute postinst:", " * ", " *  dev-libs/foo-1.0 (postinst failed)", " * "]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        assert_eq!(parse_failed_packages(&lines), vec!["dev-libs/foo".to_string()]);
    }

    #[test]
    fn no_keep_going_summary_yields_nothing() {
        let lines = vec!["Some unrelated failure.".to_string()];
        assert!(parse_failed_packages(&lines).is_empty());
    }

    #[test]
    fn sizes_render_in_binary_units() {
        assert_eq!(format_size_kib(88_689), "87 MiB");
        assert_eq!(format_size_kib(512), "512 KiB");
        assert_eq!(format_size_kib(2_500_000), "2.4 GiB");
    }

    #[test]
    fn per_package_lines_are_parsed_with_size() {
        let lines = vec![
            r#"[ebuild   R   ~] media-video/obs-studio-32.1.2::gentoo  USE="alsa browser" 333,933 KiB"#.to_string(),
        ];
        let packages = parse_pretend_packages(&lines);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].atom, "media-video/obs-studio");
        assert_eq!(packages[0].version, "32.1.2");
        assert_eq!(packages[0].download_kib, Some(333_933));
        assert!(!packages[0].is_new);
    }

    #[test]
    fn new_dependency_lines_are_marked_new_and_size_extracted() {
        let lines = vec![r#"[ebuild  N     ] app-text/poppler-data-0.4.12::gentoo  4,403 KiB"#.to_string()];
        let packages = parse_pretend_packages(&lines);
        assert_eq!(packages[0].atom, "app-text/poppler-data");
        assert_eq!(packages[0].version, "0.4.12");
        assert_eq!(packages[0].download_kib, Some(4_403));
        assert!(packages[0].is_new);
    }

    #[test]
    fn lines_with_no_trailing_size_still_parse_the_atom() {
        let lines = vec![r#"[ebuild   R    ] sys-apps/sed-4.9::gentoo  USE="nls -static""#.to_string()];
        let packages = parse_pretend_packages(&lines);
        assert_eq!(packages[0].atom, "sys-apps/sed");
        assert_eq!(packages[0].version, "4.9");
        assert_eq!(packages[0].download_kib, None);
    }

    #[test]
    fn slotted_atoms_drop_the_slot_suffix() {
        let lines = vec![
            r#"[ebuild  N     ] media-libs/libmypaint-1.6.1-r3:0/0.0.0::gentoo  USE="nls" 508 KiB"#.to_string(),
        ];
        let packages = parse_pretend_packages(&lines);
        assert_eq!(packages[0].atom, "media-libs/libmypaint");
        assert_eq!(packages[0].version, "1.6.1-r3");
    }

    #[test]
    fn non_package_lines_are_ignored() {
        let lines = vec![
            "Calculating dependencies  ... done!".to_string(),
            "Total: 2 packages (1 new), Size of downloads: 895,067 KiB".to_string(),
        ];
        assert!(parse_pretend_packages(&lines).is_empty());
    }

    #[test]
    fn live_ebuild_version_9999_splits_correctly() {
        let lines = vec![r#"[ebuild   R    ] app-editors/neovim-9999::gentoo  120 KiB"#.to_string()];
        let packages = parse_pretend_packages(&lines);
        assert_eq!(packages[0].atom, "app-editors/neovim");
        assert_eq!(packages[0].version, "9999");
    }

    #[test]
    fn a_toolchain_rebuild_is_flagged() {
        let lines = vec![
            r#"[ebuild   R    ] app-editors/neovim-0.10.0::gentoo  120 KiB"#.to_string(),
            r#"[ebuild   R    ] sys-devel/gcc-14.2.1_p20241221::gentoo  95,000 KiB"#.to_string(),
        ];
        let packages = parse_pretend_packages(&lines);
        assert_eq!(touches_toolchain(&packages), Some("sys-devel/gcc"));
    }

    #[test]
    fn an_ordinary_update_is_not_flagged_as_a_toolchain_rebuild() {
        let lines = vec![r#"[ebuild   R    ] app-editors/neovim-0.10.0::gentoo  120 KiB"#.to_string()];
        let packages = parse_pretend_packages(&lines);
        assert_eq!(touches_toolchain(&packages), None);
    }

    #[test]
    fn atoms_touch_toolchain_matches_a_bare_atom_list() {
        let atoms = vec!["app-editors/neovim".to_string(), "sys-libs/glibc".to_string()];
        assert_eq!(atoms_touch_toolchain(&atoms), Some("sys-libs/glibc"));
    }

    #[test]
    fn atoms_touch_toolchain_is_none_for_an_ordinary_list() {
        let atoms = vec!["app-editors/neovim".to_string()];
        assert_eq!(atoms_touch_toolchain(&atoms), None);
    }
}
