use super::priv_write;
use anyhow::{Context, Result};
use similar::{ChangeTag, DiffTag, TextDiff};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One file Portage refused to overwrite in place — `CONFIG_PROTECT`'s
/// whole mechanism: rather than clobbering a config file you may have
/// edited, an update lands beside it as `._cfg0000_<name>` and waits for
/// a human to decide. Left alone (the traditional `etc-update`/
/// `dispatch-conf` terminal ritual nobody enjoys), these just pile up.
#[derive(Debug, Clone)]
pub struct PendingUpdate {
    /// The real config file's own path, e.g. `/etc/nginx/nginx.conf`.
    pub live_path: PathBuf,
    /// The `._cfgNNNN_nginx.conf` Portage actually wrote.
    pub proposed_path: PathBuf,
}

impl PendingUpdate {
    pub fn file_name(&self) -> String {
        self.live_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

fn portageq_list(arg: &str) -> Vec<PathBuf> {
    Command::new("portageq")
        .arg(arg)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.split_whitespace().map(PathBuf::from).collect())
        .unwrap_or_default()
}

/// Splits `._cfg0000_nginx.conf` into `"nginx.conf"` — the four digits are
/// a revision counter (bumped if a second update lands before the first
/// is resolved), not meaningful beyond "there's more than one pending".
fn strip_protect_prefix(filename: &str) -> Option<&str> {
    let rest = filename.strip_prefix("._cfg")?;
    let (digits, rest) = rest.split_at_checked(4)?;
    digits.chars().all(|c| c.is_ascii_digit()).then_some(())?;
    rest.strip_prefix('_')
}

/// Walks one `CONFIG_PROTECT` root looking for pending files. Bounded
/// depth and no symlink following — `/etc` is deep but finite, and
/// following symlinked directories risks a cycle for no benefit here.
fn walk(dir: &Path, depth: u32, out: &mut Vec<PendingUpdate>) {
    if depth > 16 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        let path = entry.path();
        if file_type.is_dir() {
            walk(&path, depth + 1, out);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(filename) = entry.file_name().to_str().map(str::to_string) else { continue };
        if let Some(original_name) = strip_protect_prefix(&filename) {
            out.push(PendingUpdate { live_path: dir.join(original_name), proposed_path: path });
        }
    }
}

/// Every pending config update across every `CONFIG_PROTECT` root
/// (usually `/etc` plus a handful of others depending on installed
/// packages). Cheap enough to run on every startup/refresh — it's a
/// bounded filesystem walk, no subprocess per file.
pub fn scan() -> Vec<PendingUpdate> {
    let mut out = Vec::new();
    for dir in portageq_list("config_protect") {
        walk(&dir, 0, &mut out);
    }
    out
}

/// One line-diff segment between the live and proposed content: either
/// identical context, or a real change where exactly one side will be
/// kept. `Vec<DiffSegment>` is a lossless, order-preserving encoding of
/// the whole diff — `build_merged` walks it back into a full file using
/// each `Change` segment's chosen resolution.
#[derive(Debug, Clone)]
pub enum DiffSegment {
    Context(Vec<String>),
    Change { removed: Vec<String>, added: Vec<String> },
}

/// Diffs `live` against `proposed`, line by line.
pub fn diff(live: &str, proposed: &str) -> Vec<DiffSegment> {
    let text_diff = TextDiff::from_lines(live, proposed);
    let mut segments = Vec::new();

    for op in text_diff.ops() {
        match op.tag() {
            DiffTag::Equal => {
                let lines = text_diff.iter_changes(op).map(|c| c.to_string_lossy().trim_end_matches('\n').to_string()).collect();
                segments.push(DiffSegment::Context(lines));
            }
            DiffTag::Delete | DiffTag::Insert | DiffTag::Replace => {
                let mut removed = Vec::new();
                let mut added = Vec::new();
                for change in text_diff.iter_changes(op) {
                    let line = change.to_string_lossy().trim_end_matches('\n').to_string();
                    match change.tag() {
                        ChangeTag::Delete => removed.push(line),
                        ChangeTag::Insert => added.push(line),
                        ChangeTag::Equal => {}
                    }
                }
                segments.push(DiffSegment::Change { removed, added });
            }
        }
    }
    segments
}

/// How many `Change` segments a diff has — the number of independent
/// keep/take decisions the UI needs a resolution for.
pub fn change_count(segments: &[DiffSegment]) -> usize {
    segments.iter().filter(|s| matches!(s, DiffSegment::Change { .. })).count()
}

/// Reconstructs the merged file text: context lines are copied verbatim
/// (identical on both sides by definition), and each `Change` segment
/// contributes either its removed (kept-mine) or added (took-theirs)
/// lines, per `take_theirs[i]` — indexed in the same order `diff` emitted
/// its `Change` segments.
pub fn build_merged(segments: &[DiffSegment], take_theirs: &[bool]) -> String {
    let mut out = String::new();
    let mut change_index = 0;
    for segment in segments {
        let lines: &[String] = match segment {
            DiffSegment::Context(lines) => lines,
            DiffSegment::Change { removed, added } => {
                let choice = take_theirs.get(change_index).copied().unwrap_or(true);
                change_index += 1;
                if choice { added } else { removed }
            }
        };
        for line in lines {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Accepts the proposed file wholesale: live file becomes the proposed
/// content, and the `._cfgNNNN_` file is cleared.
pub fn take_theirs(update: &PendingUpdate) -> Result<()> {
    let content = std::fs::read_to_string(&update.proposed_path)
        .with_context(|| format!("failed to read {}", update.proposed_path.display()))?;
    priv_write::write_then_remove_as_root(&update.live_path, &content, &update.proposed_path)
}

/// Discards the proposed file: live file is left exactly as it is, and
/// the `._cfgNNNN_` file is cleared.
pub fn keep_mine(update: &PendingUpdate) -> Result<()> {
    priv_write::remove_file_as_root(&update.proposed_path)
}

/// Saves a hand-resolved merge: `content` becomes the live file, and the
/// `._cfgNNNN_` file is cleared — the same underlying operation as
/// `take_theirs`, just with caller-supplied content instead of the
/// proposed file's own.
pub fn save_merged(update: &PendingUpdate, content: &str) -> Result<()> {
    priv_write::write_then_remove_as_root(&update.live_path, content, &update.proposed_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_the_four_digit_revision_prefix() {
        assert_eq!(strip_protect_prefix("._cfg0000_nginx.conf"), Some("nginx.conf"));
        assert_eq!(strip_protect_prefix("._cfg0042_foo"), Some("foo"));
    }

    #[test]
    fn ignores_files_that_are_not_pending_updates() {
        assert_eq!(strip_protect_prefix("nginx.conf"), None);
        assert_eq!(strip_protect_prefix("._cfg000_short"), None);
        assert_eq!(strip_protect_prefix(".cfg0000_missing_leading_dot"), None);
    }

    #[test]
    fn identical_files_diff_to_a_single_context_segment() {
        let segments = diff("a\nb\nc\n", "a\nb\nc\n");
        assert_eq!(change_count(&segments), 0);
        assert!(matches!(&segments[..], [DiffSegment::Context(lines)] if lines == &["a", "b", "c"]));
    }

    #[test]
    fn a_changed_line_becomes_one_change_segment_between_context() {
        let segments = diff("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(change_count(&segments), 1);
        let changes: Vec<_> = segments
            .iter()
            .filter_map(|s| match s {
                DiffSegment::Change { removed, added } => Some((removed.clone(), added.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(changes, vec![(vec!["b".to_string()], vec!["X".to_string()])]);
    }

    #[test]
    fn build_merged_picks_mine_or_theirs_per_change() {
        let segments = diff("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(build_merged(&segments, &[true]), "a\nX\nc\n");
        assert_eq!(build_merged(&segments, &[false]), "a\nb\nc\n");
    }

    #[test]
    fn multiple_changes_resolve_independently() {
        let segments = diff("a\nb\nc\nd\ne\n", "A\nb\nc\nD\ne\n");
        assert_eq!(change_count(&segments), 2);
        assert_eq!(build_merged(&segments, &[true, false]), "A\nb\nc\nd\ne\n");
        assert_eq!(build_merged(&segments, &[false, true]), "a\nb\nc\nD\ne\n");
    }
}
