use super::emerge::Job;
use std::time::SystemTime;

/// Where the main repo's own sync timestamp lives — rewritten by
/// `emerge --sync` every time it actually completes, so its mtime is
/// exactly "how long ago the tree was last updated". Hardcoded to the
/// standard location rather than resolved via `portageq`, matching how
/// this app already assumes the default layout elsewhere (`man.rs`,
/// `appstream.rs`) — a non-standard repo layout just means this reports
/// nothing rather than reporting the wrong repo.
const TIMESTAMP_FILE: &str = "/var/db/repos/gentoo/metadata/timestamp.chk";

/// Seconds since the tree was last synced, if it's ever been synced at
/// all. `None` rather than an error — a system that's never run
/// `emerge --sync` (some minimal/embedded setups skip it) isn't broken,
/// it just has nothing to report here.
pub fn seconds_since_last_sync() -> Option<u64> {
    let modified = std::fs::metadata(TIMESTAMP_FILE).ok()?.modified().ok()?;
    SystemTime::now().duration_since(modified).ok().map(|d| d.as_secs())
}

/// A plain-language age — "2 hours ago", "3 weeks ago" — for the one
/// coarsest unit that actually matters; nobody needs "3 weeks, 2 days, 4
/// hours" to know the tree is stale.
pub fn format_age(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;

    let (value, unit) = if seconds < HOUR {
        (seconds / MINUTE, "minute")
    } else if seconds < DAY {
        (seconds / HOUR, "hour")
    } else if seconds < WEEK {
        (seconds / DAY, "day")
    } else {
        (seconds / WEEK, "week")
    };
    let value = value.max(1);
    if value == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{value} {unit}s ago")
    }
}

/// Syncs the package tree. Privileged — writing the tree is a root
/// operation the same way installing a package is.
pub fn sync_job() -> Job {
    Job { privileged: true, binary: "emerge".into(), args: vec!["--sync".into()] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_each_unit_at_its_own_scale() {
        assert_eq!(format_age(30), "1 minute ago");
        assert_eq!(format_age(3 * 60 * 60), "3 hours ago");
        assert_eq!(format_age(2 * 24 * 60 * 60), "2 days ago");
        assert_eq!(format_age(21 * 24 * 60 * 60), "3 weeks ago");
    }

    #[test]
    fn a_value_of_one_is_not_pluralized() {
        assert_eq!(format_age(60 * 60), "1 hour ago");
        assert_eq!(format_age(24 * 60 * 60), "1 day ago");
    }

}
