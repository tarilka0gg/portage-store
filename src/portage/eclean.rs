use anyhow::{Context, Result};
use std::process::Command;

/// Which cache `eclean-dist`/`eclean-pkg` (from `app-portage/gentoolkit`)
/// cleans — distfiles (downloaded sources/tarballs) or old binary
/// packages. Two different tools, but identical CLI shape and output, so
/// one type covers both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Distfiles,
    Binpkgs,
}

impl Target {
    fn binary(self) -> &'static str {
        match self {
            Target::Distfiles => "eclean-dist",
            Target::Binpkgs => "eclean-pkg",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::Distfiles => "Downloaded Sources",
            Target::Binpkgs => "Old Binary Packages",
        }
    }
}

/// What a cleanup pass would (or did) reclaim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupPreview {
    pub file_count: usize,
    pub total_kib: u64,
}

/// Runs `eclean-* --pretend --deep` and parses its own summary line
/// rather than the app's normal privileged-job queue — this is read-only
/// (nothing is deleted), fast, and its result is needed synchronously to
/// decide whether the real cleanup button is even worth showing, not
/// streamed like an install.
///
/// `--deep` ("only keep the minimum for a reinstallation") is what makes
/// this find anything at all on a system that's actually already
/// installed everything it downloaded — the non-deep default only clears
/// files for ebuilds no longer in the tree whatsoever, which is a much
/// smaller and rarer category.
pub fn preview(target: Target) -> Result<CleanupPreview> {
    let output = Command::new(target.binary())
        .args(["--pretend", "--deep", "--nocolor"])
        .output()
        .with_context(|| format!("failed to run {}", target.binary()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_preview(&text))
}

/// Parses the trailing summary line:
/// `[  105.8 M ] Total space from 30 files would be freed in the ...`
/// — the one line both tools always print, pretend or not, whether or not
/// anything was actually found (in which case it's simply absent and this
/// returns a zeroed preview).
fn parse_preview(text: &str) -> CleanupPreview {
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix('[') else { continue };
        let Some((size_part, rest)) = rest.split_once(']') else { continue };
        let Some(rest) = rest.trim_start().strip_prefix("Total space from ") else { continue };
        let Some((count_str, _)) = rest.split_once(" files") else { continue };
        let Some(kib) = parse_size_to_kib(size_part.trim()) else { continue };
        let Ok(count) = count_str.trim().parse() else { continue };
        return CleanupPreview { file_count: count, total_kib: kib };
    }
    CleanupPreview { file_count: 0, total_kib: 0 }
}

/// `"105.8 M"` / `"17.6 K"` / `"58.0 M"` -> KiB. `eclean`'s own units are
/// already binary (G/M/K = GiB/MiB/KiB, matching `-h`-style tools, not
/// SI), so this is a straight power-of-1024 conversion, no unit ambiguity
/// to resolve.
fn parse_size_to_kib(text: &str) -> Option<u64> {
    let (value, unit) = text.trim().rsplit_once(' ')?;
    let value: f64 = value.trim().parse().ok()?;
    let kib = match unit.trim() {
        "G" => value * 1024.0 * 1024.0,
        "M" => value * 1024.0,
        "K" => value,
        "B" => value / 1024.0,
        _ => return None,
    };
    Some(kib.round() as u64)
}

/// The real cleanup. Privileged — both caches live under directories only
/// root (or the `portage` group) can write to.
pub fn clean_job(target: Target) -> super::emerge::Job {
    super::emerge::Job {
        privileged: true,
        binary: target.binary().into(),
        args: vec!["--deep".into(), "--nocolor".into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_real_summary_line() {
        let text = " [   17.6 K ] forwardable-1.3.3.tar.gz\n ===========\n [  105.8 M ] Total space from 30 files would be freed in the distfiles directory\n";
        let preview = parse_preview(text);
        assert_eq!(preview.file_count, 30);
        assert_eq!(preview.total_kib, 108339);
    }

    #[test]
    fn already_clean_output_yields_a_zeroed_preview() {
        let text = ">>> Building file list for distfiles cleaning...\n>>> Your distfiles directory was already clean.\n";
        assert_eq!(parse_preview(text), CleanupPreview { file_count: 0, total_kib: 0 });
    }

    #[test]
    fn size_units_convert_to_kib() {
        assert_eq!(parse_size_to_kib("1.0 G"), Some(1024 * 1024));
        assert_eq!(parse_size_to_kib("105.8 M"), Some(108339));
        assert_eq!(parse_size_to_kib("17.6 K"), Some(18));
    }
}
