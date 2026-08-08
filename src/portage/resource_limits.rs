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

/// Wraps `job` to run at reduced CPU/IO scheduling priority (`nice`/
/// `ionice`, idle class — keeps a build from starving whatever else is
/// running) and with `MAKEOPTS` capped to `recommended_jobs()`.
///
/// The `MAKEOPTS` override is threaded through via an explicit `env`
/// prefix baked into the command's own argv, not a `Command::env()` call
/// on the Rust side — `pkexec` sanitizes the environment of whatever it
/// launches, so a variable set only on the local `Command` object would
/// simply never reach the actual `emerge` process for a privileged job.
/// Putting it in argv via `env VAR=value` has no such problem, since it's
/// explicit input to the command chain rather than inherited state.
pub fn throttled(job: Job) -> Job {
    let jobs = recommended_jobs();
    let mut args = vec![
        "-c".to_string(),
        "3".to_string(), // ionice: idle I/O class
        "nice".to_string(),
        "-n".to_string(),
        "19".to_string(), // lowest CPU scheduling priority
        "env".to_string(),
        format!("MAKEOPTS=-j{jobs} -l{jobs}"),
        job.binary,
    ];
    args.extend(job.args);
    Job { privileged: job.privileged, binary: "ionice".to_string(), args }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn throttled_wraps_the_original_command_unchanged_at_the_tail() {
        let job = Job { privileged: true, binary: "emerge".to_string(), args: vec!["--ask=n".to_string(), "www-client/firefox".to_string()] };
        let wrapped = throttled(job);
        assert!(wrapped.privileged);
        assert_eq!(wrapped.binary, "ionice");
        // The original binary+args must still appear, in order, at the
        // tail of the wrapped argv — that's what actually gets run once
        // ionice/nice/env are done setting up.
        let tail = &wrapped.args[wrapped.args.len() - 3..];
        assert_eq!(tail, &["emerge".to_string(), "--ask=n".to_string(), "www-client/firefox".to_string()]);
    }

    #[test]
    fn recommended_jobs_is_never_zero() {
        assert!(recommended_jobs() >= 1);
    }
}
