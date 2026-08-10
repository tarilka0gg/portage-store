use super::command::{CommandRunner, RealCommandRunner};
use super::world;
use std::cell::Cell;
use std::collections::HashSet;

/// How many levels up the "who depends on this" chain to follow before
/// stopping. Four is enough to reach an `@world`-selected package (the
/// actual "why") in the overwhelming majority of real dependency chains.
const MAX_DEPTH: u32 = 4;

/// How many direct dependents to keep *per node* — a secondary, local
/// bound on top of `MAX_EQUERY_CALLS` below. The count still reflects the
/// true total even when truncated.
const MAX_CHILDREN: usize = 8;

/// The real safety bound: a hard ceiling on total `equery` subprocess
/// calls across the *entire* search, not just per level or per node.
/// `MAX_DEPTH` × `MAX_CHILDREN` alone still allows a worst case of
/// `8^4` = 4096 calls if every node happened to fan out to the cap at
/// every level — a low-level library like `glib` genuinely has enough
/// direct dependents to hit that in practice. This counter is checked
/// before every call and stops expanding (treating whatever's left as
/// leaves) once it's spent, so the search's wall-clock time stays
/// bounded by a fixed call count no matter how connected the dependency
/// graph turns out to be — exactly the kind of "this could quietly run
/// for a very long time" risk that's worth guarding against explicitly
/// rather than trusting depth/width limits alone to keep it in check.
const MAX_EQUERY_CALLS: u32 = 60;

/// One node in a "why is this installed" tree: the package, whether it's
/// explicitly in `@world` (the actual reason something's installed, as
/// opposed to merely a transitive dependency), and whatever else depends
/// on *it* — recursion stops at a `@world` node, since that's already a
/// satisfying answer, not just another link to follow further.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepNode {
    pub atom: String,
    pub in_world: bool,
    pub children: Vec<DepNode>,
    /// How many direct dependents were found in total, before capping to
    /// `MAX_CHILDREN` — lets the UI say "+12 more" honestly instead of
    /// silently dropping them.
    pub total_children: usize,
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// `cat/name-1.2.3` -> `cat/name` — `equery depends` reports (and
/// expects) an exact atom-with-version, but `@world` entries and repeat
/// lookups both work on the bare atom.
fn strip_version(atom_with_version: &str) -> String {
    let Some((category, pf)) = atom_with_version.split_once('/') else {
        return atom_with_version.to_string();
    };
    let parts: Vec<&str> = pf.split('-').collect();
    for i in (1..parts.len()).rev() {
        if parts[i].starts_with(|c: char| c.is_ascii_digit()) {
            return format!("{category}/{}", parts[..i].join("-"));
        }
    }
    atom_with_version.to_string()
}

/// Every currently-installed package that directly depends on
/// `bare_atom` — installed-only (no `-a`/`--all-packages`), which is both
/// faster and the right scope here: an ebuild in the tree that *could*
/// depend on this but isn't even installed can't be the reason something
/// else on this exact system is.
fn direct_dependents(runner: &impl CommandRunner, bare_atom: &str) -> Vec<String> {
    let Ok(output) = runner.output("equery", &["--no-color", "depends", bare_atom]) else { return Vec::new() };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().map(|l| strip_ansi(l.trim())).filter(|l| !l.is_empty()).collect()
}

fn build_tree(
    runner: &impl CommandRunner,
    atom_with_version: &str,
    world_atoms: &HashSet<String>,
    depth_remaining: u32,
    visited: &mut HashSet<String>,
    calls_remaining: &Cell<u32>,
) -> DepNode {
    let bare = strip_version(atom_with_version);
    let in_world = world_atoms.contains(&bare);

    let mut children = Vec::new();
    let mut total_children = 0;
    // Stops at a `@world` node (that's the answer, not a link to follow
    // further), at the depth cap, the global call budget, or the first
    // time this exact atom is seen again (a shared dependency reached via
    // two different paths — without this, a diamond in the dependency
    // graph would make the search revisit the same subtree repeatedly).
    if !in_world && depth_remaining > 0 && calls_remaining.get() > 0 && visited.insert(atom_with_version.to_string()) {
        calls_remaining.set(calls_remaining.get() - 1);
        let dependents = direct_dependents(runner, &bare);
        total_children = dependents.len();
        for parent in dependents.into_iter().take(MAX_CHILDREN) {
            if calls_remaining.get() == 0 {
                break;
            }
            children.push(build_tree(runner, &parent, world_atoms, depth_remaining - 1, visited, calls_remaining));
        }
    }

    DepNode { atom: atom_with_version.to_string(), in_world, children, total_children }
}

