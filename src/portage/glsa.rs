use anyhow::{Context, Result};
use std::process::Command;

/// One Gentoo Linux Security Advisory currently affecting this system —
/// `glsa-check`'s own verdict, not something this app re-derives. Nothing
/// in the default install surfaces these at all; you have to already know
/// `glsa-check` (from `app-portage/gentoolkit`) exists to ever see them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlsaEntry {
    pub id: String,
    pub description: String,
    pub packages: Vec<String>,
}

/// Parses one `glsa-check -l affected` line:
/// `202401-03 [N] BlueZ: Privilege Escalation ( net-wireless/bluez )`
/// — an id, a status marker (ignored; `-l affected` already filters to
/// only the ones that matter), a description, and the affected atoms in
/// parens, space-separated and occasionally trailing `...` when the real
/// list is longer than the line has room for.
fn parse_line(line: &str) -> Option<GlsaEntry> {
    let (id, rest) = line.split_once(' ')?;
    if !id.chars().next()?.is_ascii_digit() {
        return None; // the three legend lines glsa-check always prints first
    }
    let rest = rest.trim_start().strip_prefix('[')?;
    let (_status, rest) = rest.split_once(']')?;
    let (description, packages_part) = rest.rsplit_once('(')?;
    let packages_part = packages_part.trim().trim_end_matches(')');
    let packages: Vec<String> =
        packages_part.split_whitespace().filter(|tok| *tok != "...").map(str::to_string).collect();
    Some(GlsaEntry { id: id.trim().to_string(), description: description.trim().to_string(), packages })
}

/// Every GLSA `glsa-check` currently considers this system affected by —
/// already filtered (that's what `affected` as the glsa-list argument
/// means to `glsa-check` itself), not something this app filters further.
/// Read-only, no root needed.
pub fn list_affected() -> Result<Vec<GlsaEntry>> {
    let output =
        Command::new("glsa-check").args(["--nocolor", "--list", "affected"]).output().context("failed to run glsa-check")?;
    if !output.status.success() {
        anyhow::bail!("glsa-check exited with an error: {}", String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text.lines().filter_map(parse_line).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_glsa_line() {
        let entry = parse_line("202401-03 [N] BlueZ: Privilege Escalation ( net-wireless/bluez )").unwrap();
        assert_eq!(entry.id, "202401-03");
        assert_eq!(entry.description, "BlueZ: Privilege Escalation");
        assert_eq!(entry.packages, vec!["net-wireless/bluez"]);
    }

    #[test]
    fn multiple_packages_and_a_truncated_list() {
        let entry = parse_line(
            "200401-01 [N] Linux kernel do_mremap() local privilege escalation vulnerability ( sys-kernel/aa-sources  sys-kernel/alpha-sources  sys-kernel/arm-sources ... )",
        )
        .unwrap();
        assert_eq!(entry.id, "200401-01");
        assert_eq!(
            entry.packages,
            vec!["sys-kernel/aa-sources", "sys-kernel/alpha-sources", "sys-kernel/arm-sources"]
        );
    }

    #[test]
    fn legend_lines_are_not_entries() {
        assert_eq!(parse_line("[A] means this GLSA was marked as applied (injected),"), None);
        assert_eq!(parse_line("[U] means the system is not affected and"), None);
    }
}
