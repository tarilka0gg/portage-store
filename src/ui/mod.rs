mod audit_log_page;
mod blocker_dialog;
mod browse;
mod build_failure;
mod build_log_history;
mod checks;
mod cleanup;
mod config_update;
mod depclean;
mod detail;
mod flatpak_lane;
mod health;
mod kernel_page;
mod log_drawer;
mod news;
mod onboarding;
mod why_installed;
mod preferences;
mod presets;
mod profile_switch;
mod queue;
mod runtime;
mod settings;
mod webview;
mod widgets;

use portage_store::backend;
use portage_store::flatpak;
use portage_store::portage::eix::{self, PackageSummary};
use portage_store::portage::emerge::{self, Job};
use portage_store::portage::config_protect::PendingUpdate;
use portage_store::portage::installed::{self, InstalledPackage};
use portage_store::portage::icons;
use portage_store::portage::news::NewsItem;
use portage_store::portage::package_use;
use portage_store::portage::search_query;
use portage_store::portage::world;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;
use widgets::{CATEGORY_GROUPS, CURATED_BLOCKS};

/// One queued emerge invocation plus the label shown while it runs. Jobs
/// run strictly one at a time — portage takes a global lock, so concurrent
/// emerges would just fail — and everything else waits its turn here.
struct QueueEntry {
    job: Job,
    label: String,
    /// Whether finishing this job changes what's installed, and so should
    /// trigger a rescan of the package database.
    mutating: bool,
    /// Whether a failure needing a relaxation on a dependency — USE
    /// flags, an unstable keyword, or a license acceptance (see
    /// `PendingRelaxation`) — should be auto-fixed (writing the change
    /// ourselves, then retrying this exact job once) rather than just
    /// reported. `false` on the retry itself, so a still-failing job
    /// after the fix reports as a real failure instead of looping.
    retry_with_use_fix: bool,
    /// The exact atoms this job is expected to touch, if known ahead of
    /// time — empty when it isn't (most jobs: a single install already
    /// names its own atom in `job.args`, and there's nothing to gain by
    /// duplicating that here). Currently only "Update All" populates
    /// this, since it's the one case that already has the full atom list
    /// on hand from the Updates tab's own pretend run — and the one case
    /// long enough (see `START_ETA_MIN_SECONDS`) for an upfront ETA to
    /// actually be worth showing.
    known_atoms: Vec<String>,
}

/// A queued Flatpak job — much simpler than `QueueEntry`: no USE-flag
/// relaxation retry (Flatpak has no USE flags), no ETA (Flatpak installs
/// are minutes, not hours — see `Caps::shows_build_time`), and mutation
/// tracking isn't needed since the Flatpak "installed" list is re-scanned
/// fresh on every completion regardless.
struct FlatpakQueueEntry {
    job: Job,
    label: String,
}

/// One line in the log sheet: which lane printed it (shown as a prefix
/// when both are running) and whether `is_log_error_line` flagged it —
/// the "errors only" filter just hides everything that isn't.
struct LogLine {
    source: &'static str,
    text: String,
    is_error: bool,
}

/// Whether a line of `emerge`/`flatpak` output reads as an actual error
/// rather than routine noise. Deliberately narrow: portage's own build
/// log is mostly informational " * " lines and the occasional genuine
/// "eselect news read" reminder, neither of which is what "show only
/// errors" is asking to still see. Matches the conventions both tools
/// actually use for a real problem — portage's `!!!`/`* ERROR:` prefixes,
/// flatpak's own "error:" — rather than a bare substring match on
/// "error" (which would also catch lines like `USE=... "error-reporting"`
/// or a package literally named with "error" in it).
fn is_log_error_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("!!!")
        || trimmed.starts_with("* ERROR")
        // Catches both a bare `error: ...` line and the common
        // `<tool>: error: ...` shape autotools/gcc/clang build failures
        // use (e.g. `configure: error: ...`) — a colon after "error" is
        // what actually marks it as the message-severity keyword rather
        // than, say, a USE flag literally named `error-reporting`.
        || trimmed.contains("error:")
        || trimmed.contains(" FAILED")
}

/// One of the four "you need to relax something to proceed" blocks
/// portage prints in an identical shape (see `emerge::parse_required_*`),
/// detected from a failed job's own output and offered as an apply-and-
/// retry dialog rather than just reported as a bare failure — the exact
/// same treatment already proven for USE flags, generalized to the other
/// cases that hit the identical dead end.
#[derive(Clone)]
enum PendingRelaxation {
    Use(Vec<(String, String, bool)>),
    Keyword(Vec<(String, String)>),
    License(Vec<(String, Vec<String>)>),
    /// A circular dependency, resolved by flipping one of the USE flags
    /// portage's own resolver already identified as breaking the cycle
    /// (see `emerge::parse_circular_dependency_use_changes`) — mechanically
    /// identical to `Use` (same `package_use::set_flag` write), but kept
    /// as its own variant so the dialog can honestly say *why* this
    /// change is needed instead of implying it's an ordinary
    /// REQUIRED_USE mismatch.
    Circular(Vec<(String, String, bool)>),
}

impl PendingRelaxation {
    /// Checked in the same order portage itself prints the blocks
    /// (keyword, then USE, then license, then circular-dependency — see
    /// the real output this is parsed from) — not that the order matters
    /// for correctness, since each parser only ever matches its own
    /// block, but this is only ever used to prompt for *one* fix at a
    /// time even if a run somehow needed more than one kind, and
    /// starting with keywords first is as good a choice as any.
    fn detect(output: &[String]) -> Option<Self> {
        let keyword_changes = emerge::parse_required_keyword_changes(output);
        if !keyword_changes.is_empty() {
            return Some(Self::Keyword(keyword_changes));
        }
        let use_changes = emerge::parse_required_use_changes(output);
        if !use_changes.is_empty() {
            return Some(Self::Use(use_changes));
        }
        let license_changes = emerge::parse_required_license_changes(output);
        if !license_changes.is_empty() {
            return Some(Self::License(license_changes));
        }
        let circular_changes = emerge::parse_circular_dependency_use_changes(output);
        if !circular_changes.is_empty() {
            return Some(Self::Circular(circular_changes));
        }
        None
    }

    fn dialog_title(&self) -> &'static str {
        match self {
            Self::Use(_) => "USE flag changes needed",
            Self::Keyword(_) => "Keyword changes needed",
            Self::License(_) => "License acceptance needed",
            Self::Circular(_) => "Circular dependency found",
        }
    }

    /// A short noun phrase for slotting into "applying {this}, retrying"
    /// — `dialog_title` reads fine as a heading but awkwardly mid-sentence.
    fn noun_phrase(&self) -> &'static str {
        match self {
            Self::Use(_) => "the required USE changes",
            Self::Keyword(_) => "the required keyword changes",
            Self::License(_) => "the required license changes",
            Self::Circular(_) => "a USE change to break the cycle",
        }
    }

    /// What this relaxation is for, worded to slot directly after
    /// "{label} " in the confirmation dialog's body.
    fn intro(&self) -> &'static str {
        match self {
            Self::Use(_) => "needs these USE flag changes on a dependency before it can proceed:",
            Self::Keyword(_) => "needs these keyword changes before it can proceed:",
            Self::License(_) => "needs these licenses accepted before it can proceed:",
            Self::Circular(_) => {
                "hit a circular dependency — two or more packages need each other before either can \
                 build. Portage's own resolver found a USE flag change that breaks the cycle:"
            }
        }
    }

    fn body_lines(&self) -> Vec<String> {
        match self {
            Self::Use(changes) | Self::Circular(changes) => changes
                .iter()
                .map(|(atom, flag, enabled)| format!("{atom}  {}{flag}", if *enabled { "" } else { "-" }))
                .collect(),
            Self::Keyword(changes) => changes.iter().map(|(atom, keyword)| format!("{atom}  {keyword}")).collect(),
            Self::License(changes) => {
                changes.iter().map(|(atom, licenses)| format!("{atom}  {}", licenses.join(" "))).collect()
            }
        }
    }

    /// Writes every change in this relaxation to its own managed
    /// `zz-portage-store` file (see `package_use`/`package_keywords`/
    /// `package_license`) — called off the main thread, since each write
    /// goes through `pkexec`.
    fn apply(&self) -> anyhow::Result<()> {
        match self {
            Self::Use(changes) | Self::Circular(changes) => {
                for (atom, flag, enabled) in changes {
                    package_use::set_flag(atom, flag, *enabled)?;
                }
            }
            Self::Keyword(changes) => {
                for (atom, keyword) in changes {
                    portage_store::portage::package_keywords::accept(atom, keyword)?;
                }
            }
            Self::License(changes) => {
                for (atom, licenses) in changes {
                    for license in licenses {
                        portage_store::portage::package_license::accept(atom, license)?;
                    }
                }
            }
        }
        Ok(())
    }
}

pub struct App {
    window: adw::ApplicationWindow,
    nav: adw::NavigationView,
    view_stack: adw::ViewStack,
    toasts: adw::ToastOverlay,
    /// Crossfades from a loading spinner to `toasts` once startup data is
    /// ready — see `mark_startup_task_done`.
    root_stack: gtk::Stack,
    startup_spinner: gtk::Spinner,
    /// Counts down from the number of async startup tasks that must finish
    /// before `root_stack` reveals the real content.
    startup_pending: Cell<u8>,

    search_entry: gtk::SearchEntry,
    search_bar: gtk::SearchBar,

