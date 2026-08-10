use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// One past job's saved output — enough to list and reopen it, not the
/// text itself (that lives in its own flat file under `LOG_DIR`, see
/// `record`/`read_log`). Kept as a small JSON index, same
/// read-modify-write-whole-file convention `health_history.rs` already
/// uses for its own capped history — a full build log commonly runs to
/// multi-KB of text, so unlike a numeric snapshot it isn't itself embedded
/// in the index: writing one new log would otherwise mean rewriting every
/// previously-stored log's text too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub unix_time: u64,
    pub label: String,
    pub atom: Option<String>,
    pub success: bool,
    /// Filename under `LOG_DIR`, not a full path — the config directory
    /// itself can move (`XDG_CONFIG_HOME` changing between runs), so a
    /// full path baked into the index would go stale.
    pub file: String,
}

/// Capped the same way `health_history.rs` caps its own snapshots — a
/// build log runs bigger than a numeric snapshot, so this cap is smaller;
/// trimming past it deletes the corresponding `.log` file too, not just
/// its index row, so old logs don't just accumulate on disk forever.
const MAX_STORED_LOGS: usize = 200;
const LOG_DIR: &str = "build-logs";

fn config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    let dir = base.join("portage-store");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn log_dir() -> Option<PathBuf> {
    let dir = config_dir()?.join(LOG_DIR);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn index_path() -> Option<PathBuf> {
    Some(config_dir()?.join("build_log_index.json"))
}

/// Every stored entry, newest first.
pub fn index() -> Vec<LogEntry> {
    let Some(path) = index_path() else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let mut entries: Vec<LogEntry> = serde_json::from_str(&text).unwrap_or_default();
    entries.reverse();
    entries
}

/// A URL-unfriendly-character-free stub for the log's filename — doesn't
/// need to be unique on its own (the timestamp prefix already is), just
/// readable in a directory listing.
fn slug(label: &str) -> String {
    let s: String = label.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let s = s.trim_matches('-');
    if s.is_empty() { "job".to_string() } else { s.chars().take(40).collect() }
}

/// Saves one finished job's captured output as its own flat text file
/// (grep-friendly, and so writing this one log never means rewriting any
/// other), then appends its metadata to the index and trims past
/// `MAX_STORED_LOGS` — deleting the trimmed entries' own log files, not
/// just dropping their index rows.
pub fn record(label: &str, atom: Option<&str>, success: bool, lines: &[String]) {
    let Some(dir) = log_dir() else { return };
    let Some(index_path) = index_path() else { return };
    let unix_time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let file = format!("{unix_time}-{}.log", slug(label));
    if std::fs::write(dir.join(&file), lines.join("\n")).is_err() {
        return;
    }

    // `index()` already reverses to newest-first for callers — read the
    // raw (oldest-first) on-disk order back out here so appending and
    // trimming from the front behave the same way `health_history.rs`'s
    // `record` does.
    let mut entries: Vec<LogEntry> = std::fs::read_to_string(&index_path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    entries.push(LogEntry { unix_time, label: label.to_string(), atom: atom.map(str::to_string), success, file });
    if entries.len() > MAX_STORED_LOGS {
        let excess = entries.len() - MAX_STORED_LOGS;
        for trimmed in entries.drain(..excess) {
            let _ = std::fs::remove_file(dir.join(&trimmed.file));
        }
    }
    if let Ok(text) = serde_json::to_string(&entries) {
        let _ = std::fs::write(&index_path, text);
    }
}

/// Reads a stored log's full text back out.
pub fn read_log(entry: &LogEntry) -> Option<String> {
    std::fs::read_to_string(log_dir()?.join(&entry.file)).ok()
}

/// One collapsible section of a build log — `label` is the `>>> ...` line
/// that opened it (portage's own real step/phase marker, the same
/// convention `emerge::parse_step_progress` already relies on elsewhere),
/// or `"Output"` for whatever came before the first one, if anything did.
pub struct LogPhase {
    pub label: String,
    pub lines: Vec<String>,
}

/// Groups `lines` under whichever `>>> ...`-prefixed line precedes them —
/// portage's own real phase/step marker in its output, not a guessed
/// fetch/configure/compile/install taxonomy the raw text doesn't reliably
/// spell out the same way across every ebuild/EAPI.
pub fn split_into_phases(lines: &[String]) -> Vec<LogPhase> {
    let mut phases: Vec<LogPhase> = Vec::new();
    for line in lines {
        if line.trim_start().starts_with(">>> ") {
            phases.push(LogPhase { label: line.trim().to_string(), lines: Vec::new() });
        }
        match phases.last_mut() {
            Some(phase) => phase.lines.push(line.clone()),
            None => phases.push(LogPhase { label: "Output".to_string(), lines: vec![line.clone()] }),
        }
    }
    phases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_lines_under_their_preceding_marker() {
        let lines: Vec<String> = vec![
            ">>> Unpacking source...".to_string(),
            "unpacking foo-1.0.tar.gz".to_string(),
            ">>> Compiling source in /var/tmp/portage/...".to_string(),
            "gcc -O2 ...".to_string(),
            "gcc -O2 ...".to_string(),
        ];
        let phases = split_into_phases(&lines);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].label, ">>> Unpacking source...");
        assert_eq!(phases[0].lines.len(), 2);
        assert_eq!(phases[1].label, ">>> Compiling source in /var/tmp/portage/...");
        assert_eq!(phases[1].lines.len(), 3);
    }

    #[test]
    fn lines_before_the_first_marker_land_in_an_output_section() {
        let lines: Vec<String> = vec!["Calculating dependencies... done!".to_string(), ">>> Emerging (1 of 1) foo".to_string()];
        let phases = split_into_phases(&lines);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].label, "Output");
        assert_eq!(phases[1].label, ">>> Emerging (1 of 1) foo");
    }

    #[test]
    fn a_log_with_no_markers_at_all_is_one_output_section() {
        let lines: Vec<String> = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let phases = split_into_phases(&lines);
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].label, "Output");
        assert_eq!(phases[0].lines, lines);
    }

    #[test]
    fn an_empty_log_yields_no_phases() {
        assert!(split_into_phases(&[]).is_empty());
    }

    #[test]
    fn slug_falls_back_to_job_for_all_punctuation_labels() {
        assert_eq!(slug("!!!"), "job");
        assert_eq!(slug("Installing www-client/firefox"), "installing-www-client-firefox");
    }
}