/// Builds the "why is this installed" tree rooted at `atom_with_version`
/// (e.g. `dev-libs/glib-2.84.0`, the exact form `equery depends` itself
/// reports and `installed::InstalledPackage` can supply). Blocking — runs
/// several `equery` subprocesses (bounded by `MAX_EQUERY_CALLS` no matter
/// how connected the dependency graph is); call off the main thread.
pub fn why_installed(atom_with_version: &str) -> DepNode {
    why_installed_with(&RealCommandRunner, atom_with_version)
}

fn why_installed_with(runner: &impl CommandRunner, atom_with_version: &str) -> DepNode {
    let world_atoms: HashSet<String> = world::read().unwrap_or_default().into_iter().collect();
    let mut visited = HashSet::new();
    let calls_remaining = Cell::new(MAX_EQUERY_CALLS);
    build_tree(runner, atom_with_version, &world_atoms, MAX_DEPTH, &mut visited, &calls_remaining)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portage::command::fake::{FakeCommandRunner, ok};

    #[test]
    fn strips_the_version_suffix() {
        assert_eq!(strip_version("dev-libs/glib-2.84.0"), "dev-libs/glib");
        assert_eq!(strip_version("app-emulation/wine-vanilla-11.0"), "app-emulation/wine-vanilla");
    }

    #[test]
    fn atom_without_a_version_suffix_is_unchanged() {
        assert_eq!(strip_version("dev-libs/glib"), "dev-libs/glib");
    }

    /// A fixed, wider-than-`MAX_CHILDREN` dependent list — every `equery
    /// depends` call in these tests sees the same fixture (the fake
    /// runner keys by program name only), which is exactly what a real
    /// dependency graph's diamonds/reconvergences look like from this
    /// function's perspective: the same atom string showing up as a
    /// "dependent" via more than one path.
    fn wide_dependents_fixture() -> String {
        (0..15).map(|i| format!("dev-libs/fake-dep-{i}-1.0")).collect::<Vec<_>>().join("\n")
    }

    fn max_depth(node: &DepNode) -> u32 {
        1 + node.children.iter().map(max_depth).max().unwrap_or(0)
    }

    #[test]
    fn a_node_reports_its_true_dependent_count_but_caps_real_children() {
        let runner = FakeCommandRunner::new();
        runner.respond("equery", ok(&wide_dependents_fixture()));
        let mut visited = HashSet::new();
        let calls_remaining = Cell::new(MAX_EQUERY_CALLS);
        let node = build_tree(&runner, "dev-libs/root-1.0", &HashSet::new(), MAX_DEPTH, &mut visited, &calls_remaining);
        assert_eq!(node.total_children, 15, "the honest count the UI's \"+N more\" relies on");
        assert!(node.children.len() <= MAX_CHILDREN, "real recursion must stay capped at MAX_CHILDREN");
    }

    #[test]
    fn recursion_never_exceeds_the_global_call_budget() {
        let runner = FakeCommandRunner::new();
        runner.respond("equery", ok(&wide_dependents_fixture()));
        let mut visited = HashSet::new();
        let calls_remaining = Cell::new(MAX_EQUERY_CALLS);
        build_tree(&runner, "dev-libs/root-1.0", &HashSet::new(), MAX_DEPTH, &mut visited, &calls_remaining);
        assert!(
            runner.calls.borrow().len() <= MAX_EQUERY_CALLS as usize,
            "a maximally-connected fixture (15 fan-out at every level) must still respect MAX_EQUERY_CALLS, \
             the one bound that actually keeps this from running for an unbounded amount of time"
        );
    }

    #[test]
    fn recursion_never_exceeds_the_configured_depth() {
        let runner = FakeCommandRunner::new();
        runner.respond("equery", ok(&wide_dependents_fixture()));
        let mut visited = HashSet::new();
        let calls_remaining = Cell::new(MAX_EQUERY_CALLS);
        let node = build_tree(&runner, "dev-libs/root-1.0", &HashSet::new(), MAX_DEPTH, &mut visited, &calls_remaining);
        assert!(max_depth(&node) <= MAX_DEPTH + 1, "MAX_DEPTH levels of children below the root node");
    }

    #[test]
    fn a_world_member_stops_recursion_immediately() {
        let runner = FakeCommandRunner::new();
        // Deliberately no response configured — if this were called, the
        // test would fail with "no response configured", which is exactly
        // the point: a @world node is the answer, not a link to follow,
        // so `direct_dependents` must never run for it.
        let mut visited = HashSet::new();
        let calls_remaining = Cell::new(MAX_EQUERY_CALLS);
        let world_atoms: HashSet<String> = ["dev-libs/root".to_string()].into_iter().collect();
        let node = build_tree(&runner, "dev-libs/root-1.0", &world_atoms, MAX_DEPTH, &mut visited, &calls_remaining);
        assert!(node.in_world);
        assert!(node.children.is_empty());
        assert!(runner.calls.borrow().is_empty());
    }
}
