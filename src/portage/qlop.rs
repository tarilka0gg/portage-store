use std::collections::HashMap;
use std::process::Command;

/// How long this package actually took to merge here, averaged over every
/// recorded merge.
///
/// Read via `qlop` from portage-utils rather than parsing
/// `/var/log/emerge.log` directly: it is the tool the rest of the Gentoo
/// ecosystem uses for this, it already handles the log's quirks, and it is
/// a hard dependency of nothing — portage-utils is on every system.
///
/// Returns `(average_seconds, merge_count)`, or `None` for a package that
/// has never been merged on this machine.
pub fn average_merge_seconds(atom: &str) -> Option<(u64, u32)> {
    // -a: average, -M: machine-readable seconds instead of "1′16″"
    let output = Command::new("qlop").args(["-a", atom, "-M"]).output().ok()?;
    parse_average(&String::from_utf8_lossy(&output.stdout)).into_values().next()
}

/// As `average_merge_seconds`, but for a whole dependency list in one
/// `qlop` invocation instead of one process fork per package — `qlop`
/// already accepts any number of package names in a single call, and
/// there's exactly one emerge log to scan regardless of how many packages
/// it's being asked about, so N separate forks (build-time breakdowns
/// commonly run to 20-30+ dependencies) each re-read the same log for what
/// one pass answers in full. Missing entries (no merge history) simply
/// aren't keys in the result, same as `average_merge_seconds` returning
/// `None` for them.
pub fn average_merge_seconds_batch(atoms: &[String]) -> HashMap<String, (u64, u32)> {
    if atoms.is_empty() {
        return HashMap::new();
    }
    let Ok(output) = Command::new("qlop").arg("-a").args(atoms).arg("-M").output() else {
        return HashMap::new();
    };
    parse_average(&String::from_utf8_lossy(&output.stdout))
}

/// Parses however many of qlop's `cat/pkg: SECONDS average for N merges`
/// lines are in `output` into an atom -> (seconds, merges) map — one line
/// per package when `qlop` was asked about several at once, exactly one
/// when asked about just one.
///
/// Unmerge lines are skipped: qlop reports removals in the same format, and
/// counting "took 2 seconds to uninstall" as a build time would be wildly
/// wrong.
fn parse_average(output: &str) -> HashMap<String, (u64, u32)> {
    let mut result = HashMap::new();
    for line in output.lines() {
        if !line.contains("average for") || line.contains("unmerge") {
            continue;
        }
        let Some((atom, rest)) = line.split_once(": ") else { continue };
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        // "SECONDS average for N merges"
        if tokens.len() >= 4
            && let (Ok(seconds), Ok(count)) = (tokens[0].parse(), tokens[3].parse())
        {
            result.insert(atom.to_string(), (seconds, count));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_average_and_merge_count() {
        let output = "gnome-extra/gnome-software: 16 average for 2 merges\n";
        assert_eq!(parse_average(output).get("gnome-extra/gnome-software"), Some(&(16, 2)));
    }

    #[test]
    fn unmerge_lines_are_not_build_times() {
        let output = "\
www-client/firefox: 3 average for 2 unmerges
www-client/firefox: 7200 average for 3 merges
";
        assert_eq!(parse_average(output).get("www-client/firefox"), Some(&(7200, 3)));
    }

    #[test]
    fn no_history_reads_as_none() {
        assert!(parse_average("").is_empty());
        assert!(parse_average("qlop: no matches found\n").is_empty());
    }

    #[test]
    fn batch_output_yields_one_entry_per_package() {
        let output = "\
app-editors/vim: 12 average for 4 merges
www-client/firefox: 7200 average for 3 merges
dev-lang/rust: 900 average for 1 merges
";
        let parsed = parse_average(output);
        assert_eq!(parsed.get("app-editors/vim"), Some(&(12, 4)));
        assert_eq!(parsed.get("www-client/firefox"), Some(&(7200, 3)));
        assert_eq!(parsed.get("dev-lang/rust"), Some(&(900, 1)));
        assert_eq!(parsed.len(), 3);
    }
}