    /// "landing" (category tiles + your apps) vs "results" (search or
    /// category browse) — the Explore page swaps between the two.
    explore_stack: gtk::Stack,
    explore_scroller: gtk::ScrolledWindow,
    results_heading: gtk::Label,
    results_grid: gtk::FlowBox,
    results_spinner: gtk::Spinner,
    yours_grid: gtk::FlowBox,
    filter_button: gtk::MenuButton,
    /// The unfiltered list behind whatever's currently on screen in
    /// `results_grid` — kept around so a filter/sort change can re-render
    /// instantly (`render_filtered_results`) without re-running `eix`.
    last_results: RefCell<Vec<eix::PackageSummary>>,
    search_filters: RefCell<eix::SearchFilters>,
    /// From the search box's own `use:<flag>`/`installed:` operators (see
    /// `portage::search_query`) — kept separate from `search_filters`
    /// rather than folded into it, since the filter popover
    /// (`build_filter_popover`) always rewrites `search_filters` wholesale
    /// from its own widget state, which would silently clobber whatever a
    /// typed operator set. Applied as an extra pass in
    /// `render_filtered_results`, reset on every new search/category
    /// browse so a stale operator from a previous query doesn't linger.
    search_query_use_flag: RefCell<Option<String>>,
    search_query_installed_only: Cell<Option<bool>>,
    /// Whether the Flatpak backend is even worth asking — `flatpak.rs`'s
    /// own auto-detection (binary present, at least one remote), checked
    /// once at startup rather than on every keystroke.
    flatpak_available: Cell<bool>,
    /// Cards currently on screen in `results_grid`, keyed by atom — a
    /// Flatpak search match arriving after Portage's own results are
    /// already drawn needs to find the right card to pin a chip onto
    /// (`widgets::add_flatpak_chip`) without re-rendering the grid.
    search_cards: RefCell<HashMap<String, gtk::Button>>,
    /// Holds the collapsed "Also available via Flatpak" group beneath the
    /// main results — a Flatpak-only hit never gets a card of its own in
    /// `results_grid`, which stays Portage-authoritative.
    flatpak_section: gtk::Box,
    /// The last `backend::merge_search_results` outcome's chip mapping —
    /// consulted every time `render_filtered_results` rebuilds cards
    /// (e.g. a filter changing), since a fresh rebuild otherwise loses
    /// whatever chips `apply_flatpak_results` had already pinned on.
    flatpak_chips: RefCell<HashMap<String, Vec<flatpak::FlatpakApp>>>,

    /// A second, independent job lane for Flatpak installs/updates/
    /// removals. Deliberately not the same `queue`/`running` a Portage
    /// job uses: Portage's global lock means only one `emerge` can ever
    /// run at a time, but Flatpak has no such lock — sharing one FIFO
    /// would make a Flatpak update wait behind an hours-long `@world`
    /// rebuild for no real reason.
    flatpak_queue: RefCell<VecDeque<FlatpakQueueEntry>>,
    flatpak_running: Cell<bool>,
    flatpak_job_revealer: gtk::Revealer,
    flatpak_job_label: gtk::Label,
    flatpak_job_progress: gtk::ProgressBar,

    /// The full-output log sheet — opened by clicking either job bar's
    /// status text, slides up from the bottom (a `gtk::Revealer` stacked
    /// as one more `AdwToolbarView` bottom bar, the same mechanism
    /// `job_revealer`/`flatpak_job_revealer` already use, just for a
    /// taller panel). Every line either job has printed lands here —
    /// `job_log` only ever shows the latest one — with a toggle to hide
    /// anything that isn't flagged as an error.
    log_drawer_revealer: gtk::Revealer,
    log_drawer_buffer: gtk::TextBuffer,
    log_drawer_scroller: gtk::ScrolledWindow,
    log_errors_only: gtk::ToggleButton,
    /// Every line either job type has printed this run, tagged with
    /// which job produced it (for when both lanes are active at once)
    /// and whether it looks like an error — kept independently of
    /// `log_drawer_buffer` so toggling the filter can re-render from
    /// scratch without re-parsing anything.
    log_lines: RefCell<Vec<LogLine>>,

    installed_grid: gtk::FlowBox,
    installed_stack: gtk::Stack,
    installed_scroller: gtk::ScrolledWindow,

    updates_stack: gtk::Stack,
    updates_scroller: gtk::ScrolledWindow,
    updates_list: gtk::ListBox,
    updates_subtitle: gtk::Label,
    updates_space_banner: adw::Banner,
    updates_view_page: adw::ViewStackPage,
    security_section: gtk::Box,
    security_list: gtk::ListBox,
    news_banner: adw::Banner,
    /// Read by `news_banner`'s single, permanently-connected click
    /// handler (wired once in `build()`) — kept up to date by
    /// `check_news`, rather than reconnecting a fresh handler on every
    /// check, which would stack duplicate handlers on top of each other.
    news_items: RefCell<Vec<NewsItem>>,
    config_protect_banner: adw::Banner,
    /// As `news_items`, but for `config_protect_banner`.
    config_protect_items: RefCell<Vec<PendingUpdate>>,
    pending_update_atoms: RefCell<Vec<String>>,
    sync_banner: adw::Banner,

    job_revealer: gtk::Revealer,
    job_label: gtk::Label,
    job_log: gtk::Label,
    job_progress: gtk::ProgressBar,
    job_eta: gtk::Label,
    job_queue_eta: gtk::Label,
    job_run_now_button: gtk::Button,

    installed: RefCell<HashMap<String, InstalledPackage>>,
    icon_paths: RefCell<HashMap<String, PathBuf>>,
    queue: RefCell<VecDeque<QueueEntry>>,
    running: Cell<bool>,
    /// The cookie from `GtkApplication::inhibit`, held for exactly as long
    /// as either job lane is running — see `sync_inhibit`. `None` means
    /// nothing is currently inhibited.
    inhibit_cookie: Cell<Option<u32>>,
    /// Set right before a one-shot `start_next()` call that should ignore
    /// `night_builds_only`'s off-hours gate — see the "Build Now" button
    /// wired to the night-window deferral state. Consumed (reset to
    /// `false`) the moment it's read, via `Cell::take()`, so it never
    /// persists past the single call it was set for.
    force_run_next: Cell<bool>,
    /// Bumped per search so results from an overtaken keystroke get dropped
    /// instead of clobbering newer ones.
    search_generation: Cell<u64>,
    /// Bumped on every keystroke in the search entry, independently of
    /// `search_generation` — lets a debounce timer (see `connect_search_changed`
    /// below) tell whether it's still the most recent keystroke once its
    /// delay elapses, without touching `search_generation`'s own bookkeeping
    /// for in-flight background lookups.
    search_debounce: Cell<u64>,
    /// As `search_generation`, but for `check_updates` — an update job's
    /// own post-success refresh, the header refresh button, and startup
    /// can all trigger a `check_updates` call independently of each
    /// other, and a `--pretend` run against `@world` isn't instant. Without
    /// this, an earlier-started but slower-finishing check could land
    /// *after* a later, more relevant one and silently overwrite its
    /// accurate result with stale data — exactly "I just updated this and
    /// it still shows as pending".
    updates_generation: Cell<u64>,
    settings: RefCell<settings::Settings>,
    /// Container the landing page's curated-theme sections get appended
    /// into once resolved — one section per `CuratedBlock`, each its own
    /// named, independently auto-advancing carousel.
    featured_section: gtk::Box,
}

fn resolve_icons(installed: &HashMap<String, InstalledPackage>) -> HashMap<String, PathBuf> {
    installed
        .iter()
        .filter_map(|(atom, pkg)| {
            icons::resolve_installed_icon(&pkg.category, &pkg.name, &pkg.version)
                .map(|path| (atom.clone(), path))
        })
        .collect()
}

fn scan_installed() -> HashMap<String, InstalledPackage> {
    installed::scan()
        .unwrap_or_default()
        .into_iter()
        .map(|p| (format!("{}/{}", p.category, p.name), p))
        .collect()
}

