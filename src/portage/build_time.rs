use crate::portage::make_conf;

/// Roughly how much compressed source one core chews through in a minute.
///
/// Calibrated against typical Gentoo source builds rather than measured per
/// machine: it is the order of magnitude that matters here, since the point
/// is to tell "a couple of minutes" apart from "most of an evening" before
/// the user commits to an install.
const MIB_PER_CORE_MINUTE: f64 = 1.0;

/// `-bin` packages ship an already-compiled binary, so their "build" is an
/// unpack and this is disk throughput, not compiler throughput.
const UNPACK_MIB_PER_SECOND: f64 = 10.0;

/// How many compile jobs this machine runs at once, taken from MAKEOPTS so
/// the estimate reflects the user's actual configuration rather than the
/// core count they chose not to use.
pub fn parallel_jobs() -> usize {
    let configured = make_conf::read_raw()
        .ok()
        .and_then(|raw| make_conf::parse_vars(&raw).get("MAKEOPTS").cloned())
        .and_then(|makeopts| parse_jobs(&makeopts));

    configured
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1)
        .max(1)
}

/// Pulls the `N` out of a MAKEOPTS string like `-j24 -l24`.
fn parse_jobs(makeopts: &str) -> Option<usize> {
    for token in makeopts.split_whitespace() {
        if let Some(value) = token.strip_prefix("-j") {
            if value.is_empty() {
                continue;
            }
            return value.parse().ok();
        }
    }
    None
}

/// Estimated seconds to install a package, from how much there is to
/// compile and how many jobs run in parallel.
///
/// Always an estimate and never a promise: the real figure depends on what
/// the sources actually contain, which no amount of inspecting the tarball
/// size will reveal. It is here so that "this is a 40-minute job" is
/// visible *before* starting, which is the thing worth knowing.
pub fn estimate_seconds(download_kib: u64, prebuilt: bool, jobs: usize) -> u64 {
    let mib = download_kib as f64 / 1024.0;
    let seconds = if prebuilt {
        mib / UNPACK_MIB_PER_SECOND
    } else {
        (mib / (jobs.max(1) as f64 * MIB_PER_CORE_MINUTE)) * 60.0
    };
    (seconds.round() as u64).max(1)
}

pub fn jobs_label(jobs: usize) -> String {
    format!("{jobs} thread{}", if jobs == 1 { "" } else { "s" })
}

pub fn merges_label(merges: u32) -> String {
    format!("{merges} build{}", if merges == 1 { "" } else { "s" })
}

/// Renders a duration the way a person would say it.
pub fn format_duration(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds.div_ceil(60)),
        _ => {
            let hours = seconds / 3600;
            let minutes = (seconds % 3600) / 60;
            if minutes == 0 {
                format!("{hours}h")
            } else {
                format!("{hours}h {minutes}m")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_come_from_makeopts() {
        assert_eq!(parse_jobs("-j24 -l24"), Some(24));
        assert_eq!(parse_jobs("-l8 -j6"), Some(6));
        assert_eq!(parse_jobs("--quiet"), None);
        assert_eq!(parse_jobs("-j"), None);
    }

    #[test]
    fn prebuilt_packages_are_an_unpack_not_a_build() {
        // ~87 MiB of prebuilt browser: seconds, not minutes.
        let seconds = estimate_seconds(88_689, true, 24);
        assert!(seconds < 30, "expected an unpack, got {seconds}s");
    }

    #[test]
    fn large_source_builds_scale_with_job_count() {
        let one_core = estimate_seconds(895_067, false, 1);
        let many_cores = estimate_seconds(895_067, false, 24);
        assert!(many_cores < one_core / 10);
        // ~874 MiB across 24 jobs lands in the tens of minutes.
        assert!((1200..4000).contains(&many_cores), "got {many_cores}s");
    }

    #[test]
    fn estimates_are_never_zero() {
        assert_eq!(estimate_seconds(0, false, 24), 1);
    }

    #[test]
    fn job_counts_pluralize_in_english() {
        assert_eq!(jobs_label(1), "1 thread");
        assert_eq!(jobs_label(4), "4 threads");
        assert_eq!(jobs_label(24), "24 threads");
    }

    #[test]
    fn merge_counts_pluralize_in_english() {
        assert_eq!(merges_label(1), "1 build");
        assert_eq!(merges_label(2), "2 builds");
    }

    #[test]
    fn durations_render_in_human_units() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(600), "10m");
        assert_eq!(format_duration(7200), "2h");
        assert_eq!(format_duration(5400), "1h 30m");
    }
}
