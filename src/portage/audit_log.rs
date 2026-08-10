/// One line the priv-helper itself appended after a privileged call —
/// the compensating control for a passwordless doas rule: there's no
/// auth prompt standing between "the app asked for this" and "it
/// happened as root", so this is root-appended (never by the app's own
/// unprivileged process) and read-only from here. See
/// `resources/priv-helper.sh`'s `log_line`/`trap ... EXIT` for exactly
/// how each line gets written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub timestamp: String,
    pub uid: String,
    pub cmd: String,
    pub argv: String,
    pub exit: String,
}

const LOG_PATH: &str = "/var/log/portage-store/priv-helper.log";

/// Exact shape written by `resources/priv-helper.sh`'s `log_line`:
/// `<ISO-8601 UTC> uid=<user> cmd=<subcommand> argv=<truncated argv> exit=<status>`
fn parse_line(line: &str) -> Option<AuditEntry> {
    let (timestamp, rest) = line.split_once(" uid=")?;
    let (uid, rest) = rest.split_once(" cmd=")?;
    let (cmd, rest) = rest.split_once(" argv=")?;
    let (argv, exit) = rest.rsplit_once(" exit=")?;
    Some(AuditEntry {
        timestamp: timestamp.to_string(),
        uid: uid.to_string(),
        cmd: cmd.to_string(),
        argv: argv.to_string(),
        exit: exit.to_string(),
    })
}

/// The most recent `limit` privileged calls, newest first. Reads the log
/// directly (group-readable, `0640 root:portage` — see
/// `resources/priv-helper.sh`'s setup comment; needs no privilege of its
/// own to read on a system where the app's own user is already in that
/// group, same as it already needs to be for `emerge.log`). An unreadable
/// or missing file (helper never installed, or the manual `chown`/`chmod`
/// setup step hasn't been done yet) reads as an empty log, not an error —
/// this is a diagnostic view, not something that should block the rest of
/// the app from working.
pub fn read_recent(limit: usize) -> Vec<AuditEntry> {
    let Ok(text) = std::fs::read_to_string(LOG_PATH) else {
        return Vec::new();
    };
    let mut entries: Vec<AuditEntry> = text.lines().filter_map(parse_line).collect();
    entries.reverse();
    entries.truncate(limit);
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_log_line() {
        let line = "2026-08-10T13:34:13Z uid=tarilka0gg cmd=run argv=run -- emerge --version exit=0";
        let entry = parse_line(line).unwrap();
        assert_eq!(entry, AuditEntry {
            timestamp: "2026-08-10T13:34:13Z".to_string(),
            uid: "tarilka0gg".to_string(),
            cmd: "run".to_string(),
            argv: "run -- emerge --version".to_string(),
            exit: "0".to_string(),
        });
    }

    #[test]
    fn parses_a_line_with_a_nonzero_exit() {
        let line = "2026-08-10T13:33:38Z uid=tarilka0gg cmd=git-revert argv=git-revert not-a-hash exit=1";
        let entry = parse_line(line).unwrap();
        assert_eq!(entry.exit, "1");
    }

    #[test]
    fn parses_a_line_with_empty_argv() {
        let line = "2026-08-10T13:31:59Z uid=tarilka0gg cmd=--self-check argv= exit=0";
        let entry = parse_line(line).unwrap();
        assert_eq!(entry.argv, "");
    }

    #[test]
    fn a_malformed_line_is_skipped() {
        assert_eq!(parse_line("not a log line at all"), None);
    }
}