/// Strips the trailing `-<version>` off a `category/name-version` string
/// (as `emerge::parse_update_atoms` returns) to get the bare atom `emerge`
/// expects
/// for an unpinned "install the latest version" invocation — the same
/// "last `-` followed by a digit" rule `installed::split_pf` uses to split
/// a `PF` directory name, applied here to the category/name half only.
fn strip_version_suffix(atom_with_version: &str) -> String {
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

/// Runs whatever `eix` call(s) a parsed search-box query actually needs —
/// the routing logic behind the `cat:`/`@world` operators (`use:` and
/// `installed:` don't change which `eix` call runs at all; they're applied
/// afterward as in-memory filters, see `render_filtered_results`).
///
/// - `@world` with no other text: resolves every `world::read()` atom via
///   `eix::lookup` — world sets are realistically tens to a couple hundred
///   entries, the same cost class as a normal free-text search.
/// - `@world` combined with text: runs the normal search, then keeps only
///   atoms that are also in the world set.
/// - `cat:<name>` with no other text: `eix::list_categories(&[name])`
///   directly — the exact primitive `browse_category` already uses for its
///   curated category tiles, just fed a user-typed name instead.
/// - `cat:<name>` combined with text: runs the normal search, then keeps
///   only that category (`cat:` narrows an otherwise-normal search rather
///   than replacing it, matching how `use:`/`installed:` behave).
/// - Neither operator: unchanged, `eix::search(text)`.
fn resolve_search(parsed: &search_query::ParsedQuery) -> Result<Vec<PackageSummary>, String> {
    let text = parsed.text.trim();

    let mut results = if parsed.world_only && text.is_empty() {
        let atoms = world::read().map_err(|e| e.to_string())?;
        atoms.into_iter().filter_map(|atom| eix::lookup(&atom).ok().flatten()).collect()
    } else if let Some(category) = &parsed.category
        && text.is_empty()
    {
        eix::list_categories(&[category.as_str()]).map_err(|e| e.to_string())?
    } else if text.is_empty() {
        // An operator with no value and no free text (`cat:`/`use:` alone
        // with nothing else typed) — nothing to search for.
        Vec::new()
    } else {
        eix::search(text).map_err(|e| e.to_string())?
    };

    if parsed.world_only && !text.is_empty() {
        let world_atoms: std::collections::HashSet<String> = world::read().map_err(|e| e.to_string())?.into_iter().collect();
        results.retain(|pkg| world_atoms.contains(&pkg.atom()));
    }
    if let Some(category) = &parsed.category
        && !text.is_empty()
    {
        results.retain(|pkg| &pkg.category == category);
    }

    Ok(results)
}

const NIGHT_WINDOW_START_HOUR: i32 = 23;
const NIGHT_WINDOW_END_HOUR: i32 = 7;

/// Whether it's currently within the "collect at night" window
/// (23:00–07:00 local time). Uses GLib's own local-time clock rather than
/// pulling in a dedicated time crate for one hour comparison.
fn in_night_window() -> bool {
    let Some(now) = gtk::glib::DateTime::now_local().ok() else { return true };
    let hour = now.hour();
    hour >= NIGHT_WINDOW_START_HOUR || hour < NIGHT_WINDOW_END_HOUR
}

/// Turns `active` on for the dot at index `active` in a hand-rolled
/// carousel dot row and off for every other one — see `install_carousel_behavior`
/// for why these are built by hand instead of `adw::CarouselIndicatorDots`.
fn set_active_dot(dots: &[gtk::Box], active: u32) {
    for (i, dot) in dots.iter().enumerate() {
        if i as u32 == active {
            dot.add_css_class("carousel-dot-active");
        } else {
            dot.remove_css_class("carousel-dot-active");
        }
    }
}

/// Only swallows scroll events so they don't fall through to the outer
/// landing page — for a section with just one page, there's nothing to
/// advance to, so the rest of `install_carousel_behavior`'s wiring would
/// be dead weight.
fn install_scroll_guard(carousel: &adw::Carousel) {
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    scroll.connect_scroll(|_, _, _| gtk::glib::Propagation::Stop);
    carousel.add_controller(scroll);
}

/// Wires up auto-advance (paused while hovered), scroll-stop, and true
/// looping behavior for one featured-section carousel.
///
/// `carousel` must already hold `real_pages` real pages at internal
/// indices `1..=real_pages`, bracketed by a duplicate of the *last* real
/// page at index `0` and a duplicate of the *first* at index
/// `real_pages + 1` (built by `populate_featured_carousel`). AdwCarousel's
/// pages sit on one physical strip, so an animated `scroll_to` always
/// slides across it linearly — asked to jump from the last page straight
/// to the first, it would slide backwards through every page in between,
/// reading as movement in the wrong direction. Scrolling one step past
/// either end instead lands on that end's duplicate, which is caught by
/// `connect_page_changed` below and instantly (no animation, since the
/// content is pixel-identical) re-points the carousel at the real page —
/// so from the outside, scrolling right off the last page keeps sliding
/// right onto what looks like the first page, never backwards.
fn install_carousel_behavior(carousel: &adw::Carousel, real_pages: u32, dots: Vec<gtk::Box>) {
    install_carousel_behavior_with_deadzone(carousel, real_pages, dots, 0.0);
}

/// As `install_carousel_behavior`, but the top and bottom `vertical_deadzone`
/// pixels of the carousel don't capture the scroll wheel at all — the event
/// falls through to whatever's underneath instead. Screenshots rarely fill
/// their reserved height exactly (`ContentFit::Contain` letterboxes to fit),
/// so without this, scrolling over that empty letterboxed margin advances
/// the carousel instead of the page behind it, even though nothing visible
/// is there to scroll *through*. The featured carousels' cards always fill
/// their carousel edge-to-edge, so they call the zero-deadzone wrapper above
/// instead of dealing with this parameter at all.
fn install_carousel_behavior_with_deadzone(
    carousel: &adw::Carousel,
    real_pages: u32,
    dots: Vec<gtk::Box>,
    vertical_deadzone: f64,
) {
    let dots = Rc::new(dots);
    let paused = Rc::new(Cell::new(false));

    // The internal carousel index (0 = leading clone, 1..=real_pages =
    // real pages, real_pages + 1 = trailing clone) we're already
    // animating *towards*, tracked ourselves rather than read back from
    // `carousel.position()` on every event — `position()` reports
    // wherever the in-flight scroll animation currently is, which lags
    // the target while it's still easing in, so basing the next step on
    // it while several scroll events arrive in quick succession made the
    // target lag behind the animation and the carousel visibly get stuck
    // oscillating between two pages instead of advancing further each
    // time. Kept in sync with reality by `connect_page_changed` too, so a
    // wrap's instant snap-back doesn't leave it pointing at a duplicate
    // page that no longer exists as the "current" one.
    let target = Rc::new(Cell::new(1u32));

    let dots_for_page_changed = dots.clone();
    let target_for_page_changed = target.clone();
    carousel.connect_page_changed(move |carousel, position| {
        if position == 0 {
            let real_last = carousel.nth_page(real_pages);
            carousel.scroll_to(&real_last, false);
            target_for_page_changed.set(real_pages);
            set_active_dot(&dots_for_page_changed, real_pages - 1);
        } else if position == real_pages + 1 {
            let real_first = carousel.nth_page(1);
            carousel.scroll_to(&real_first, false);
            target_for_page_changed.set(1);
            set_active_dot(&dots_for_page_changed, 0);
        } else {
            target_for_page_changed.set(position);
            set_active_dot(&dots_for_page_changed, position - 1);
        }
    });

    // Also tracks the pointer's last Y within the carousel (only needed
    // for `vertical_deadzone` below) — `EventControllerScroll::connect_scroll`
    // only hands back a delta, not a position, so there's no other way to
    // tell a scroll over the letterboxed margin from one over the picture
    // itself.
    let last_y = Rc::new(Cell::new(0.0f64));
    let hover = gtk::EventControllerMotion::new();
    let paused_for_enter = paused.clone();
    let last_y_for_enter = last_y.clone();
    hover.connect_enter(move |_, _, y| {
        paused_for_enter.set(true);
        // Scrolling right after the pointer enters, before it moves again,
        // is common enough (arrive, scroll immediately) that skipping this
        // would leave `last_y` at its stale default and misjudge every one
        // of those as inside the deadzone.
        last_y_for_enter.set(y);
    });
    let paused_for_leave = paused.clone();
    hover.connect_leave(move |_| paused_for_leave.set(false));
    let last_y_for_motion = last_y.clone();
    hover.connect_motion(move |_, _, y| last_y_for_motion.set(y));
    carousel.add_controller(hover);

    // Without this, scrolling over the carousel to flip through picks
    // also scrolls the outer landing page underneath it — the same fix
    // already proven in detail.rs's screenshot_carousel().
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    let carousel_for_scroll = carousel.clone();
    let target_for_scroll = target.clone();
    // Touchpads report smooth scrolling as many small fractional deltas
    // per physical gesture rather than one whole unit per wheel notch;
    // accumulating them and only stepping once a full notch's worth has
    // built up keeps one gesture equal to one page rather than several.
    let accumulated = Cell::new(0.0f64);
    scroll.connect_scroll(move |_, dx, dy| {
        if vertical_deadzone > 0.0 {
            let y = last_y.get();
            let height = carousel_for_scroll.height() as f64;
            // Capped well under half the carousel's actual height — a
            // `vertical_deadzone` taller than that (as a fixed 200px is,
            // against this carousel's 340px height_request) would have its
            // top and bottom halves overlap and cover the *entire* height,
            // leaving no pixel row that ever reads as "over the picture".
            // With every scroll then falling through as Propagation::Proceed,
            // AdwCarousel's own built-in wheel handling still moves it
            // *and* the event keeps bubbling to page — a double-scroll,
            // not the deadzone this was meant to carve out.
            let effective_deadzone = vertical_deadzone.min(height / 2.0 - 20.0).max(0.0);
            if y < effective_deadzone || y > height - effective_deadzone {
                return gtk::glib::Propagation::Proceed;
            }
        }

        let delta = if dx.abs() > dy.abs() { dx } else { dy };
        if delta == 0.0 {
            return gtk::glib::Propagation::Proceed;
        }

        let total = accumulated.get() + delta;
        let steps = total.trunc();
        accumulated.set(total - steps);
        if steps == 0.0 {
            return gtk::glib::Propagation::Stop;
        }

        // Clamped to the duplicate pages at either end rather than
        // wrapped here — landing on one is what triggers the loop, so a
        // multi-page burst just rides the slide to that edge and lets
        // `connect_page_changed` take it from there instead of trying to
        // hop several real pages at once.
        let next = (target_for_scroll.get() as i64 + steps as i64).clamp(0, real_pages as i64 + 1) as u32;
        target_for_scroll.set(next);
        let page = carousel_for_scroll.nth_page(next);
        carousel_for_scroll.scroll_to(&page, true);
        gtk::glib::Propagation::Stop
    });
    carousel.add_controller(scroll);

    let carousel_for_timer = carousel.clone();
    let target_for_timer = target;
    // Randomized rather than a flat 5 seconds, and started after a
    // randomized initial delay rather than immediately: every section's
    // carousel gets built within the same event-loop pass at startup, so a
    // fixed interval starting right away would tick all of them in
    // lockstep — every category's slideshow advancing on the same beat
    // reads as one big, synchronized flicker rather than independent
    // panels. Different periods and start times mean they drift apart
    // and stay that way.
    let initial_delay = std::time::Duration::from_millis(u64::from(widgets::random_range(0, 4000)));
    let period = widgets::random_range(4, 7);
    gtk::glib::timeout_add_local_once(initial_delay, move || {
        gtk::glib::timeout_add_seconds_local(period, move || {
            if !paused.get() {
                let next = (target_for_timer.get() + 1).min(real_pages + 1);
                target_for_timer.set(next);
                let page = carousel_for_timer.nth_page(next);
                carousel_for_timer.scroll_to(&page, true);
            }
            gtk::glib::ControlFlow::Continue
        });
    });
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

fn page_box() -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content
}

/// A placeholder pushed onto the navigation stack the instant a package
/// card is clicked, standing in for the real detail page until it's
/// actually ready (see `App::open_detail`) — so there's an immediate
/// response to the click instead of a dead pause while `eix::lookup` and
/// the description/screenshot enrichment run.
fn loading_navigation_page() -> adw::NavigationPage {
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_width_request(32);
    spinner.set_height_request(32);
    let label = gtk::Label::new(Some("Loading…"));
    label.add_css_class("dim-label");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_valign(gtk::Align::Center);
    content.set_halign(gtk::Align::Center);
    content.set_vexpand(true);
    content.set_hexpand(true);
    content.append(&spinner);
    content.append(&label);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));

    adw::NavigationPage::builder().title("").child(&toolbar).build()
}

/// The currently visible detail page, if its tag (set to the package's
/// atom in `detail::build`) matches `atom` — so a running job can tell
/// whether the page for the exact package it's installing/removing
/// happens to be open right now.
fn visible_detail_page(nav: &adw::NavigationView, atom: &str) -> Option<adw::NavigationPage> {
    let page = nav.visible_page()?;
    (page.tag().as_deref() == Some(atom)).then_some(page)
}

/// SAFETY: only ever reads data `detail::build` stored under this exact
/// key for its entire lifetime, so this either finds a `GtkProgressBar` or
/// finds nothing.
fn detail_progress_bar(page: &adw::NavigationPage) -> Option<gtk::ProgressBar> {
    unsafe { page.data::<gtk::ProgressBar>("detail-progress-bar").map(|ptr| ptr.as_ref().clone()) }
}

/// SAFETY: as `detail_progress_bar`, but for the `GtkButton` key.
fn detail_action_button(page: &adw::NavigationPage) -> Option<gtk::Button> {
    unsafe { page.data::<gtk::Button>("detail-action-button").map(|ptr| ptr.as_ref().clone()) }
}

/// SAFETY: as `detail_progress_bar`, but for the `Rc<Cell<bool>>` key —
/// whether the button's click handler currently means "Install" (`false`)
/// or "Remove" (`true`), checked live at click time rather than baked into
/// which closure got connected.
fn detail_installed_state(page: &adw::NavigationPage) -> Option<Rc<Cell<bool>>> {
    unsafe { page.data::<Rc<Cell<bool>>>("detail-installed-state").map(|ptr| ptr.as_ref().clone()) }
}

