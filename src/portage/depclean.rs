use super::emerge::Job;

/// Dry-run of a full `--depclean` — distinct from the per-package
/// `emerge --depclean <atom>` `uninstall_job` already uses, which only
/// ever targets one atom the user explicitly asked to remove. A full
/// depclean instead sweeps every package Portage's own dependency graph
/// no longer justifies keeping — the single most dangerous routine
/// operation in Gentoo if run carelessly, which is exactly why this gets
/// a dedicated review flow instead of firing straight from a button.
pub fn pretend_job() -> Job {
    Job { privileged: false, binary: "emerge".into(), args: vec!["--pretend".into(), "--depclean".into()] }
}

/// The real sweep. `protect` atoms are excluded from *this run* via
/// `--exclude` — permanent protection (so future depcleans leave them
/// alone too) is a separate step, adding the atom to `@world` (see
/// `noreplace_job`), since that's a different, longer-lived decision than
/// "not right now".
pub fn depclean_job(protect: &[String]) -> Job {
    let mut args = vec!["--ask=n".into(), "--depclean".into()];
    for atom in protect {
        args.push("--exclude".into());
        args.push(atom.clone());
    }
    Job { privileged: true, binary: "emerge".into(), args }
}

/// Adds `atom` to `@world` without changing what's installed — the
/// "protect this from every future depclean, not just this one" action.
/// `--noreplace` is what makes this a no-op on an already-installed
/// package rather than a reinstall.
pub fn noreplace_job(atom: &str) -> Job {
    Job { privileged: true, binary: "emerge".into(), args: vec!["--noreplace".into(), atom.into()] }
}

/// Whether depclean's own dependency resolution failed outright — it
/// refuses to remove anything unless the whole graph resolves cleanly
/// first, and on a tree that hasn't been fully updated recently, it
/// usually doesn't. Distinct from "resolved fine, found nothing to
/// remove", which needs no warning at all.
pub fn needs_update_first(lines: &[String]) -> bool {
    lines.iter().any(|l| l.contains("Dependencies could not be completely resolved"))
}

/// The exact atoms `--pretend --depclean` would remove, parsed from its
/// one authoritative summary line:
/// `All selected packages: =cat/pkg-1.0 =cat/pkg2-2.0`
/// — rather than the per-package listing above it, which is formatted
/// for a human to read (multi-line, indented "selected:"/"protected:"
/// blocks), not for a program to parse reliably.
pub fn parse_candidates(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .find_map(|l| l.strip_prefix("All selected packages: "))
        .map(|rest| rest.split_whitespace().map(|a| a.trim_start_matches('=').to_string()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_summary_line() {
        let lines = vec![
            ">>> These are the packages that would be unmerged:".to_string(),
            String::new(),
            "All selected packages: =dev-libs/orphan-1.2.3 =sys-apps/stale-0.9".to_string(),
        ];
        assert_eq!(parse_candidates(&lines), vec!["dev-libs/orphan-1.2.3", "sys-apps/stale-0.9"]);
    }

    #[test]
    fn no_summary_line_means_no_candidates() {
        assert_eq!(parse_candidates(&["Nothing to clean up.".to_string()]), Vec::<String>::new());
    }

    #[test]
    fn detects_the_real_unresolved_dependency_message() {
        let lines = vec![
            " * Dependencies could not be completely resolved due to".to_string(),
            " * the following required packages not being installed:".to_string(),
        ];
        assert!(needs_update_first(&lines));
    }

    #[test]
    fn a_clean_resolve_is_not_mistaken_for_an_unresolved_one() {
        let lines = vec!["All selected packages: =dev-libs/orphan-1.2.3".to_string()];
        assert!(!needs_update_first(&lines));
    }
}
