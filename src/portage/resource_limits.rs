use super::emerge::Job;

/// The classic rule of thumb: budget roughly 2GiB of RAM per parallel
/// compile job. Sizing parallelism only for CPU core count and ignoring
/// memory is exactly what caused a real incident during this app's own
/// development — an unthrottled `-j24` build exhausted RAM and the
/// kernel's OOM killer took out unrelated processes (the user's browser,
/// Steam) along with the build itself.
const RAM_PER_JOB_KIB: u64 = 2 * 1024 * 1024;

pub fn cpu_cores() -> u32 {
    std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1)
}

/// `MemAvailable` from `/proc/meminfo`, in KiB — the kernel's own "how
/// much could actually be allocated right now" estimate (already
/// accounting for reclaimable cache), not `MemFree`, which undercounts by
/// treating reclaimable page cache as unavailable.
pub fn available_ram_kib() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    content
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
}

/// How many parallel compile jobs are safe right now — the tighter of a
/// CPU-core-count bound and a RAM bound, never fewer than 1.
pub fn recommended_jobs() -> u32 {
    let cores = cpu_cores();
    let ram_limit = available_ram_kib().map(|kib| (kib / RAM_PER_JOB_KIB).max(1) as u32).unwrap_or(cores);
    cores.min(ram_limit).max(1)
}

/// Marks `job` to run at reduced CPU/IO scheduling priority with `MAKEOPTS`
/// capped to `recommended_jobs()`, by setting `jobs_override` rather than
/// rewriting `binary`/`args` into an `ionice`/`nice`/`env` chain here.
///
/// That rewrite is what the old `pkexec`-based design did, but it can't
/// survive translation into a small, auditable binary allowlist on the
/// privileged side (the allowlist would have to permit `ionice` with any
/// trailing argv, defeating the point of having one at all). Instead the
/// helper itself (`resources/priv-helper.sh`'s `cmd_run`) builds the fixed
/// `ionice -c3 nice -n19 env MAKEOPTS=...` wrap from this validated
/// integer — never from argv this binary supplied.
pub fn throttled(job: Job) -> Job {
    Job { jobs_override: Some(recommended_jobs()), ..job }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttled_sets_jobs_override_and_leaves_binary_args_untouched() {
        let job = Job {
            privileged: true,
            binary: "emerge".to_string(),
            args: vec!["--ask=n".to_string(), "www-client/firefox".to_string()],
            jobs_override: None,
        };
        let wrapped = throttled(job);
        assert!(wrapped.privileged);
        assert_eq!(wrapped.binary, "emerge");
        assert_eq!(wrapped.args, vec!["--ask=n".to_string(), "www-client/firefox".to_string()]);
        assert!(wrapped.jobs_override.unwrap() >= 1);
    }

    #[test]
    fn recommended_jobs_is_never_zero() {
        assert!(recommended_jobs() >= 1);
    }
}