/// Flips this exact detail page's Install/Remove button (and its backing
/// `detail-installed-state` flag) to match `now_installed`, and clears the
/// progress bar/pulsing state a just-finished job left behind — called
/// once a job for this page's own atom completes (see `App::start_next`),
/// updating only the one widget that actually needs to change instead of
/// rebuilding the whole page.
fn refresh_detail_action_button(page: &adw::NavigationPage, now_installed: bool) {
    if let Some(state) = detail_installed_state(page) {
        state.set(now_installed);
    }
    if let Some(button) = detail_action_button(page) {
        button.set_sensitive(true);
        button.remove_css_class("detail-action-pulsing");
        if now_installed {
            button.set_label("Remove");
            button.remove_css_class("suggested-action");
            button.add_css_class("destructive-action");
        } else {
            button.set_label("Install");
            button.remove_css_class("destructive-action");
            button.add_css_class("suggested-action");
        }
    }
    if let Some(bar) = detail_progress_bar(page) {
        bar.set_visible(false);
    }
}

impl App {
    pub fn build(application: &adw::Application) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Portage Store")
            .default_width(1100)
            .default_height(760)
            .build();

        // --- Explore -------------------------------------------------
        let tiles = widgets::grid();
        tiles.set_max_children_per_line(3);
        for (index, group) in CATEGORY_GROUPS.iter().enumerate() {
            tiles.insert(&widgets::category_tile(index, group), -1);
        }

        // Featured: one named section per curated theme (Social, Games,
        // Create, ...), stacked vertically, each its own auto-advancing,
        // hover-to-pause carousel over that theme's picks. Populated
        // asynchronously below; this is just the container the sections
        // get appended into. Built once here and never rebuilt afterwards
        // — switching tabs or opening a detail page and coming back
        // neither resets any carousel's position nor stops its own timer
        // from advancing in the background, since these stay the same
        // live widgets for the whole session rather than being
        // reconstructed per visit.
        let featured_section = gtk::Box::new(gtk::Orientation::Vertical, 24);

        let yours_grid = widgets::grid();
        let yours_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        yours_section.append(&widgets::section_heading("Your Apps"));
        yours_section.append(&yours_grid);

        let landing = page_box();
        landing.append(&tiles);
        landing.append(&featured_section);
        landing.append(&yours_section);

        let results_heading = widgets::section_heading("");
        results_heading.set_hexpand(true);

        // Search filters — USE flag, masked/unmasked, overlay-only,
        // license substring, sort — all applied in memory over the last
        // fetched result list (see `render_filtered_results`), so
        // changing one re-renders instantly with no extra `eix` call.
        let filter_button = gtk::MenuButton::new();
        filter_button.set_icon_name("funnel-symbolic");
        filter_button.set_tooltip_text(Some("Filter & sort results"));
        filter_button.set_valign(gtk::Align::Center);
        filter_button.add_css_class("flat");

        let results_heading_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        results_heading_row.append(&results_heading);
        results_heading_row.append(&filter_button);

        let results_grid = widgets::grid();
        let results_spinner = gtk::Spinner::new();
        results_spinner.set_halign(gtk::Align::Center);

        // The "Also available via Flatpak" section — a Flatpak-only
        // search hit never gets a card in `results_grid` itself, which
        // stays exclusively Portage's. Empty (and so invisible — a
        // `gtk::Box` with no children takes up no space) until a search
        // actually turns up something Flatpak-only.
        let flatpak_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        flatpak_section.set_margin_top(16);

        let results = page_box();
        results.append(&results_heading_row);
        results.append(&results_spinner);
        results.append(&results_grid);
        results.append(&flatpak_section);

        let explore_stack = gtk::Stack::new();
        explore_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        explore_stack.set_transition_duration(180);
        explore_stack.add_named(&adw::Clamp::builder().maximum_size(1160).child(&landing).build(), Some("landing"));
        explore_stack.add_named(&adw::Clamp::builder().maximum_size(1160).child(&results).build(), Some("results"));

        // --- Installed -----------------------------------------------
        let installed_grid = widgets::grid();
        let installed_page = page_box();
        installed_page.append(&widgets::section_heading("Installed Packages"));
        installed_page.append(&installed_grid);

