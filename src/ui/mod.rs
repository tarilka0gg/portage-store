mod build_failure;
mod cleanup;
mod config_update;
mod depclean;
mod detail;
mod health;
mod news;
mod why_installed;
mod preferences;
mod runtime;
mod settings;
mod webview;
mod widgets;

use crate::portage::eix::{self, PackageSummary};
use crate::portage::emerge::{self, Job};
use crate::portage::config_protect::PendingUpdate;
use crate::portage::installed::{self, InstalledPackage};
use crate::portage::icons;
use crate::portage::news::NewsItem;
use crate::portage::package_use;
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

/// One of the three "you need to relax something to proceed" blocks
/// portage prints in an identical shape (see `emerge::parse_required_*`),
/// detected from a failed job's own output and offered as an apply-and-
/// retry dialog rather than just reported as a bare failure — the exact
/// same treatment already proven for USE flags, generalized to the two
/// other cases that hit the identical dead end.
#[derive(Clone)]
enum PendingRelaxation {
    Use(Vec<(String, String, bool)>),
    Keyword(Vec<(String, String)>),
    License(Vec<(String, Vec<String>)>),
}

impl PendingRelaxation {
    /// Checked in the same order portage itself prints the blocks
    /// (keyword, then USE, then license — see the real output this is
    /// parsed from) — not that the order matters for correctness, since
    /// each parser only ever matches its own block, but this is only
    /// ever used to prompt for *one* fix at a time even if a run somehow
    /// needed more than one kind, and starting with keywords first is as
    /// good a choice as any.
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
        None
    }

    fn dialog_title(&self) -> &'static str {
        match self {
            Self::Use(_) => "USE flag changes needed",
            Self::Keyword(_) => "Keyword changes needed",
            Self::License(_) => "License acceptance needed",
        }
    }

    /// A short noun phrase for slotting into "applying {this}, retrying"
    /// — `dialog_title` reads fine as a heading but awkwardly mid-sentence.
    fn noun_phrase(&self) -> &'static str {
        match self {
            Self::Use(_) => "the required USE changes",
            Self::Keyword(_) => "the required keyword changes",
            Self::License(_) => "the required license changes",
        }
    }

    /// What this relaxation is for, worded to slot directly after
    /// "{label} " in the confirmation dialog's body.
    fn intro(&self) -> &'static str {
        match self {
            Self::Use(_) => "needs these USE flag changes on a dependency before it can proceed:",
            Self::Keyword(_) => "needs these keyword changes before it can proceed:",
            Self::License(_) => "needs these licenses accepted before it can proceed:",
        }
    }

    fn body_lines(&self) -> Vec<String> {
        match self {
            Self::Use(changes) => changes
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
            Self::Use(changes) => {
                for (atom, flag, enabled) in changes {
                    package_use::set_flag(atom, flag, *enabled)?;
                }
            }
            Self::Keyword(changes) => {
                for (atom, keyword) in changes {
                    crate::portage::package_keywords::accept(atom, keyword)?;
                }
            }
            Self::License(changes) => {
                for (atom, licenses) in changes {
                    for license in licenses {
                        crate::portage::package_license::accept(atom, license)?;
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

    installed: RefCell<HashMap<String, InstalledPackage>>,
    icon_paths: RefCell<HashMap<String, PathBuf>>,
    queue: RefCell<VecDeque<QueueEntry>>,
    running: Cell<bool>,
    /// Bumped per search so results from an overtaken keystroke get dropped
    /// instead of clobbering newer ones.
    search_generation: Cell<u64>,
    /// Bumped on every keystroke in the search entry, independently of
    /// `search_generation` — lets a debounce timer (see `connect_search_changed`
    /// below) tell whether it's still the most recent keystroke once its
    /// delay elapses, without touching `search_generation`'s own bookkeeping
    /// for in-flight background lookups.
    search_debounce: Cell<u64>,
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

/// Pulls the atoms out of `emerge --pretend` output lines, which look like
/// `[ebuild   U  ] cat/name-1.2 [1.1] USE="..."`.
fn parse_update_atoms(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            if !trimmed.starts_with("[ebuild") && !trimmed.starts_with("[binary") {
                return None;
            }
            let after_bracket = trimmed.split_once(']')?.1.trim();
            let atom_with_version = after_bracket.split_whitespace().next()?;
            Some(atom_with_version.to_string())
        })
        .collect()
}

/// Strips the trailing `-<version>` off a `category/name-version` string
/// (as `parse_update_atoms` returns) to get the bare atom `emerge` expects
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
        let results = page_box();
        results.append(&results_heading_row);
        results.append(&results_spinner);
        results.append(&results_grid);

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

        let maintenance_menu = gtk::gio::Menu::new();
        maintenance_menu.append(Some("System Health"), Some("win.health"));
        maintenance_menu.append(Some("Sync Package Tree"), Some("win.sync"));
        maintenance_menu.append(Some("Free Up Space"), Some("win.cleanup"));
        maintenance_menu.append(Some("Remove Orphaned Packages"), Some("win.depclean"));

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

        let job_text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        job_text.set_hexpand(true);
        job_text.append(&job_label);
        job_text.append(&job_progress);
        job_text.append(&job_eta);
        job_text.append(&job_log);

        // Opens a popover listing everything waiting behind the current
        // job — cancel a queued job outright, or bump one to the front
        // ahead of a long `@world` update, without needing to touch (or
        // kill) whatever's actually running right now.
        let queue_button = gtk::Button::from_icon_name("view-list-symbolic");
        queue_button.add_css_class("flat");
        queue_button.set_valign(gtk::Align::Center);
        queue_button.set_tooltip_text(Some("View queued jobs"));

        let job_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        job_box.set_margin_top(10);
        job_box.set_margin_bottom(10);
        job_box.set_margin_start(14);
        job_box.set_margin_end(14);
        job_box.append(&job_spinner);
        job_box.append(&job_text);
        job_box.append(&queue_button);

        let job_revealer = gtk::Revealer::builder().child(&job_box).build();

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
        toolbar.add_bottom_bar(&job_revealer);

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
            installed: RefCell::new(HashMap::new()),
            icon_paths: RefCell::new(HashMap::new()),
            queue: RefCell::new(VecDeque::new()),
            running: Cell::new(false),
            search_generation: Cell::new(0),
            search_debounce: Cell::new(0),
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
                    job: crate::portage::sync::sync_job(),
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
            let popover = app.build_filter_popover();
            app.filter_button.set_popover(Some(&popover));
        }
        app.install_header_gesture(&header);
        app.install_window_actions();
        app.connect_signals(&tiles, &update_all, &refresh_button);
        app.rescan_installed();
        app.check_updates();
        app.check_news();
        app.check_config_protect();
        app.check_glsa();
        app.check_sync();
        app.populate_featured_carousel();
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

        let health = gtk::gio::SimpleAction::new("health", None);
        let app = self.clone();
        health.connect_activate(move |_, _| health::present(&app));
        self.window.add_action(&health);

        let sync = gtk::gio::SimpleAction::new("sync", None);
        let app = self.clone();
        sync.connect_activate(move |_, _| {
            app.enqueue(QueueEntry {
                job: crate::portage::sync::sync_job(),
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
        update_all.connect_clicked(move |_| {
            app.enqueue(QueueEntry {
                job: emerge::update_world_job(app.settings.borrow().prefer_binary_packages),
                label: "System update (@world)".to_string(),
                mutating: true,
                retry_with_use_fix: false,
                known_atoms: app.pending_update_atoms.borrow().clone(),
            });
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
        }
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
                                    let app = crate::portage::flathub::lookup(&name)?;
                                    crate::portage::media::fetch(&app.icon?)
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

    fn check_updates(self: &Rc<Self>) {
        self.updates_stack.set_visible_child_name("checking");
        let lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let collect = lines.clone();
        let app = self.clone();
        runtime::spawn_job(
            emerge::pretend_world_job(self.settings.borrow().prefer_binary_packages),
            move |line| collect.borrow_mut().push(line),
            move |_success| {
                let atoms = parse_update_atoms(&lines.borrow());
                let download_kib = emerge::parse_pretend_output(&lines.borrow()).download_kib;
                app.show_updates(atoms, download_kib);
            },
        );
    }

    /// Checks for unread Gentoo news (GLEP-42 items — profile migrations,
    /// dropped defaults, anything portage itself would print a "N news
    /// items need reading" reminder about) and reveals `news_banner` if
    /// there are any. Re-run on startup and on manual refresh; unlike
    /// updates, nothing else in this app changes what's unread, so there's
    /// no reason to also re-check after a job finishes.
    fn check_news(self: &Rc<Self>) {
        let app = self.clone();
        runtime::spawn_blocking(
            crate::portage::news::list,
            move |result| {
                let items = result.unwrap_or_default();
                let unread = items.iter().filter(|i| i.unread).count();
                *app.news_items.borrow_mut() = items;
                if unread == 0 {
                    app.news_banner.set_revealed(false);
                    return;
                }
                app.news_banner.set_title(&if unread == 1 {
                    "1 Gentoo news item to read".to_string()
                } else {
                    format!("{unread} Gentoo news items to read")
                });
                app.news_banner.set_revealed(true);
            },
        );
    }

    /// Scans every `CONFIG_PROTECT` root for pending `._cfgNNNN_name`
    /// files — updates portage wrote beside a config file it wouldn't
    /// overwrite in place — and reveals `config_protect_banner` if there
    /// are any. Re-run on startup, manual refresh, and after any
    /// mutating job succeeds, since that's exactly when new ones show up.
    fn check_config_protect(self: &Rc<Self>) {
        let app = self.clone();
        runtime::spawn_blocking(crate::portage::config_protect::scan, move |items| {
            let count = items.len();
            *app.config_protect_items.borrow_mut() = items;
            if count == 0 {
                app.config_protect_banner.set_revealed(false);
                return;
            }
            app.config_protect_banner.set_title(&if count == 1 {
                "1 config file needs review".to_string()
            } else {
                format!("{count} config files need review")
            });
            app.config_protect_banner.set_revealed(true);
        });
    }

    /// Checks `glsa-check` for security advisories affecting what's
    /// actually installed — a category no default Gentoo install surfaces
    /// anywhere without already knowing `app-portage/gentoolkit`'s
    /// `glsa-check` exists. Shown as its own section above the ordinary
    /// update list, plus a badge on the Updates tab itself so it's
    /// visible without opening the tab at all.
    fn check_glsa(self: &Rc<Self>) {
        let app = self.clone();
        runtime::spawn_blocking(crate::portage::glsa::list_affected, move |result| {
            let entries = result.unwrap_or_default();

            while let Some(row) = app.security_list.first_child() {
                app.security_list.remove(&row);
            }

            if entries.is_empty() {
                app.security_section.set_visible(false);
                app.updates_view_page.set_badge_number(0);
                app.updates_view_page.set_needs_attention(false);
                return;
            }

            app.updates_view_page.set_badge_number(entries.len() as u32);
            app.updates_view_page.set_needs_attention(true);
            app.security_section.set_visible(true);

            for entry in entries {
                let row = adw::ActionRow::builder()
                    .title(format!("{} — {}", entry.id, entry.description))
                    .subtitle(entry.packages.join(", "))
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("security-high-symbolic"));

                let update_button = gtk::Button::with_label("Update");
                update_button.add_css_class("destructive-action");
                update_button.set_valign(gtk::Align::Center);
                let app_for_click = app.clone();
                let packages = entry.packages.clone();
                update_button.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    for atom in &packages {
                        app_for_click.enqueue(QueueEntry {
                            job: emerge::install_job(atom, app_for_click.settings.borrow().prefer_binary_packages),
                            label: format!("Security update: {atom}"),
                            mutating: true,
                            retry_with_use_fix: true,
                            known_atoms: Vec::new(),
                        });
                    }
                });
                row.add_suffix(&update_button);

                app.security_list.append(&row);
            }
        });
    }

    /// How stale the tree can get before "Updates" is answering from
    /// data old enough to be actively misleading — a week is a
    /// commonly-cited rule of thumb for how often Gentoo's own
    /// documentation suggests syncing, not an arbitrary number.
    const STALE_SYNC_SECONDS: u64 = 7 * 24 * 60 * 60;

    fn check_sync(self: &Rc<Self>) {
        let app = self.clone();
        runtime::spawn_blocking(crate::portage::sync::seconds_since_last_sync, move |age| {
            match age {
                Some(seconds) if seconds >= Self::STALE_SYNC_SECONDS => {
                    app.sync_banner.set_title(&format!(
                        "Package tree last synced {} — \"Updates\" may be out of date",
                        crate::portage::sync::format_age(seconds)
                    ));
                    app.sync_banner.set_revealed(true);
                }
                _ => app.sync_banner.set_revealed(false),
            }
        });
    }

    fn show_updates(self: &Rc<Self>, atoms: Vec<String>, download_kib: Option<u64>) {
        while let Some(row) = self.updates_list.first_child() {
            self.updates_list.remove(&row);
        }
        // Kept around for "Update All" to hand off as `known_atoms` — the
        // one place in the app that already has the full pending-update
        // atom list on hand before the job that would need it even
        // starts, letting it show a real upfront ETA instead of none.
        *self.pending_update_atoms.borrow_mut() = atoms.clone();
        if atoms.is_empty() {
            self.updates_stack.set_visible_child_name("uptodate");
            return;
        }
        self.updates_subtitle
            .set_text(&format!("{} packages will be updated", atoms.len()));

        match download_kib.and_then(crate::portage::diskspace::low_space_warning) {
            Some(warning) => {
                self.updates_space_banner.set_title(&warning);
                self.updates_space_banner.set_revealed(true);
            }
            None => self.updates_space_banner.set_revealed(false),
        }
        for atom_with_version in atoms {
            let row = adw::ActionRow::builder().title(&atom_with_version).build();
            row.add_prefix(&gtk::Image::from_icon_name("software-update-available-symbolic"));

            // Lets one package be updated on its own instead of only via
            // "Update All" — useful when only one update is wanted right
            // now (e.g. everything else would pull in a long rebuild).
            let update_button = gtk::Button::with_label("Update");
            update_button.add_css_class("flat");
            update_button.set_valign(gtk::Align::Center);
            let bare_atom = strip_version_suffix(&atom_with_version);
            let app = self.clone();
            update_button.connect_clicked(move |button| {
                button.set_sensitive(false);
                app.enqueue(QueueEntry {
                    job: emerge::install_job(&bare_atom, app.settings.borrow().prefer_binary_packages),
                    label: format!("Updating {bare_atom}"),
                    mutating: true,
                    retry_with_use_fix: true,
                    known_atoms: Vec::new(),
                });
            });
            row.add_suffix(&update_button);

            self.updates_list.append(&row);
        }
        self.updates_stack.set_visible_child_name("list");
    }

    // --- browsing -----------------------------------------------------

    fn run_search(self: &Rc<Self>, query: String) {
        if query.trim().len() < 2 {
            self.explore_stack.set_visible_child_name("landing");
            return;
        }
        self.search_generation.set(self.search_generation.get() + 1);
        let generation = self.search_generation.get();

        self.results_heading.set_text(&format!("Results for \u{201c}{query}\u{201d}"));
        self.explore_stack.set_visible_child_name("results");
        widgets::clear(&self.results_grid);
        self.results_spinner.start();
        self.results_spinner.set_visible(true);
        self.scroll_to_top();

        let app = self.clone();
        runtime::spawn_blocking(
            move || eix::search(&query).map_err(|e| e.to_string()),
            move |result| {
                if generation != app.search_generation.get() {
                    return;
                }
                app.show_results(result);
            },
        );
    }

    fn browse_category(self: &Rc<Self>, name: &'static str, categories: &'static [&'static str]) {
        self.search_generation.set(self.search_generation.get() + 1);
        let generation = self.search_generation.get();

        self.search_entry.set_text("");
        self.search_bar.set_search_mode(false);
        self.results_heading.set_text(name);
        self.explore_stack.set_visible_child_name("results");
        widgets::clear(&self.results_grid);
        self.results_spinner.start();
        self.results_spinner.set_visible(true);
        self.scroll_to_top();

        let app = self.clone();
        runtime::spawn_blocking(
            move || eix::list_categories(categories).map_err(|e| e.to_string()),
            move |result| {
                if generation != app.search_generation.get() {
                    return;
                }
                app.show_results(result);
            },
        );
    }

    /// Puts the browse view back at the top. Called whenever what is on
    /// screen changes wholesale — a new search, a new category — because
    /// otherwise the previous scroll position carries over and the fresh
    /// results open somewhere in their middle.
    fn scroll_to_top(&self) {
        self.explore_scroller.vadjustment().set_value(0.0);
    }

    /// The "take me back" gesture, double-click-on-the-tab triggered.
    ///
    /// A package detail page always pops back to the tab it was opened
    /// from first, regardless of which tab that is. From there, behaviour
    /// depends on *which* tab is showing: Explore has a real home (the
    /// category-tile landing page) to unwind to, so it goes landing, then
    /// top. Installed and Updates have no such second screen — there's
    /// nothing to navigate to below their single list — so double-clicking
    /// either just scrolls that list to the top, the same as double-
    /// clicking Explore once it's already on the landing page.
    fn home_or_top(self: &Rc<Self>) {
        if self.nav.visible_page().and_then(|p| p.tag()).as_deref() != Some("main") {
            self.nav.pop_to_tag("main");
            return;
        }
        match self.view_stack.visible_child_name().as_deref() {
            Some("installed") => self.installed_scroller.vadjustment().set_value(0.0),
            Some("updates") => self.updates_scroller.vadjustment().set_value(0.0),
            _ => {
                if self.explore_stack.visible_child_name().as_deref() != Some("landing") {
                    self.search_entry.set_text("");
                    self.search_bar.set_search_mode(false);
                    self.browsing_landing();
                } else {
                    self.scroll_to_top();
                }
            }
        }
    }

    fn browsing_landing(self: &Rc<Self>) {
        self.search_generation.set(self.search_generation.get() + 1);
        self.explore_stack.set_visible_child_name("landing");
        self.scroll_to_top();
    }

    fn show_results(self: &Rc<Self>, result: Result<Vec<PackageSummary>, String>) {
        self.results_spinner.stop();
        self.results_spinner.set_visible(false);

        let packages = match result {
            Ok(packages) => packages,
            Err(err) => {
                widgets::clear(&self.results_grid);
                self.toast(&format!("Search failed: {err}"));
                return;
            }
        };

        *self.last_results.borrow_mut() = packages;
        self.render_filtered_results();
    }

    /// Re-applies the current `search_filters` (and sort order) to
    /// `last_results` and rebuilds `results_grid` from scratch. Called
    /// both after a fresh search/category fetch and whenever a filter
    /// control changes — filtering is pure and in-memory, so there's
    /// nothing to await here.
    fn render_filtered_results(self: &Rc<Self>) {
        widgets::clear(&self.results_grid);

        let base_count = self.last_results.borrow().len();
        let filtered = eix::apply_filters(self.last_results.borrow().clone(), &self.search_filters.borrow());

        if base_count == 0 {
            self.results_heading.set_text("No results — try a different search term");
            return;
        }
        if filtered.is_empty() {
            self.results_heading.set_text("No results match the current filters");
            return;
        }
        if filtered.len() != base_count {
            self.results_heading.set_text(&format!("{} of {} results match the current filters", filtered.len(), base_count));
        }

        let installed = self.installed.borrow();
        let icons = self.icon_paths.borrow();
        for pkg in filtered.into_iter().take(300) {
            let (card, _icon) = widgets::package_card(&pkg, &installed, &icons);
            self.connect_card(&card, pkg);
            self.results_grid.insert(&card, -1);
        }
        self.scroll_to_top();
    }

    /// The funnel popover next to search results: USE flag presence,
    /// masked/unmasked, overlay-only, license substring, and sort — every
    /// control writes straight into `search_filters` and re-renders
    /// immediately via `render_filtered_results`, since filtering never
    /// needs to touch `eix` again once a result list is in hand.
    fn build_filter_popover(self: &Rc<Self>) -> gtk::Popover {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        column.set_margin_top(12);
        column.set_margin_bottom(12);
        column.set_margin_start(12);
        column.set_margin_end(12);
        column.set_width_request(280);

        let use_row = adw::EntryRow::builder().title("USE flag").build();
        let use_mode = gtk::DropDown::from_strings(&["Has flag", "Lacks flag"]);
        use_mode.set_margin_top(4);

        let masked_mode = gtk::DropDown::from_strings(&["Any", "Masked only", "Unmasked only"]);
        let masked_label = gtk::Label::new(Some("Masked"));
        masked_label.set_xalign(0.0);
        masked_label.add_css_class("dim-label");
        masked_label.add_css_class("caption");

        let overlay_only = gtk::CheckButton::with_label("Overlay packages only");

        let license_row = adw::EntryRow::builder().title("License contains").build();

        let sort_label = gtk::Label::new(Some("Sort by"));
        sort_label.set_xalign(0.0);
        sort_label.add_css_class("dim-label");
        sort_label.add_css_class("caption");
        let sort_mode = gtk::DropDown::from_strings(&["Relevance", "Name (A–Z)", "Name (Z–A)", "License"]);

        let reset_button = gtk::Button::with_label("Reset Filters");
        reset_button.add_css_class("flat");

        column.append(&use_row);
        column.append(&use_mode);
        column.append(&masked_label);
        column.append(&masked_mode);
        column.append(&overlay_only);
        column.append(&license_row);
        column.append(&sort_label);
        column.append(&sort_mode);
        column.append(&reset_button);

        let apply: Rc<dyn Fn()> = Rc::new({
            let app = self.clone();
            let use_row = use_row.clone();
            let use_mode = use_mode.clone();
            let masked_mode = masked_mode.clone();
            let overlay_only = overlay_only.clone();
            let license_row = license_row.clone();
            let sort_mode = sort_mode.clone();
            let filter_button = self.filter_button.clone();
            move || {
                let flag = use_row.text().trim().to_string();
                let use_flag = (!flag.is_empty())
                    .then(|| eix::UseConstraint { flag, must_be_set: use_mode.selected() == 0 });
                let masked = match masked_mode.selected() {
                    1 => Some(true),
                    2 => Some(false),
                    _ => None,
                };
                let license = license_row.text().trim().to_string();
                let sort = match sort_mode.selected() {
                    1 => eix::SortOrder::NameAsc,
                    2 => eix::SortOrder::NameDesc,
                    3 => eix::SortOrder::LicenseAsc,
                    _ => eix::SortOrder::Default,
                };
                let filters = eix::SearchFilters {
                    use_flag,
                    masked,
                    overlay_only: overlay_only.is_active(),
                    license: (!license.is_empty()).then_some(license),
                    sort,
                };
                // A visual cue that a filter is active — otherwise a
                // filtered-down result list with no obvious cause looks
                // like a bug rather than a deliberate narrowing.
                if filters.is_default() {
                    filter_button.remove_css_class("suggested-action");
                } else {
                    filter_button.add_css_class("suggested-action");
                }
                *app.search_filters.borrow_mut() = filters;
                app.render_filtered_results();
            }
        });

        {
            let apply = apply.clone();
            use_row.connect_changed(move |_| apply());
        }
        {
            let apply = apply.clone();
            use_mode.connect_selected_notify(move |_| apply());
        }
        {
            let apply = apply.clone();
            masked_mode.connect_selected_notify(move |_| apply());
        }
        {
            let apply = apply.clone();
            overlay_only.connect_toggled(move |_| apply());
        }
        {
            let apply = apply.clone();
            license_row.connect_changed(move |_| apply());
        }
        {
            let apply = apply.clone();
            sort_mode.connect_selected_notify(move |_| apply());
        }
        {
            let apply = apply.clone();
            reset_button.connect_clicked(move |_| {
                use_row.set_text("");
                use_mode.set_selected(0);
                masked_mode.set_selected(0);
                overlay_only.set_active(false);
                license_row.set_text("");
                sort_mode.set_selected(0);
                apply();
            });
        }

        let popover = gtk::Popover::new();
        popover.set_child(Some(&column));
        popover
    }

    fn connect_card(self: &Rc<Self>, card: &gtk::Button, pkg: PackageSummary) {
        let app = self.clone();
        card.connect_clicked(move |_| app.open_detail(pkg.clone()));
    }



    fn open_detail(self: &Rc<Self>, pkg: PackageSummary) {
        // Pushed immediately so there's something to look at while both
        // `eix::lookup` below and the detail page's own description/
        // screenshot enrichment (Flathub, then Terminal Trove/GitHub if
        // still needed) are in flight — swapped for the real page in
        // `on_ready` below once that settles, rather than pushing the real
        // page right away and letting its content visibly fill in piece by
        // piece.
        let loading_page = loading_navigation_page();
        self.nav.push(&loading_page);

        // eix's search results carry no IUSE for packages matched by
        // description, so re-look the package up to get full metadata.
        let atom = pkg.atom();
        let app = self.clone();
        runtime::spawn_blocking(
            move || eix::lookup(&atom).ok().flatten(),
            move |full| {
                let pkg = full.unwrap_or_else(|| pkg.clone());
                let install_app = app.clone();
                let uninstall_app = app.clone();
                let sandbox_app = app.clone();

                // `on_ready` needs the built page to push it, but the page
                // isn't built until `detail::build` returns — which itself
                // needs `on_ready` to pass in. Broken by routing the page
                // through this cell instead of capturing it directly: by
                // the time `on_ready` can actually run (asynchronously,
                // after this whole function returns), `page` below has
                // long since been stored in it.
                let held_page: Rc<RefCell<Option<adw::NavigationPage>>> = Rc::new(RefCell::new(None));
                let held_page_for_ready = held_page.clone();
                let nav_for_ready = app.nav.clone();
                let on_ready: Rc<dyn Fn()> = Rc::new(move || {
                    if let Some(page) = held_page_for_ready.borrow_mut().take() {
                        nav_for_ready.pop();
                        nav_for_ready.push(&page);
                    }
                });

                let page = detail::build(
                    &pkg,
                    &app.installed.borrow(),
                    &app.icon_paths.borrow(),
                    app.settings.borrow().prefer_binary_packages,
                    Rc::new(move |atom: String| install_app.install(atom)),
                    Rc::new(move |atom: String| uninstall_app.uninstall(atom)),
                    Rc::new(move |atom: String| sandbox_app.sandbox_build(atom)),
                    on_ready,
                );
                *held_page.borrow_mut() = Some(page);
            },
        );
    }

    // --- jobs ----------------------------------------------------------

    fn install(self: &Rc<Self>, atom: String) {
        self.enqueue(QueueEntry {
            job: emerge::install_job(&atom, self.settings.borrow().prefer_binary_packages),
            label: format!("Installing {atom}"),
            mutating: true,
            retry_with_use_fix: true,
            known_atoms: Vec::new(),
        });
    }

    fn uninstall(self: &Rc<Self>, atom: String) {
        self.enqueue(QueueEntry {
            job: emerge::uninstall_job(&atom),
            label: format!("Removing {atom}"),
            mutating: true,
            retry_with_use_fix: false,
            known_atoms: Vec::new(),
        });
    }

    /// Queues a build of `atom` in the isolated sandbox chroot (see
    /// `portage::sandbox`) rather than the live system — offered from the
    /// detail page only once a normal `--pretend` has already failed.
    /// `mutating: false`: unlike a real install, this never touches what's
    /// actually installed on the host, so there's nothing for a rescan
    /// afterwards to pick up.
    fn sandbox_build(self: &Rc<Self>, atom: String) {
        match crate::portage::sandbox::build_job(&atom) {
            Ok(job) => self.enqueue(QueueEntry {
                job,
                label: format!("Sandbox build: {atom}"),
                mutating: false,
                retry_with_use_fix: false,
                known_atoms: Vec::new(),
            }),
            Err(err) => self.toast(&format!("Couldn't start sandbox build: {err}")),
        }
    }

    /// Removes an outright-wasted queued job — it never got to run at
    /// all, so unlike cancelling something in progress there's nothing
    /// destructive here to warn about.
    fn cancel_queued(self: &Rc<Self>, index: usize) {
        let removed = self.queue.borrow_mut().remove(index);
        if let Some(entry) = removed {
            self.toast(&format!("{} — removed from queue", entry.label));
        }
    }

    /// Moves a queued job to the very front — the "an urgent single
    /// install shouldn't have to wait behind a multi-hour `@world`
    /// update" case. Never touches whatever's currently running; the
    /// bumped job simply becomes the *next* one `start_next` picks up.
    fn prioritize_queued(self: &Rc<Self>, index: usize) {
        let mut queue = self.queue.borrow_mut();
        if let Some(entry) = queue.remove(index) {
            queue.push_front(entry);
        }
    }

    /// Shows what's waiting behind the current job — the queue is
    /// otherwise invisible beyond a toast's passing "queued (N)" message
    /// at the moment something's added to it.
    fn present_queue_popover(self: &Rc<Self>, anchor: &gtk::Button) {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);

        let popover = gtk::Popover::new();

        let queue = self.queue.borrow();
        if queue.is_empty() {
            list.append(&adw::ActionRow::builder().title("Nothing queued").build());
        }
        for (index, entry) in queue.iter().enumerate() {
            let row = adw::ActionRow::builder().title(&entry.label).build();

            if index > 0 {
                let bump = gtk::Button::from_icon_name("go-top-symbolic");
                bump.add_css_class("flat");
                bump.set_valign(gtk::Align::Center);
                bump.set_tooltip_text(Some("Move to front"));
                let app = self.clone();
                let popover_for_bump = popover.clone();
                bump.connect_clicked(move |_| {
                    app.prioritize_queued(index);
                    popover_for_bump.popdown();
                });
                row.add_suffix(&bump);
            }

            let cancel = gtk::Button::from_icon_name("edit-delete-symbolic");
            cancel.add_css_class("flat");
            cancel.set_valign(gtk::Align::Center);
            cancel.set_tooltip_text(Some("Remove from queue"));
            let app = self.clone();
            let popover_for_cancel = popover.clone();
            cancel.connect_clicked(move |_| {
                app.cancel_queued(index);
                popover_for_cancel.popdown();
            });
            row.add_suffix(&cancel);

            list.append(&row);
        }
        drop(queue);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        column.set_margin_top(8);
        column.set_margin_bottom(8);
        column.set_margin_start(8);
        column.set_margin_end(8);
        column.set_size_request(280, -1);
        column.append(&list);

        popover.set_child(Some(&column));
        popover.set_parent(anchor);
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    }

    fn enqueue(self: &Rc<Self>, entry: QueueEntry) {
        let label = entry.label.clone();
        self.queue.borrow_mut().push_back(entry);
        if self.running.get() {
            let pending = self.queue.borrow().len();
            self.toast(&format!("{label} — queued ({pending})"));
        } else {
            self.start_next();
        }
    }

    fn start_next(self: &Rc<Self>) {
        let Some(mut entry) = self.queue.borrow_mut().pop_front() else {
            self.running.set(false);
            self.job_revealer.set_reveal_child(false);
            return;
        };

        // "Collect at night" — a mutating job (an actual build, not a
        // pretend/preview run) waits for the configured off-hours window
        // instead of starting immediately, so a long queue can be left to
        // run unattended overnight without competing with the machine
        // during the day. Deferred jobs stay at the front of the queue
        // and get rechecked periodically rather than blocking anything
        // else — an urgent non-mutating check can still run in the
        // meantime.
        if entry.mutating && self.settings.borrow().night_builds_only && !in_night_window() {
            self.queue.borrow_mut().push_front(entry);
            self.running.set(false);
            self.job_label.set_text("Waiting for night hours to build…");
            self.job_progress.set_visible(false);
            self.job_eta.set_visible(false);
            self.job_log.set_text("");
            self.job_revealer.set_reveal_child(true);
            let app = self.clone();
            gtk::glib::timeout_add_seconds_local(300, move || {
                if !app.running.get() {
                    app.start_next();
                }
                gtk::glib::ControlFlow::Break
            });
            return;
        }

        // Resource-throttled by default (see `resource_limits::throttled`)
        // for anything that actually builds — a queued job left running
        // for hours shouldn't be the reason something else on the machine
        // starves for CPU/IO, and an unthrottled `MAKEOPTS` can exhaust
        // RAM outright on a job with enough packages to build back to
        // back.
        if entry.mutating && self.settings.borrow().throttle_builds {
            entry.job = crate::portage::resource_limits::throttled(entry.job);
        }

        self.running.set(true);
        self.job_label.set_text(&entry.label);
        self.job_log.set_text("");
        self.job_eta.set_visible(false);
        if !entry.known_atoms.is_empty() {
            let job_eta = self.job_eta.clone();
            let atoms = entry.known_atoms.clone();
            let total_atoms = atoms.len();
            runtime::spawn_blocking(
                move || crate::portage::qlop::average_merge_seconds_batch(&atoms),
                move |averages| {
                    let known = averages.len();
                    if known == 0 {
                        return;
                    }
                    let total: u64 = averages.values().map(|(secs, _)| secs).sum();
                    let estimate = crate::portage::build_time::format_duration(total);
                    job_eta.set_text(&if known < total_atoms {
                        format!("Estimated ≥{estimate} ({known} of {total_atoms} packages have build history)")
                    } else {
                        format!("Estimated ~{estimate}")
                    });
                    job_eta.set_visible(true);
                },
            );
        }
        self.job_progress.set_fraction(0.0);
        self.job_progress.set_text(None);
        // Visible and pulsing from the moment the job starts, not just once
        // a "Jobs: N of M" line shows up — for a job with a lot of
        // resolving to do up front (an `@world` update easily takes a
        // while before touching its first package), that line can be
        // long enough coming that the bottom bar looked like an inert
        // label with no indication anything was actually running.
        self.job_progress.set_visible(true);
        self.job_progress.pulse();
        self.job_revealer.set_reveal_child(true);

        let log_app = self.clone();
        let done_app = self.clone();
        let label = entry.label;
        let mutating = entry.mutating;
        let retry_with_use_fix = entry.retry_with_use_fix;
        let job_for_retry = entry.job.clone();
        // Install/uninstall jobs' last arg is always the atom (see
        // `install_job`/`uninstall_job`) — used to find this exact
        // package's own detail page, if it happens to be the one open
        // right now, so it can show this job's progress under its own
        // Install/Remove button instead of only in the bottom bar.
        let job_atom = entry.job.args.last().cloned();
        // `--depclean` only ever appears in `uninstall_job`'s args — used
        // to tell `refresh_detail_action_button` below which way to flip
        // once this job succeeds.
        let job_is_install = !entry.job.args.iter().any(|a| a == "--depclean");

        // Real progress (a "Jobs: N of M" line) only starts appearing once
        // portage is actually building/merging — dependency resolution and
        // downloading beforehand report nothing to size a determinate bar
        // against. Pulses (GTK's own bar animates a block sliding left to
        // right, `bar.pulse()` on a timer) fill that stretch instead of the
        // bar just sitting empty; `pulsing` flips false the moment real
        // progress arrives (below) or the job ends, which is what stops
        // the timer.
        let pulsing = Rc::new(Cell::new(true));
        if let Some(atom) = &job_atom
            && let Some(page) = visible_detail_page(&self.nav, atom) {
                if let Some(button) = detail_action_button(&page) {
                    button.add_css_class("detail-action-pulsing");
                }
                if let Some(bar) = detail_progress_bar(&page) {
                    bar.set_visible(true);
                    bar.pulse();
                    let pulsing_for_timer = pulsing.clone();
                    gtk::glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
                        if !pulsing_for_timer.get() {
                            return gtk::glib::ControlFlow::Break;
                        }
                        bar.pulse();
                        gtk::glib::ControlFlow::Continue
                    });
                }
            }

        // The bottom bar's own progress indicator, independent of whichever
        // (if any) detail page is open — an `@world` update has no single
        // package's page to show progress under, so this is the only
        // animation it ever gets.
        let bottom_bar = self.job_progress.clone();
        let pulsing_for_bottom_timer = pulsing.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
            if !pulsing_for_bottom_timer.get() {
                return gtk::glib::ControlFlow::Break;
            }
            bottom_bar.pulse();
            gtk::glib::ControlFlow::Continue
        });

        // Collected alongside the log label's running "latest line" above
        // so a failure can be inspected for a fixable cause afterwards —
        // the label only ever shows the newest line, not the whole run.
        let output: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let output_for_line = output.clone();
        let pulsing_for_done = pulsing.clone();
        runtime::spawn_job(
            entry.job,
            move |line| {
                if let Some(step) = emerge::parse_step_progress(&line) {
                    // The bar itself deliberately keeps pulsing instead of
                    // switching to this as a determinate fraction: "Jobs: N
                    // of M" only advances between *whole packages*
                    // finishing, not during one's own (often multi-minute)
                    // build — showing that fraction would leave the bar
                    // looking frozen for most of the job instead of
                    // reading as "still working". The count (and, once
                    // known, which package) is still worth showing, so it
                    // goes in the bar's own text overlay instead, updated
                    // without disturbing the pulse.
                    let text = match &step.atom {
                        // `::gentoo`/`::guru`/etc. suffix trimmed — which
                        // repo an atom came from isn't part of "what's
                        // building right now" at a glance.
                        Some(atom) => format!("{} / {}: {}", step.done, step.total, atom.split("::").next().unwrap_or(atom)),
                        None => format!("{} / {}", step.done, step.total),
                    };
                    log_app.job_progress.set_text(Some(&text));
                }
                log_app.job_log.set_text(&line);
                output_for_line.borrow_mut().push(line);
            },
            move |success| {
                pulsing_for_done.set(false);
                if !success && retry_with_use_fix
                    && let Some(relaxation) = PendingRelaxation::detect(&output.borrow()) {
                        let done_app = done_app.clone();
                        let job_for_retry = job_for_retry.clone();
                        let label_for_retry = label.clone();

                        // Shown before touching anything: this is a
                        // dependency's own relaxation, not something the
                        // user directly asked for, and applying the wrong
                        // one (an EULA accepted site-wide, say) is worth a
                        // look before it happens rather than an automatic
                        // silent fix.
                        let mut body = format!("{label_for_retry} {}\n\n", relaxation.intro());
                        for line in relaxation.body_lines() {
                            body.push_str(&line);
                            body.push('\n');
                        }
                        body.push_str("\nApply them and retry?");

                        let dialog = adw::AlertDialog::new(Some(relaxation.dialog_title()), Some(&body));
                        dialog.add_response("cancel", "Cancel");
                        dialog.add_response("apply", "Apply & Retry");
                        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
                        dialog.set_default_response(Some("apply"));
                        dialog.set_close_response("cancel");

                        let done_app_for_present = done_app.clone();
                        dialog.connect_response(None, move |_, response| {
                            if response != "apply" {
                                done_app.toast(&format!("{label_for_retry} — failed"));
                                done_app.start_next();
                                return;
                            }
                            let done_app = done_app.clone();
                            let job_for_retry = job_for_retry.clone();
                            let label_for_retry = label_for_retry.clone();
                            let relaxation_kind = relaxation.noun_phrase();
                            let relaxation = relaxation.clone();
                            runtime::spawn_blocking(
                                move || relaxation.apply(),
                                move |result| {
                                    if result.is_ok() {
                                        // Retried once, at the front of
                                        // the queue — with
                                        // `retry_with_use_fix` false this
                                        // time, so a second failure
                                        // reports normally instead of
                                        // looping (or prompting again).
                                        done_app.queue.borrow_mut().push_front(QueueEntry {
                                            job: job_for_retry.clone(),
                                            label: label_for_retry.clone(),
                                            mutating,
                                            retry_with_use_fix: false,
                                            known_atoms: Vec::new(),
                                        });
                                        done_app.toast(&format!("{label_for_retry} — applying {relaxation_kind}, retrying"));
                                    } else {
                                        done_app.toast(&format!("{label_for_retry} — failed"));
                                    }
                                    done_app.start_next();
                                },
                            );
                        });
                        dialog.present(Some(&done_app_for_present.window));
                        return;
                    }
                // A real build failure (not the USE-flag block already
                // handled above, not a resolver issue — an ebuild phase
                // actually died) gets its own dialog with the log tail and
                // follow-up actions, rather than just a toast that's easy
                // to miss and gives no way to actually see what broke.
                let build_failure = (!success).then(|| emerge::parse_build_failure(&output.borrow())).flatten();
                // Sent regardless of which branch below fires — a job
                // that just finished is exactly as worth knowing about
                // whether the window's in focus or the person's stepped
                // away from a multi-hour build entirely, which a toast
                // alone (gone the moment it fades, and only ever seen if
                // this window happens to be visible right now) doesn't
                // cover.
                done_app.send_notification(&label, success);
                if let Some(failure) = build_failure {
                    build_failure::present(&done_app.window, &label, failure);
                } else {
                    done_app.toast(&if success {
                        format!("{label} — done")
                    } else {
                        format!("{label} — failed")
                    });
                }
                // This exact package's own Install/Remove button, left
                // showing its old label and no progress bar otherwise —
                // the page was already built and on screen before this
                // job ever started, so it has no way to know on its own
                // that `app.installed` just changed underneath it.
                // Flipped directly (not by rebuilding the page) rather
                // than waiting on `rescan_installed_then` — the button
                // only needs to know *this job's own* outcome, which is
                // already known here, not the full freshly-rescanned map.
                if let Some(atom) = &job_atom
                    && let Some(page) = visible_detail_page(&done_app.nav, atom) {
                        if success {
                            refresh_detail_action_button(&page, job_is_install);
                        } else {
                            if let Some(bar) = detail_progress_bar(&page) {
                                bar.set_visible(false);
                            }
                            if let Some(button) = detail_action_button(&page) {
                                button.set_sensitive(true);
                                button.remove_css_class("detail-action-pulsing");
                            }
                        }
                    }
                if mutating && success {
                    done_app.rescan_installed();
                    done_app.check_updates();
                    done_app.check_config_protect();
                    done_app.check_glsa();
                    done_app.check_sync();
                }
                done_app.start_next();
            },
        );
    }

    fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// A real desktop notification, not just a toast — a multi-hour
    /// `@world` update is exactly the kind of job someone starts and
    /// then leaves the computer for, and a toast that's already faded by
    /// the time they're back tells them nothing.
    fn send_notification(&self, label: &str, success: bool) {
        let Some(application) = self.window.application() else { return };
        let notification = gtk::gio::Notification::new(label);
        notification.set_body(Some(if success {
            "Finished successfully."
        } else {
            "Failed — open Portage Store for details."
        }));
        notification.set_priority(if success {
            gtk::gio::NotificationPriority::Normal
        } else {
            gtk::gio::NotificationPriority::High
        });
        // A fixed id (not the app id — this identifies the notification
        // itself) so a second job finishing while the first's
        // notification is still showing replaces it instead of stacking
        // up duplicates for jobs that have already been superseded.
        application.send_notification(Some("job-complete"), &notification);
    }
}
