use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One point-in-time reading of the health dashboard's own checks —
/// enough to answer "has this actually been sitting here a while" rather
/// than only "is it a problem right now". Counts only, not the items
/// themselves: the dashboard's own live checks already re-fetch full
/// detail every time it opens, so history only needs to remember *how
/// many*, not *which*.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub unix_time: u64,
    pub unread_news: usize,
    pub pending_config: usize,
    pub glsa_count: usize,
    pub orphan_count: usize,
}

/// Capped so the history file can't grow without bound on a system left
/// running for years — at one snapshot per dashboard visit (or, with
/// periodic checks on, one every few hours), this is well over a year of
/// history before the oldest entries start rolling off.
const MAX_SNAPSHOTS: usize = 2000;

fn history_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let dir = base.join("portage-store");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("health_history.json"))
}

pub fn history() -> Vec<HealthSnapshot> {
    let Some(path) = history_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Appends one snapshot, timestamped now, and trims to `MAX_SNAPSHOTS`.
pub fn record(unread_news: usize, pending_config: usize, glsa_count: usize, orphan_count: usize) {
    let Some(path) = history_path() else { return };
    let mut entries = history();
    let unix_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    entries.push(HealthSnapshot { unix_time, unread_news, pending_config, glsa_count, orphan_count });
    if entries.len() > MAX_SNAPSHOTS {
        let excess = entries.len() - MAX_SNAPSHOTS;
        entries.drain(..excess);
    }
    if let Ok(text) = serde_json::to_string(&entries) {
        let _ = std::fs::write(path, text);
    }
}

/// How long (in whole days) `metric` has been continuously nonzero,
/// walking backward from the most recent snapshot — the "and it's been 9
/// days" figure. `None` if the metric is currently zero (nothing pending
/// right now, so "how long has it been pending" doesn't apply) or there's
/// no history yet.
pub fn days_pending(history: &[HealthSnapshot], metric: impl Fn(&HealthSnapshot) -> usize) -> Option<u64> {
    let latest = history.last()?;
    if metric(latest) == 0 {
        return None;
    }
    let mut oldest_still_pending = latest;
    for snapshot in history.iter().rev() {
        if metric(snapshot) == 0 {
            break;
        }
        oldest_still_pending = snapshot;
    }
    let seconds = latest.unix_time.saturating_sub(oldest_still_pending.unix_time);
    Some(seconds / (24 * 60 * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(days_ago: u64, glsa: usize) -> HealthSnapshot {
        HealthSnapshot { unix_time: 1_000_000_000 - days_ago * 86_400, unread_news: 0, pending_config: 0, glsa_count: glsa, orphan_count: 0 }
    }

    #[test]
    fn currently_zero_has_no_pending_duration() {
        let history = vec![snap(3, 2), snap(0, 0)];
        assert_eq!(days_pending(&history, |s| s.glsa_count), None);
    }

    #[test]
    fn counts_back_to_the_start_of_the_current_nonzero_run() {
        let history = vec![snap(9, 3), snap(5, 3), snap(2, 1), snap(0, 2)];
        assert_eq!(days_pending(&history, |s| s.glsa_count), Some(9));
    }

    #[test]
    fn a_gap_of_zero_resets_the_run() {
        let history = vec![snap(9, 3), snap(5, 0), snap(2, 1), snap(0, 2)];
        assert_eq!(days_pending(&history, |s| s.glsa_count), Some(2));
    }

    #[test]
    fn no_history_is_none() {
        assert_eq!(days_pending(&[], |s: &HealthSnapshot| s.glsa_count), None);
    }
}