        let installed_scroller = scrolled(&adw::Clamp::builder().maximum_size(1160).child(&installed_page).build());
        let installed_stack = gtk::Stack::new();
        installed_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        installed_stack.set_transition_duration(180);
        installed_stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("content-loading-symbolic")
                .title("Reading package database…")
                .build(),
            Some("loading"),
        );
        installed_stack.add_named(&installed_scroller, Some("list"));

        // --- Updates --------------------------------------------------
        let updates_list = gtk::ListBox::new();
        updates_list.add_css_class("boxed-list");
        updates_list.set_selection_mode(gtk::SelectionMode::None);

        let updates_subtitle = gtk::Label::new(None);
        updates_subtitle.set_xalign(0.0);
        updates_subtitle.add_css_class("dim-label");

        let update_all = gtk::Button::with_label("Update All");
        update_all.add_css_class("suggested-action");
        update_all.add_css_class("pill");
        update_all.set_halign(gtk::Align::Start);

        // Hidden until `show_updates` knows a total download size to check
        // free space against, and only revealed if that space looks tight
        // — a full `@world` update is the single largest download/build
        // this app ever runs, and the one place running out of disk mid-way
        // is most painful.
        let updates_space_banner = adw::Banner::new("");

        // Security advisories (`glsa-check`) sit above the ordinary update
        // list, not mixed into it — an unpatched vulnerability isn't just
        // another pending update, it's the one category worth a visibly
        // different (destructive-tinted) section and its own heading.
        // Hidden entirely when nothing's affected, which — with no GLSAs
        // currently affecting most systems most of the time — is the
        // common case.
        let security_list = gtk::ListBox::new();
        security_list.add_css_class("boxed-list");
        security_list.set_selection_mode(gtk::SelectionMode::None);
        let security_heading = widgets::section_heading("Security Advisories");
        let security_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        security_section.append(&security_heading);
        security_section.append(&security_list);
        security_section.set_visible(false);

        let updates_page = page_box();
        updates_page.append(&security_section);
        updates_page.append(&widgets::section_heading("Available Updates"));
        updates_page.append(&updates_space_banner);
        updates_page.append(&updates_subtitle);
        updates_page.append(&update_all);
        updates_page.append(&updates_list);

        let updates_stack = gtk::Stack::new();
        updates_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        updates_stack.set_transition_duration(180);
        updates_stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("content-loading-symbolic")
                .title("Checking for updates…")
                .description("Portage is checking the package tree")
                .build(),
            Some("checking"),
        );
        updates_stack.add_named(
            &adw::StatusPage::builder()
                .icon_name("object-select-symbolic")
                .title("Up to Date")
                .description("No updates for @world")
                .build(),
            Some("uptodate"),
        );
        let updates_scroller = scrolled(&adw::Clamp::builder().maximum_size(1160).child(&updates_page).build());
        updates_stack.add_named(&updates_scroller, Some("list"));

        // --- header + view switcher ----------------------------------
        let explore_scroller = scrolled(&explore_stack);
        let view_stack = adw::ViewStack::new();
        view_stack.add_titled_with_icon(&explore_scroller, Some("explore"), "Explore", "view-grid-symbolic");
        view_stack.add_titled_with_icon(&installed_stack, Some("installed"), "Installed", "drive-harddisk-symbolic");
        let updates_view_page =
            view_stack.add_titled_with_icon(&updates_stack, Some("updates"), "Updates", "software-update-available-symbolic");

        let switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();

        let search_toggle = gtk::ToggleButton::new();
        search_toggle.set_icon_name("system-search-symbolic");
        search_toggle.set_tooltip_text(Some("Search packages"));

        let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_button.set_tooltip_text(Some("Rescan system"));

        let advanced_menu = gtk::gio::Menu::new();
        advanced_menu.append(Some("GitHub Page Preview"), Some("win.github-preview"));
        advanced_menu.append(Some("Throttle Builds (nice/ionice + MAKEOPTS)"), Some("win.throttle-builds"));
        advanced_menu.append(Some("Collect at Night Only"), Some("win.night-builds-only"));
        advanced_menu.append(Some("Periodic Health Checks"), Some("win.periodic-health-checks"));
        advanced_menu.append(Some("Cache Binary Packages on Install"), Some("win.buildpkg-on-install"));

        let maintenance_menu = gtk::gio::Menu::new();
        maintenance_menu.append(Some("System Health"), Some("win.health"));
        maintenance_menu.append(Some("Sync Package Tree"), Some("win.sync"));
        maintenance_menu.append(Some("Free Up Space"), Some("win.cleanup"));
        maintenance_menu.append(Some("Remove Orphaned Packages"), Some("win.depclean"));
        maintenance_menu.append(Some("Setup Wizard"), Some("win.onboarding"));
        maintenance_menu.append(Some("Package Presets"), Some("win.presets"));

        let menu = gtk::gio::Menu::new();
        menu.append_submenu(Some("Maintenance"), &maintenance_menu);
        menu.append(Some("Prefer Binary Packages"), Some("win.prefer-binpkg"));
        menu.append(Some("Portage Settings"), Some("win.preferences"));
        menu.append(Some("About"), Some("win.about"));
        menu.append_submenu(Some("Advanced"), &advanced_menu);
        let menu_button = gtk::MenuButton::new();
        menu_button.set_icon_name("open-menu-symbolic");
        menu_button.set_menu_model(Some(&menu));

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&switcher));
        header.pack_start(&search_toggle);
        header.pack_end(&menu_button);
        header.pack_end(&refresh_button);

        let search_entry = gtk::SearchEntry::new();
        search_entry.set_placeholder_text(Some("Find a package…"));
        search_entry.set_hexpand(true);
        let search_bar = gtk::SearchBar::builder()
            .child(&adw::Clamp::builder().maximum_size(600).child(&search_entry).build())
            .build();
        search_bar.connect_entry(&search_entry);
        search_bar.set_key_capture_widget(Some(&window));
        search_toggle
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        // --- job bar --------------------------------------------------
        let job_label = gtk::Label::new(None);
        job_label.set_xalign(0.0);
        job_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        job_label.add_css_class("heading");

        let job_log = gtk::Label::new(None);
        job_log.set_xalign(0.0);
        job_log.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        job_log.add_css_class("dim-label");
        job_log.add_css_class("job-log");

        let job_spinner = gtk::Spinner::new();
        job_spinner.start();
        job_spinner.set_valign(gtk::Align::Center);

        // Hidden until a job actually starts (`start_next` reveals and
        // pulses it) — before that there's nothing running to show
        // progress for.
        let job_progress = gtk::ProgressBar::new();
        job_progress.set_show_text(true);
        job_progress.set_visible(false);

        // Hidden until a job with a known atom list (currently only
        // "Update All" — see `QueueEntry::known_atoms`) actually starts
        // and its qlop lookup answers. For anything shorter this would
        // just be noise; it exists for exactly the "this could run for
        // hours, and I'm about to walk away" case.
        let job_eta = gtk::Label::new(None);
        job_eta.set_xalign(0.0);
        job_eta.add_css_class("dim-label");
        job_eta.add_css_class("caption");
        job_eta.set_visible(false);

        // Separate from `job_eta` (which is scoped to the one job actually
        // running) — this sums whatever's left in `queue` behind it, so
        // "walking away for the night" has one number for the whole
        // backlog, not just whatever happens to be running first. Hidden
        // whenever nothing queued has any build history to estimate from.
        let job_queue_eta = gtk::Label::new(None);
        job_queue_eta.set_xalign(0.0);
        job_queue_eta.add_css_class("dim-label");
        job_queue_eta.add_css_class("caption");
        job_queue_eta.set_visible(false);

        let job_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        job_text.set_hexpand(true);
        job_text.append(&job_label);
        job_text.append(&job_progress);
        job_text.append(&job_eta);
        job_text.append(&job_queue_eta);
        job_text.append(&job_log);

        // Opens a popover listing everything waiting behind the current
        // job — cancel a queued job outright, or bump one to the front
        // ahead of a long `@world` update, without needing to touch (or
        // kill) whatever's actually running right now.
        let queue_button = gtk::Button::from_icon_name("view-list-symbolic");
        queue_button.add_css_class("flat");
        queue_button.set_valign(gtk::Align::Center);
        queue_button.set_tooltip_text(Some("View queued jobs"));

        // Shown only while a job is sitting out `night_builds_only`'s
        // off-hours window (see `start_next`'s deferral branch) — the
        // override for "actually, run it now," wired once `app` exists.
        // Hidden the rest of the time rather than always present and
        // disabled, since it does nothing outside that one state.
        let job_run_now_button = gtk::Button::with_label("Build Now");
        job_run_now_button.add_css_class("flat");
        job_run_now_button.set_valign(gtk::Align::Center);
        job_run_now_button.set_tooltip_text(Some("Skip the night-hours wait and start now"));
        job_run_now_button.set_visible(false);

        // Wrapped in a flat, chrome-free button rather than a bare click
        // gesture on the box — same visible layout, but gets hover/press
        // feedback and keyboard/accessibility activation for free.
        // Opens the log sheet (`log_drawer_revealer`), wired once `app`
        // exists (see below `App::build`).
        let job_text_button = gtk::Button::builder().child(&job_text).build();
        job_text_button.add_css_class("flat");
        job_text_button.set_hexpand(true);
        job_text_button.set_tooltip_text(Some("View full log"));

        let job_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        job_box.set_margin_top(10);
        job_box.set_margin_bottom(10);
        job_box.set_margin_start(14);
        job_box.set_margin_end(14);
        job_box.append(&job_spinner);
        job_box.append(&job_text_button);
        job_box.append(&job_run_now_button);
        job_box.append(&queue_button);

        let job_revealer = gtk::Revealer::builder().child(&job_box).build();

        // --- Flatpak job bar --------------------------------------------
        // A second, thinner bar rather than reusing `job_box`: Flatpak
        // jobs run on their own lock domain (see `flatpak_queue` on
        // `App`) and can be going on *at the same time* as a Portage job
        // — one shared bar could only ever show one of the two.
        let flatpak_job_label = gtk::Label::new(None);
        flatpak_job_label.set_xalign(0.0);
        flatpak_job_label.add_css_class("caption-heading");

        let flatpak_job_progress = gtk::ProgressBar::new();
        flatpak_job_progress.set_show_text(true);

        let flatpak_job_spinner = gtk::Spinner::new();
        flatpak_job_spinner.start();
        flatpak_job_spinner.set_valign(gtk::Align::Center);

        let flatpak_job_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        flatpak_job_text.set_hexpand(true);
        flatpak_job_text.append(&flatpak_job_label);
        flatpak_job_text.append(&flatpak_job_progress);

        let flatpak_job_text_button = gtk::Button::builder().child(&flatpak_job_text).build();
        flatpak_job_text_button.add_css_class("flat");
        flatpak_job_text_button.set_hexpand(true);
        flatpak_job_text_button.set_tooltip_text(Some("View full log"));

        let flatpak_job_box = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        flatpak_job_box.set_margin_top(6);
        flatpak_job_box.set_margin_bottom(6);
        flatpak_job_box.set_margin_start(14);
        flatpak_job_box.set_margin_end(14);
        flatpak_job_box.append(&flatpak_job_spinner);
        flatpak_job_box.append(&flatpak_job_text_button);

        let flatpak_job_revealer = gtk::Revealer::builder().child(&flatpak_job_box).build();

        // --- log sheet ---------------------------------------------------
        // The full-output view behind either job bar's "latest line"
        // label — every line collected in `log_lines`, rendered here on
        // demand rather than kept permanently on screen the way the
        // one-line summary is. A `gtk::TextView` over a `gtk::TextBuffer`
        // rather than one row per line: a real build can print thousands
        // of lines, and a `ListBox` of that many rows would be far
        // heavier than one text buffer holding the same content.
        let log_drawer_buffer = gtk::TextBuffer::new(None);
        let log_drawer_view = gtk::TextView::with_buffer(&log_drawer_buffer);
        log_drawer_view.set_editable(false);
        log_drawer_view.set_cursor_visible(false);
        log_drawer_view.set_monospace(true);
        log_drawer_view.set_left_margin(8);
        log_drawer_view.set_top_margin(6);
        log_drawer_view.set_bottom_margin(6);
        log_drawer_view.add_css_class("caption");

        let log_drawer_scroller =
            gtk::ScrolledWindow::builder().vexpand(true).height_request(220).child(&log_drawer_view).build();

        let log_errors_only = gtk::ToggleButton::with_label("Errors Only");
        log_errors_only.add_css_class("flat");
        log_errors_only.set_tooltip_text(Some("Hide everything except lines that look like an actual error"));

        // Opens the persisted, phase-collapsed, searchable log history
        // (`build_log_history.rs`) — a *past* job's output, distinct from
        // this drawer, which only ever shows the currently-running (or
        // just-finished) job's live tail.
        let log_history_button = gtk::Button::from_icon_name("document-open-recent-symbolic");
        log_history_button.add_css_class("flat");
        log_history_button.set_tooltip_text(Some("Past builds"));
        log_history_button.connect_clicked(|button| build_log_history::present(button));

        let log_close_button = gtk::Button::from_icon_name("go-down-symbolic");
        log_close_button.add_css_class("flat");
        log_close_button.set_tooltip_text(Some("Close"));

        let log_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        log_header.set_margin_start(12);
        log_header.set_margin_end(8);
        log_header.set_margin_top(6);
        let log_header_title = gtk::Label::new(Some("Log"));
        log_header_title.add_css_class("heading");
        log_header_title.set_hexpand(true);
        log_header_title.set_xalign(0.0);
        log_header.append(&log_header_title);
        log_header.append(&log_errors_only);
        log_header.append(&log_history_button);
        log_header.append(&log_close_button);

        let log_column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        log_column.append(&log_header);
        log_column.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        log_column.append(&log_drawer_scroller);
        log_column.add_css_class("background");

        // Slides up over the job bars beneath it rather than replacing
        // them — closing it (via `log_close_button` or reopening the same
        // job's own status line) leaves whatever job is still running
        // visible exactly as before.
        let log_drawer_revealer =
            gtk::Revealer::builder().transition_type(gtk::RevealerTransitionType::SlideUp).child(&log_column).build();
        {
            let log_drawer_revealer_for_close = log_drawer_revealer.clone();
            log_close_button.connect_clicked(move |_| log_drawer_revealer_for_close.set_reveal_child(false));
        }

        // Gentoo news is a GLEP-42 announcement channel for changes that
        // can break a system if missed (a profile migration, a dropped
        // default) — `emerge`/`eix` print a one-line reminder about
        // unread items after every sync, easy to lose in terminal output
        // and, before this, not surfaced anywhere in this app at all.
        // Sits above the tab content (not per-tab) since it applies
        // regardless of which tab happens to be open.
        let news_banner = adw::Banner::new("");
        news_banner.set_button_label(Some("Read"));

        // `CONFIG_PROTECT` files (`._cfg0000_name`) that portage refused
        // to overwrite in place — the terminal-only `etc-update`/
        // `dispatch-conf` ritual nobody enjoys, surfaced the same way
        // unread news is: a banner that's there when it matters and gone
        // otherwise.
        let config_protect_banner = adw::Banner::new("");
        config_protect_banner.set_button_label(Some("Review"));

        // A stale tree means "Updates" is answering from an old snapshot
        // of the package repository — every result on that tab could be
        // wrong (missing a real update, or offering one that's already
        // been superseded) without anything else in the app being able to
        // tell. Shown only past a threshold (see `check_sync`), not on
        // every launch — a tree synced an hour ago needs no banner at all.
        let sync_banner = adw::Banner::new("");
        sync_banner.set_button_label(Some("Sync Now"));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&search_bar);
        toolbar.add_top_bar(&news_banner);
        toolbar.add_top_bar(&config_protect_banner);
        toolbar.add_top_bar(&sync_banner);
        toolbar.set_content(Some(&view_stack));
        toolbar.add_bottom_bar(&log_drawer_revealer);
        toolbar.add_bottom_bar(&job_revealer);
        toolbar.add_bottom_bar(&flatpak_job_revealer);

        let main_page = adw::NavigationPage::builder()
            .title("Portage Store")
            .tag("main")
            .child(&toolbar)
            .build();
        let nav = adw::NavigationView::new();
        nav.add(&main_page);

        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&nav));

        // Preload: the window comes up on a spinner instead of the real
        // layout, which otherwise would flash empty grids and a blank
        // "Featured" area for the split second before the installed-package
        // scan and the curated carousels finish resolving. `root_stack`
        // crossfades to `toasts` once both are ready (see
        // `mark_startup_task_done`), so what the user actually sees appear
        // is one settled screen, not content popping in piece by piece.
        let startup_spinner = gtk::Spinner::new();
        startup_spinner.set_spinning(true);
        startup_spinner.set_width_request(32);
        startup_spinner.set_height_request(32);
        let startup_label = gtk::Label::new(Some("Loading your packages…"));
        startup_label.add_css_class("dim-label");
        let startup_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        startup_box.set_valign(gtk::Align::Center);
        startup_box.set_halign(gtk::Align::Center);
        startup_box.set_vexpand(true);
        startup_box.set_hexpand(true);
        startup_box.append(&startup_spinner);
        startup_box.append(&startup_label);

        let root_stack = gtk::Stack::new();
        root_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        root_stack.set_transition_duration(220);
        root_stack.add_named(&startup_box, Some("loading"));
        root_stack.add_named(&toasts, Some("content"));
        root_stack.set_visible_child_name("loading");

        // The screenshot lightbox's scrim (detail.rs) — a plain dimmed
        // backdrop with the carousel centered on it, layered over the
        // window's whole content via `gtk::Overlay`. An earlier attempt at
        // exactly this failed: an overlay child sized down to its own
        // content instead of covering the window, and neither
        // `hexpand`/`vexpand` nor an explicit `size_request` fixed it — so
        // that attempt fell back to a `gtk::Stack` page instead, which
        // guarantees full coverage but can only ever show one page at a
        // time, hiding the actual page content the scrim was supposed to
        // dim rather than fully replace it. The fix here is `halign`/
        // `valign` set to `Fill` *on the overlay child itself* — the
        // hexpand/vexpand properties control how a widget's *parent*
        // divides space among siblings, which does nothing for an overlay
        // child (there are no siblings competing for space; it's stacked,
        // not packed), while `GtkOverlay` explicitly measures each overlay
        // child at the main child's size only when that child's own align
        // is `Fill` — anything else and it gets exactly its natural size
        // instead, which is what was actually happening before.
        let screenshot_lightbox_scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
        screenshot_lightbox_scrim.add_css_class("screenshot-lightbox-scrim");
        screenshot_lightbox_scrim.set_halign(gtk::Align::Fill);
        screenshot_lightbox_scrim.set_valign(gtk::Align::Fill);
        // Starts invisible and non-interactive: an overlay child, unlike a
        // hidden stack page, still receives pointer events and paints
        // (even at opacity 0 from the `-hidden` CSS class) unless told not
        // to — without this, an invisible full-window box would silently
        // eat every click on the actual page underneath it.
        screenshot_lightbox_scrim.set_visible(false);
        screenshot_lightbox_scrim.set_can_target(false);

        let window_overlay = gtk::Overlay::new();
        window_overlay.set_child(Some(&root_stack));
        window_overlay.add_overlay(&screenshot_lightbox_scrim);

        window.set_content(Some(&window_overlay));
        // SAFETY: `window` owns these for its entire lifetime and nothing
        // else ever inserts under either key, so retrieval always finds
        // exactly the widgets stored here.
        unsafe {
            window.set_data("screenshot-lightbox-scrim", screenshot_lightbox_scrim);
        }

        let app = Rc::new(App {
            window,
            nav,
            view_stack,
            toasts,
            root_stack,
            startup_spinner,
            startup_pending: Cell::new(2),
            search_entry,
            search_bar,
            explore_stack,
            explore_scroller,
            results_heading,
            results_grid,
            results_spinner,
            yours_grid,
            filter_button,
            last_results: RefCell::new(Vec::new()),
            search_filters: RefCell::new(eix::SearchFilters::default()),
            search_query_use_flag: RefCell::new(None),
            search_query_installed_only: Cell::new(None),
            flatpak_available: Cell::new(false),
            search_cards: RefCell::new(HashMap::new()),
            flatpak_section,
            flatpak_chips: RefCell::new(HashMap::new()),
            flatpak_queue: RefCell::new(VecDeque::new()),
            flatpak_running: Cell::new(false),
            flatpak_job_revealer,
            flatpak_job_label,
            flatpak_job_progress,
            log_drawer_revealer,
            log_drawer_buffer,
            log_drawer_scroller,
            log_errors_only,
            log_lines: RefCell::new(Vec::new()),
            installed_grid,
            installed_stack,
            installed_scroller,
            updates_stack,
            updates_scroller,
            updates_list,
            updates_subtitle,
            updates_space_banner,
            updates_view_page,
            security_section,
            security_list,
            news_banner,
            news_items: RefCell::new(Vec::new()),
            config_protect_banner,
            config_protect_items: RefCell::new(Vec::new()),
            pending_update_atoms: RefCell::new(Vec::new()),
            sync_banner,
            job_revealer,
            job_label,
            job_log,
            job_progress,
            job_eta,
            job_queue_eta,
            job_run_now_button,
            installed: RefCell::new(HashMap::new()),
            icon_paths: RefCell::new(HashMap::new()),
            queue: RefCell::new(VecDeque::new()),
            running: Cell::new(false),
            inhibit_cookie: Cell::new(None),
            force_run_next: Cell::new(false),
            search_generation: Cell::new(0),
            search_debounce: Cell::new(0),
            updates_generation: Cell::new(0),
            settings: RefCell::new(settings::load()),
            featured_section: featured_section.clone(),
        });

        {
            let app_for_click = app.clone();
            app.news_banner.connect_button_clicked(move |_| {
                let app_for_refresh = app_for_click.clone();
                let on_changed: Rc<dyn Fn()> = Rc::new(move || app_for_refresh.check_news());
                news::present(&app_for_click.window, app_for_click.news_items.borrow().clone(), on_changed);
            });
        }
        {
            let app_for_click = app.clone();
            app.config_protect_banner.connect_button_clicked(move |_| {
                let app_for_refresh = app_for_click.clone();
                let on_resolved: Rc<dyn Fn()> = Rc::new(move || app_for_refresh.check_config_protect());
                config_update::present(&app_for_click.window, app_for_click.config_protect_items.borrow().clone(), on_resolved);
            });
        }
        {
            let app_for_click = app.clone();
            app.sync_banner.connect_button_clicked(move |_| {
                app_for_click.enqueue(QueueEntry {
                    job: portage_store::portage::sync::sync_job(),
                    label: "Syncing package tree".to_string(),
                    // Not a package install, but a sync genuinely changes
                    // what "up to date" means — reusing the same
                    // post-job refresh an install triggers (updates,
                    // config-protect, GLSA, and — added below — sync age
                    // itself) is exactly right here, not a special case.
                    mutating: true,
                    retry_with_use_fix: false,
                    known_atoms: Vec::new(),
                });
            });
        }
        {
            let app_for_click = app.clone();
            queue_button.connect_clicked(move |button| app_for_click.present_queue_popover(button));
        }
        {
            let app_for_click = app.clone();
            app.job_run_now_button.connect_clicked(move |_| {
                app_for_click.force_run_next.set(true);
                app_for_click.start_next();
            });
        }
        {
            let app_for_click = app.clone();
            job_text_button.connect_clicked(move |_| app_for_click.toggle_log_drawer());
        }
        {
            let app_for_click = app.clone();
            flatpak_job_text_button.connect_clicked(move |_| app_for_click.toggle_log_drawer());
        }
        {
            let app_for_toggle = app.clone();
            app.log_errors_only.connect_toggled(move |_| app_for_toggle.render_log_drawer());
        }
        {
            let popover = app.build_filter_popover();
            app.filter_button.set_popover(Some(&popover));
        }
        {
            // Checked once, off the main thread — `flatpak::is_available`
            // shells out twice (`which`, `flatpak remotes`) and there's no
            // reason to pay that cost on every search.
            let app = app.clone();
            runtime::spawn_blocking(flatpak::is_available, move |available| app.flatpak_available.set(available));
        }
        app.install_header_gesture(&header);
        app.install_window_actions();
        app.install_keyboard_shortcuts();
        app.connect_signals(&tiles, &update_all, &refresh_button);
        app.rescan_installed();
        app.check_updates();
        app.check_news();
        app.check_config_protect();
        app.check_glsa();
        app.check_sync();
        app.populate_featured_carousel();

        // Ticks every 6 hours; the setting itself (checked inside the
        // closure, not by whether the timer exists) is what decides
        // whether that tick actually does anything — this way flipping
        // the toggle on takes effect from the very next tick without
        // needing to restart the app.
        {
            let app_for_timer = app.clone();
            gtk::glib::timeout_add_seconds_local(6 * 60 * 60, move || {
                if app_for_timer.settings.borrow().periodic_health_checks {
                    app_for_timer.run_periodic_health_check();
                }
                gtk::glib::ControlFlow::Continue
            });
        }

        app
    }

    /// Double-clicking the header bar steps back towards the landing page —
    /// including when the click lands on the ViewSwitcher's own tab
    /// buttons, which is the expected target for this gesture but also a
    /// widget with click handling of its own.
    ///
    /// Set to the capture phase deliberately: on the default bubble phase,
    /// a `GtkToggleButton` like a switcher tab claims the press for itself
    /// before it would reach an ancestor's bubble-phase gesture, so this
    /// controller would silently never see a double-click that lands on a
    /// tab. Capture runs top-down before that happens. Only claiming the
    /// sequence on the second press (never the first) leaves the button's
    /// own single-click tab-switching completely intact.
    ///
    /// Claiming also replaces GTK's own double-click-to-maximise on the
    /// header — the window buttons and the usual keyboard shortcut still
    /// maximise.
    fn install_header_gesture(self: &Rc<Self>, header: &adw::HeaderBar) {
        let gesture = gtk::GestureClick::new();
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        let app = self.clone();
        gesture.connect_pressed(move |gesture, n_press, _, _| {
            if n_press == 2 {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                app.home_or_top();
            }
        });
        header.add_controller(gesture);
    }

    /// A small, deliberately terminal-flavored keyboard layer on top of
    /// GTK's own tab/arrow-key navigation, since the actual audience here
    /// already lives with a keyboard-first workflow: `/` jumps straight
    /// to search (the same convention GitHub, Gmail, and `less` all use)
    /// and `j`/`k`/`h`/`l` move focus through whichever grid is currently
    /// showing, the same directions vim itself uses. Both are ignored
    /// the moment focus is actually inside a text field — a `j` typed
    /// into the search box must stay a `j`, not a navigation command.
    fn install_keyboard_shortcuts(self: &Rc<Self>) {
        let controller = gtk::EventControllerKey::new();
        let app = self.clone();
        controller.connect_key_pressed(move |_, keyval, _, state| {
            // Any modifier combo already means something else (Ctrl+F,
            // Alt+Tab, ...) — never intercept those.
            if state.intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK) {
                return gtk::glib::Propagation::Proceed;
            }
            let typing = gtk::prelude::RootExt::focus(&app.window).is_some_and(|w| w.is::<gtk::Text>());

            if keyval == gtk::gdk::Key::slash && !typing {
                app.search_bar.set_search_mode(true);
                app.search_entry.grab_focus();
                return gtk::glib::Propagation::Stop;
            }
            if typing {
                return gtk::glib::Propagation::Proceed;
            }
            let direction = match keyval {
                gtk::gdk::Key::j => Some(gtk::DirectionType::Down),
                gtk::gdk::Key::k => Some(gtk::DirectionType::Up),
                gtk::gdk::Key::h => Some(gtk::DirectionType::Left),
                gtk::gdk::Key::l => Some(gtk::DirectionType::Right),
                _ => None,
            };
            match direction {
                Some(direction) if app.window.child_focus(direction) => gtk::glib::Propagation::Stop,
                _ => gtk::glib::Propagation::Proceed,
            }
        });
        self.window.add_controller(controller);
    }

    fn install_window_actions(self: &Rc<Self>) {
        let preferences = gtk::gio::SimpleAction::new("preferences", None);
        let app = self.clone();
        preferences.connect_activate(move |_, _| preferences::present(&app));
        self.window.add_action(&preferences);

        let about = gtk::gio::SimpleAction::new("about", None);
        let app = self.clone();
        about.connect_activate(move |_, _| {
            let dialog = adw::AboutDialog::builder()
                .application_name("Portage Store")
                .application_icon("system-software-install")
                .version(env!("CARGO_PKG_VERSION"))
                .comments("An app store for Gentoo: search, install, and update Portage packages without a terminal.")
                .license_type(gtk::License::Gpl30)
                .build();
            dialog.present(Some(&app.window));
        });
        self.window.add_action(&about);

        // A stateful boolean action renders as a checkable item in the
        // menu automatically — no separate "is this checked" bookkeeping
        // needed beyond the action's own state.
        let github_preview = gtk::gio::SimpleAction::new_stateful(
            "github-preview",
            None,
            &self.settings.borrow().github_page_preview.to_variant(),
        );
        let app = self.clone();
        github_preview.connect_activate(move |action, _| {
            let enabled = !action.state().and_then(|v| v.get::<bool>()).unwrap_or(true);
            action.set_state(&enabled.to_variant());
            app.settings.borrow_mut().github_page_preview = enabled;
            settings::save(&app.settings.borrow());
        });
        self.window.add_action(&github_preview);

        let prefer_binpkg = gtk::gio::SimpleAction::new_stateful(
            "prefer-binpkg",
            None,
            &self.settings.borrow().prefer_binary_packages.to_variant(),
        );
        let app = self.clone();
        prefer_binpkg.connect_activate(move |action, _| {
            let enabled = !action.state().and_then(|v| v.get::<bool>()).unwrap_or(true);
            action.set_state(&enabled.to_variant());
            app.settings.borrow_mut().prefer_binary_packages = enabled;
            settings::save(&app.settings.borrow());
        });
        self.window.add_action(&prefer_binpkg);

        let throttle_builds = gtk::gio::SimpleAction::new_stateful(
            "throttle-builds",
            None,
            &self.settings.borrow().throttle_builds.to_variant(),
        );
        let app = self.clone();
        throttle_builds.connect_activate(move |action, _| {
            let enabled = !action.state().and_then(|v| v.get::<bool>()).unwrap_or(true);
            action.set_state(&enabled.to_variant());
            app.settings.borrow_mut().throttle_builds = enabled;
            settings::save(&app.settings.borrow());
        });
        self.window.add_action(&throttle_builds);

        let night_builds_only = gtk::gio::SimpleAction::new_stateful(
            "night-builds-only",
            None,
            &self.settings.borrow().night_builds_only.to_variant(),
        );
        let app = self.clone();
        night_builds_only.connect_activate(move |action, _| {
            let enabled = !action.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            action.set_state(&enabled.to_variant());
            app.settings.borrow_mut().night_builds_only = enabled;
            settings::save(&app.settings.borrow());
            // A toggle flipped on while a job is already deferred (or
            // about to start) should take effect immediately rather than
            // waiting for the next enqueue.
            if enabled && !app.running.get() && !app.queue.borrow().is_empty() {
                app.start_next();
            }
        });
        self.window.add_action(&night_builds_only);

        let periodic_health_checks = gtk::gio::SimpleAction::new_stateful(
            "periodic-health-checks",
            None,
            &self.settings.borrow().periodic_health_checks.to_variant(),
        );
        let app = self.clone();
        periodic_health_checks.connect_activate(move |action, _| {
            let enabled = !action.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            action.set_state(&enabled.to_variant());
            app.settings.borrow_mut().periodic_health_checks = enabled;
            settings::save(&app.settings.borrow());
        });
        self.window.add_action(&periodic_health_checks);

        let buildpkg_on_install = gtk::gio::SimpleAction::new_stateful(
            "buildpkg-on-install",
            None,
            &self.settings.borrow().buildpkg_on_install.to_variant(),
        );
        let app = self.clone();
        buildpkg_on_install.connect_activate(move |action, _| {
            let enabled = !action.state().and_then(|v| v.get::<bool>()).unwrap_or(false);
            action.set_state(&enabled.to_variant());
            app.settings.borrow_mut().buildpkg_on_install = enabled;
            settings::save(&app.settings.borrow());
        });
        self.window.add_action(&buildpkg_on_install);

        let health = gtk::gio::SimpleAction::new("health", None);
        let app = self.clone();
        health.connect_activate(move |_, _| health::present(&app));
        self.window.add_action(&health);

        let sync = gtk::gio::SimpleAction::new("sync", None);
        let app = self.clone();
        sync.connect_activate(move |_, _| {
            app.enqueue(QueueEntry {
                job: portage_store::portage::sync::sync_job(),
                label: "Syncing package tree".to_string(),
                mutating: true,
                retry_with_use_fix: false,
                known_atoms: Vec::new(),
            });
        });
        self.window.add_action(&sync);

        let cleanup = gtk::gio::SimpleAction::new("cleanup", None);
        let app = self.clone();
        cleanup.connect_activate(move |_, _| cleanup::present(&app));
        self.window.add_action(&cleanup);

        let depclean = gtk::gio::SimpleAction::new("depclean", None);
        let app = self.clone();
        depclean.connect_activate(move |_, _| depclean::present(&app));
        self.window.add_action(&depclean);

        // Reachable any time, not just on first run — revisiting it
        // later (a fresh install of a starter pick, or just wanting the
        // quick scan again) doesn't need `onboarding_shown` touched at
        // all, since that flag only ever gates the *automatic* showing.
        let onboarding_action = gtk::gio::SimpleAction::new("onboarding", None);
        let app = self.clone();
        onboarding_action.connect_activate(move |_, _| {
            let on_dismissed: Rc<dyn Fn()> = Rc::new(|| {});
            onboarding::present(&app, on_dismissed);
        });
        self.window.add_action(&onboarding_action);

        let presets_action = gtk::gio::SimpleAction::new("presets", None);
        let app = self.clone();
        presets_action.connect_activate(move |_, _| presets::present(&app));
        self.window.add_action(&presets_action);
    }

    fn connect_signals(
        self: &Rc<Self>,
        tiles: &gtk::FlowBox,
        update_all: &gtk::Button,
        refresh_button: &gtk::Button,
    ) {
        // Category tiles: FlowBox wraps each child, so the button we care
        // about is the flow child's own child.
        let mut index = 0usize;
        let mut child = tiles.first_child();
        while let Some(flow_child) = child {
            if let Some(button) = flow_child.first_child().and_downcast::<gtk::Button>() {
                let app = self.clone();
                let group = &CATEGORY_GROUPS[index];
                let name = group.name;
                let categories = group.categories;
                button.connect_clicked(move |_| app.browse_category(name, categories));
            }
            index += 1;
            child = flow_child.next_sibling();
        }

        let app = self.clone();
        self.search_entry.connect_search_changed(move |entry| {
            // Debounced: `eix::search` shells out to two `eix` processes
            // (name + description), so firing it on every single keystroke
            // — as opposed to once typing actually pauses — spawns a burst
            // of subprocesses that are almost all immediately superseded.
            let query = entry.text().to_string();
            app.search_debounce.set(app.search_debounce.get() + 1);
            let debounce_token = app.search_debounce.get();
            let app = app.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                if debounce_token == app.search_debounce.get() {
                    app.run_search(query);
                }
            });
        });

        let app = self.clone();
        update_all.connect_clicked(move |button| {
            let warnings = app.preflight_warnings();
            if warnings.is_empty() {
                app.start_update_all();
                return;
            }
            let Some(window) = button.root().and_downcast::<gtk::Window>() else {
                return;
            };
            let body = warnings.join("\n\n");
            let dialog = adw::AlertDialog::new(Some("Before You Start"), Some(&body));
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("continue", "Continue");
            dialog.set_response_appearance("continue", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let app = app.clone();
            dialog.connect_response(None, move |_, response| {
                if response == "continue" {
                    app.start_update_all();
                }
            });
            dialog.present(Some(&window));
        });

        let app = self.clone();
        refresh_button.connect_clicked(move |_| {
            app.rescan_installed();
            app.check_updates();
            app.check_news();
            app.check_config_protect();
            app.check_glsa();
            app.check_sync();
        });
    }

    pub fn present(&self) {
        self.window.present();
    }

    // --- data loading -------------------------------------------------

    /// Counts down the async startup tasks `root_stack` is waiting on
    /// (installed-package scan, featured carousels) and reveals the real
    /// content once the last one reports in.
    fn mark_startup_task_done(self: &Rc<Self>) {
        let remaining = self.startup_pending.get().saturating_sub(1);
        self.startup_pending.set(remaining);
        if remaining == 0 {
            self.root_stack.set_visible_child_name("content");
            self.startup_spinner.set_spinning(false);
            self.maybe_show_onboarding();
        }
    }

    /// Shows the first-run wizard once, the first time the real content
    /// is actually on screen — not immediately on `mark_startup_task_done`
    /// itself, since `check_updates()`'s own pretend run (kicked off
    /// around the same time as the two tasks gating this) is often still
    /// in flight then, and the wizard's own quick-scan numbers read
    /// straight off `pending_update_atoms` rather than watching for it.
    /// A short, fixed delay is a pragmatic best-effort here, not a
    /// guarantee — the same "Checking…" placeholder pattern used
    /// elsewhere would need `check_updates` itself reworked to notify a
    /// second listener, which is more machinery than a one-time welcome
    /// screen's own numbers being occasionally a beat stale justifies.
    fn maybe_show_onboarding(self: &Rc<Self>) {
        if self.settings.borrow().onboarding_shown {
            return;
        }
        let app = self.clone();
        gtk::glib::timeout_add_seconds_local_once(2, move || {
            let on_dismissed: Rc<dyn Fn()> = {
                let app = app.clone();
                Rc::new(move || {
                    app.settings.borrow_mut().onboarding_shown = true;
                    settings::save(&app.settings.borrow());
                })
            };
            onboarding::present(&app, on_dismissed);
        });
    }

    /// Fills the featured area with one named section per curated theme
    /// (Social, Games, Create, ...), stacked vertically — each section its
    /// own auto-advancing, hover-to-pause carousel, paginated 3x2 (six
    /// picks per page) so a theme with more than six popular picks just
    /// gets more pages instead of being capped. One `eix::lookup` per
    /// atom, batched into a single background call rather than one
    /// `spawn_blocking` per package, since resolving all of them
    /// sequentially off the main thread is cheap but dozens of separate
    /// thread hops for content that hasn't even been shown yet is not.
    /// Blocks are shuffled but mainstream ones are kept ahead of niche
    /// ones (see `order_blocks_popular_first`), so the first section on
    /// the page is always a familiar theme rather than "New & Updated"'s
    /// CLI tools.
    fn populate_featured_carousel(self: &Rc<Self>) {
        let blocks = widgets::order_blocks_popular_first(CURATED_BLOCKS);
        let app = self.clone();
        runtime::spawn_blocking(
            move || {
                blocks
                    .into_iter()
                    .map(|block| {
                        let picks = widgets::shuffled(block.atoms)
                            .into_iter()
                            .filter_map(|atom| eix::lookup(atom).ok().flatten())
                            .collect::<Vec<_>>();
                        (block.title, picks)
                    })
                    .collect::<Vec<_>>()
            },
            move |blocks| {
                let installed = app.installed.borrow();
                let icons = app.icon_paths.borrow();
                // Curated picks lacking a local icon match get primed from
                // Flathub, but queued here and fetched by one sequential
                // background worker afterwards (see below) rather than one
                // `spawn_blocking` per missing icon — dozens of concurrent
                // `curl` processes at startup was real, measurable jank for
                // no benefit, since nothing was waiting on all of them
                // finishing at once anyway.
                let mut icon_queue: HashMap<String, Vec<gtk::Image>> = HashMap::new();

                // Builds one carousel page (a 3x2 grid) from up to six
                // picks, wiring click-to-open on every card and returning
                // which of them still need their icon primed. Called once
                // per real page, plus twice more per multi-page section to
                // build the wrap-loop clones appended below.
                let build_page = |page_picks: &[PackageSummary], icon_queue: &mut HashMap<String, Vec<gtk::Image>>| {
                    let flow = widgets::grid();
                    // Without this, AdwCarousel sizes each page to its
                    // child's *natural* width rather than the carousel's
                    // own — since a 3x2 grid's natural width is narrower
                    // than the full carousel, the neighboring page peeked
                    // in at both edges instead of the current page filling
                    // the whole viewport on its own.
                    flow.set_hexpand(true);
                    for pkg in page_picks {
                        let (card, icon) = widgets::package_card(pkg, &installed, &icons);
                        if icons::resolve_by_name(&pkg.name).is_none() {
                            icon_queue.entry(pkg.name.clone()).or_default().push(icon);
                        }
                        let app = app.clone();
                        let pkg = pkg.clone();
                        card.connect_clicked(move |_| app.open_detail(pkg.clone()));
                        flow.insert(&card, -1);
                    }
                    flow
                };

                for (title, packages) in blocks {
                    let carousel = adw::Carousel::new();
                    // Two rows of package_card plus grid spacing — tall
                    // enough for a 3x2 page without the fixed-height
                    // description reservation leaving noticeable slack.
                    carousel.set_height_request(230);

                    let page_chunks: Vec<Vec<PackageSummary>> = packages.chunks(6).map(|chunk| chunk.to_vec()).collect();
                    let real_pages = page_chunks.len() as u32;

                    // A duplicate of the *last* real page glued on before
                    // the first, and a duplicate of the *first* glued on
                    // after the last — what makes scrolling past either
                    // end of the section loop straight around instead of
                    // sliding backwards through everything in between.
                    // See `install_carousel_behavior` for how landing on
                    // one of these gets silently swapped for the real page
                    // it duplicates.
                    if real_pages > 1 {
                        carousel.append(&build_page(&page_chunks[page_chunks.len() - 1], &mut icon_queue));
                    }

                    let mut dots = Vec::with_capacity(page_chunks.len());
                    for chunk in &page_chunks {
                        carousel.append(&build_page(chunk, &mut icon_queue));
                        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                        dot.add_css_class("carousel-dot");
                        dots.push(dot);
                    }

                    if real_pages > 1 {
                        carousel.append(&build_page(&page_chunks[0], &mut icon_queue));
                    }

                    let dots_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                    dots_row.set_halign(gtk::Align::Center);
                    for dot in &dots {
                        dots_row.append(dot);
                    }
                    if let Some(first) = dots.first() {
                        first.add_css_class("carousel-dot-active");
                    }

                    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
                    section.append(&widgets::section_heading(title));
                    section.append(&carousel);
                    if real_pages > 1 {
                        section.append(&dots_row);
                    }
                    app.featured_section.append(&section);

                    if real_pages > 1 {
                        // Skips straight past the leading clone at index 0
                        // to the real first page at index 1 — nothing has
                        // been shown yet at this point, so there's nothing
                        // for an animation to visibly interrupt.
                        carousel.scroll_to(&carousel.nth_page(1), false);
                        install_carousel_behavior(&carousel, real_pages, dots);
                    } else {
                        install_scroll_guard(&carousel);
                    }
                }

                if !icon_queue.is_empty() {
                    let names: Vec<String> = icon_queue.keys().cloned().collect();
                    runtime::spawn_stream(
                        move |tx| {
                            for name in names {
                                let path = (|| {
                                    let app = portage_store::portage::flathub::lookup(&name)?;
                                    portage_store::portage::media::fetch(&app.icon?)
                                })();
                                if tx.send_blocking((name, path)).is_err() {
                                    break;
                                }
                            }
                        },
                        move |(name, path)| {
                            let Some(path) = path else { return };
                            if let Some(images) = icon_queue.get(&name) {
                                for icon in images {
                                    icon.set_from_file(Some(&path));
                                    icon.remove_css_class("icon-fallback");
                                }
                            }
                        },
                    );
                }

                app.mark_startup_task_done();
            },
        );
    }

    fn rescan_installed(self: &Rc<Self>) {
        self.rescan_installed_then(|_| {});
    }

    /// As `rescan_installed`, but runs `after` once `app.installed` and
    /// `app.icon_paths` actually hold the fresh scan — needed by
    /// `start_next` below, which used to call `rescan_installed()` and
    /// immediately rebuild the just-installed package's detail page
    /// right after: since the scan runs on a background thread and only
    /// updates `installed` once *its own* completion callback runs later,
    /// that rebuild was reading the *old* map and showing the exact same
    /// "Install" button as before, rather than "Remove".
    fn rescan_installed_then(self: &Rc<Self>, after: impl Fn(&Rc<Self>) + 'static) {
        let app = self.clone();
        runtime::spawn_blocking(
            || {
                let installed = scan_installed();
                let icons = resolve_icons(&installed);
                (installed, icons)
            },
            move |(installed, icons)| {
                *app.installed.borrow_mut() = installed;
                *app.icon_paths.borrow_mut() = icons;
                app.populate_installed();
                app.mark_startup_task_done();
                after(&app);
            },
        );
    }

    /// Fills both the Installed tab and the "Your Apps" showcase on
    /// the landing page. The showcase deliberately only lists packages with
    /// real artwork — those are the desktop apps, and a wall of real logos
    /// is what makes the landing page look like a store rather than a
    /// package list.
    fn populate_installed(self: &Rc<Self>) {
        let installed = self.installed.borrow();
        let icons = self.icon_paths.borrow();

        widgets::clear(&self.installed_grid);
        widgets::clear(&self.yours_grid);

        let mut all: Vec<&InstalledPackage> = installed.values().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));

        for pkg in all.iter().take(400) {
            let summary = PackageSummary {
                category: pkg.category.clone(),
                name: pkg.name.clone(),
                // The package database records no description, so the
                // category is the most useful thing to show until the
                // detail page looks the package up in the tree.
                description: pkg.category.clone(),
                homepage: String::new(),
                license: String::new(),
                latest_version: pkg.version.clone(),
                iuse: Vec::new(),
                masked: false,
                overlay: None,
                slot: None,
            };
            let (card, _icon) = widgets::package_card(&summary, &installed, &icons);
            self.connect_card(&card, summary);
            self.installed_grid.insert(&card, -1);
        }

        for pkg in all.iter().filter(|p| icons.contains_key(&format!("{}/{}", p.category, p.name))).take(24) {
            let summary = PackageSummary {
                category: pkg.category.clone(),
                name: pkg.name.clone(),
                description: pkg.category.clone(),
                homepage: String::new(),
                license: String::new(),
                latest_version: pkg.version.clone(),
                iuse: Vec::new(),
                masked: false,
                overlay: None,
                slot: None,
            };
            let (card, _icon) = widgets::package_card(&summary, &installed, &icons);
            self.connect_card(&card, summary);
            self.yours_grid.insert(&card, -1);
        }

        self.installed_stack.set_visible_child_name("list");

        // Cards land in the grid after the window is already up, and the
        // scroller chases whichever one takes focus — leaving the landing
        // page parked halfway down the category grid on launch.
        self.scroll_to_top();
    }




















}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portage_error_markers_are_recognized() {
        assert!(is_log_error_line("!!! Multiple package instances within a single package slot"));
        assert!(is_log_error_line(" * ERROR: dev-libs/boost-1.86.0::gentoo failed (compile phase)"));
        assert!(is_log_error_line("configure: error: C compiler cannot create executables"));
    }

    #[test]
    fn flatpak_error_markers_are_recognized() {
        assert!(is_log_error_line("error: No remote refs found for 'flathub'"));
    }

    #[test]
    fn routine_output_is_not_flagged_as_an_error() {
        assert!(!is_log_error_line(" * Messages for package dev-libs/boost-1.86.0:"));
        assert!(!is_log_error_line(">>> Jobs: 3 of 17, 1 complete"));
        assert!(!is_log_error_line("Installing… ████████            13%"));
        assert!(!is_log_error_line("USE=\"error-reporting\" dev-lang/rust"));
    }
}
