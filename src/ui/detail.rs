use crate::backend;
use crate::flatpak;
use crate::portage::eix::PackageSummary;
use crate::portage::emerge::{self, InstallPreview};
use crate::portage::installed::InstalledPackage;
use crate::portage::{appstream, build_time, package_use, use_desc};
use crate::ui::runtime;
use crate::ui::widgets;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

/// One package's build-time breakdown, once `qlop` answers — `None` for a
/// package `qlop` has no merge history for (never built locally, so
/// there's nothing to average). Cached per package-list in
/// `start_time_breakdown_fetch`'s `time_breakdown_cache`, since re-running
/// the whole `qlop` batch on every re-open of the Build Time tile would
/// re-pay its cost for data that isn't going to change within the page's
/// lifetime.
type TimeBreakdown = Vec<(emerge::PendingPackage, Option<u64>)>;

/// Opens one screenshot at full size, dismissable with Escape or by
/// clicking the image.
/// Reads a stateful `win.<name>` toggle's current value straight from the
/// action, rather than a value handed to `build()` when the page was
/// first opened — the page may have been open a while, during which the
/// user could have flipped the setting elsewhere, and reading it fresh
/// here is what makes that take effect without needing to leave and
/// reopen the page. The window-level actions live on
/// `adw::ApplicationWindow` specifically — a plain `gtk::Window` doesn't
/// implement the `ActionMap`/`ActionGroup` interfaces `lookup_action`
/// needs — so the downcast has to target that concrete type rather than
/// the generic `gtk::Root` a widget's `.root()` returns.

fn lookup_window_action(widget: &impl IsA<gtk::Widget>, name: &str) -> Option<gtk::gio::Action> {
    widget.root()?.downcast::<adw::ApplicationWindow>().ok()?.lookup_action(name)
}

fn github_preview_enabled(widget: &impl IsA<gtk::Widget>) -> bool {
    lookup_window_action(widget, "github-preview")
        .and_then(|action| action.state())
        .and_then(|v| v.get::<bool>())
        .unwrap_or(true)
}

/// How much of the store window's height the lightbox is allowed to fill —
/// deliberately well short of 100%: the width below is uncapped (spans the
/// window edge-to-edge), and letting height do the same made the carousel
/// read as taking over the entire screen rather than a photo floating over
/// a dimmed backdrop. A fraction, not a fixed gap, so it scales sensibly
/// whether the store window is small or maximized.
const SCREENSHOT_LIGHTBOX_MAX_HEIGHT_FRACTION: f64 = 0.65;

/// The lightbox's own maximum photo footprint: the store window's full
/// *current* width (the carousel spans it edge-to-edge — a fixed 1400px
/// could exceed the actual window whenever it's smaller than that, and one
/// bigger than its parent window gets clipped instead of just... being
/// smaller), but height capped well short of the window's own so the
/// carousel doesn't stretch top-to-bottom too. Falls back to a sane
/// default if the window's size isn't available for some reason.
fn max_screenshot_size(widget: &impl IsA<gtk::Widget>) -> (f64, f64) {
    const FALLBACK: (f64, f64) = (1400.0, 900.0);
    let Some(root) = widget.root() else { return FALLBACK };
    let (window_width, window_height) = (root.width(), root.height());
    if window_width <= 0 || window_height <= 0 {
        return FALLBACK;
    }
    (window_width as f64, window_height as f64 * SCREENSHOT_LIGHTBOX_MAX_HEIGHT_FRACTION)
}

/// The screenshot lightbox's scrim — a plain dimmed backdrop that fills the
/// whole window when swapped in, built once by `App::build` (see `mod.rs`)
/// rather than an `AdwDialog` (which always draws its own presentation
/// card — background, rounded corners, shadow — around its content,
/// exactly the "second window inside the window" look the lightbox
/// shouldn't have).
fn screenshot_lightbox_scrim(widget: &impl IsA<gtk::Widget>) -> Option<gtk::Box> {
    let window = widget.root()?.downcast::<adw::ApplicationWindow>().ok()?;
    // SAFETY: `App::build` is the sole writer of this key, and it always
    // stores a `gtk::Box` there for the window's whole lifetime.
    unsafe { window.data::<gtk::Box>("screenshot-lightbox-scrim").map(|ptr| ptr.as_ref().clone()) }
}

/// Builds one screenshot-lightbox carousel page: `path`'s image, letterboxed
/// (`Contain`) to fill the carousel's own fixed viewport uniformly via
/// `hexpand`/`vexpand` — without those, AdwCarousel sizes each page to its
/// child's *natural* width instead of the carousel's own, the same issue
/// already worked around for the landing page's featured carousels.
fn screenshot_lightbox_page(path: &std::path::Path) -> gtk::Picture {
    let picture = gtk::Picture::for_filename(path);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture
}

/// Wires the screenshot lightbox's carousel to loop, the same technique
/// as the landing page's featured carousels (see `install_carousel_behavior`
/// in `mod.rs`): real pages sit at indices `1..=real_pages`, bracketed by
/// a duplicate of the last page at index 0 and a duplicate of the first at
/// `real_pages + 1`. AdwCarousel's pages sit on one physical strip, so an
/// *animated* `scroll_to` always slides across it linearly — asked to jump
/// from the last page straight to the first, it would slide backwards
/// through everything in between. Landing on a duplicate is caught here
/// and instantly (no animation, since the content is pixel-identical)
/// re-points the carousel at the real page it duplicates, so scrolling
/// past either end keeps sliding the same direction instead of reversing.
///
/// `target` tracks the page already asked for — not `carousel.position()`,
/// which lags behind while a slide is still in flight, so a second button
/// click arriving before the first one settles would otherwise base its
/// math on a stale in-between value.
fn install_screenshot_loop(carousel: &adw::Carousel, real_pages: u32, target: Rc<Cell<u32>>) {
    carousel.connect_page_changed(move |carousel, position| {
        if position == 0 {
            let real_last = carousel.nth_page(real_pages);
            carousel.scroll_to(&real_last, false);
            target.set(real_pages);
        } else if position == real_pages + 1 {
            let real_first = carousel.nth_page(1);
            carousel.scroll_to(&real_first, false);
            target.set(1);
        } else {
            target.set(position);
        }
    });
}

/// Opens the screenshots at full size in a carousel, dismissable with
/// Escape or by clicking anywhere that isn't one of the arrows or the
/// close button. Steppable with the prev/next arrows when there's more
/// than one screenshot, looping past either end rather than stopping dead
/// — with only one screenshot, neither arrow appears at all.
///
/// Built as a carousel filling the window's permanent scrim, not an
/// `AdwDialog` — see `screenshot_lightbox_scrim`'s doc comment for why.
fn present_full_screenshot(
    anchor: &impl IsA<gtk::Widget>,
    paths: Rc<RefCell<Vec<Option<PathBuf>>>>,
    start_index: usize,
) {
    let Some(scrim) = screenshot_lightbox_scrim(anchor) else { return };
    // The scrim is reused across every screenshot lightbox opened during
    // this session, so it may still hold a previous invocation's content.
    while let Some(child) = scrim.first_child() {
        scrim.remove(&child);
    }

    let borrowed = paths.borrow();
    let Some(start_path) = borrowed.get(start_index).cloned().flatten() else { return };
    // Only the screenshots that actually finished downloading take part —
    // a still-in-flight or failed one has nothing to show, so it's simply
    // not a page rather than a dead stop when stepping through them.
    let available: Vec<PathBuf> = borrowed.iter().flatten().cloned().collect();
    drop(borrowed);
    let start_pos = available.iter().position(|p| *p == start_path).unwrap_or(0) as u32;
    let real_pages = available.len() as u32;

    let viewport = max_screenshot_size(anchor);
    let carousel = adw::Carousel::new();
    carousel.set_width_request(viewport.0 as i32);
    carousel.set_height_request(viewport.1 as i32);
    // Without these, the carousel doesn't fill the `carousel_clamp`
    // `GtkScrolledWindow` (below) that caps its footprint — it shrinks
    // back down to whatever *it* considers a natural size within that
    // viewport, leaving visible empty space around the carousel itself
    // (in turn throwing off the close button's position, which is
    // anchored to `carousel_clamp`'s corner, not the shrunk carousel's).
    carousel.set_hexpand(true);
    carousel.set_vexpand(true);

    if real_pages > 1 {
        carousel.append(&screenshot_lightbox_page(&available[available.len() - 1]));
    }
    for path in &available {
        carousel.append(&screenshot_lightbox_page(path));
    }
    if real_pages > 1 {
        carousel.append(&screenshot_lightbox_page(&available[0]));
    }

    // AdwCarousel doesn't cap its own natural/minimum size to one page —
    // `set_width_request`/`set_height_request` above only raise a floor,
    // they don't stop the carousel reporting a much bigger requirement
    // upward (observed directly: a `Gtk-WARNING` about a box needing
    // ~1840px when the window was only 1003px wide). A `GtkScrolledWindow`
    // does NOT propagate its child's natural size by default — wrapping
    // the carousel in one (with scrollbars turned off, so it's invisible
    // and purely a sizing clamp) hard-caps its footprint to exactly what
    // we request here, regardless of what the carousel wants internally.
    let carousel_clamp = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(viewport.0 as i32)
        .min_content_height(viewport.1 as i32)
        .child(&carousel)
        .build();

    // The carousel spans the full window, so the prev/next arrows float on
    // top of it (near its left/right edges) rather than sitting beside it
    // in their own reserved gutters — there's no room left beside a
    // full-width carousel for a gutter to occupy.
    let content = gtk::Overlay::new();
    content.add_css_class("screenshot-lightbox-content");
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    content.set_child(Some(&carousel_clamp));

    // "osd", not "flat": a flat button has no chrome of its own and
    // relies on the surrounding widget background for contrast — against
    // the scrim's plain black (or, now, the photo itself), that left them
    // all but invisible. "osd" is built for exactly this (controls
    // floating over dark/arbitrary content), the same reason the close
    // button already used it.
    let prev_button = gtk::Button::from_icon_name("go-previous-symbolic");
    prev_button.add_css_class("osd");
    prev_button.add_css_class("circular");
    prev_button.set_halign(gtk::Align::Start);
    prev_button.set_valign(gtk::Align::Center);
    prev_button.set_margin_start(16);
    prev_button.set_visible(real_pages > 1);
    content.add_overlay(&prev_button);

    let next_button = gtk::Button::from_icon_name("go-next-symbolic");
    next_button.add_css_class("osd");
    next_button.add_css_class("circular");
    next_button.set_halign(gtk::Align::End);
    next_button.set_valign(gtk::Align::Center);
    next_button.set_margin_end(16);
    next_button.set_visible(real_pages > 1);
    content.add_overlay(&next_button);

    // The (shared, permanent) scrim: a plain dimmed backdrop filling the
    // whole window, with the carousel+arrows centered on top of it — no
    // card, no background of its own beyond the dim, nothing that reads
    // as "a window inside the window". Escape and a plain click both
    // close it; unlike `AdwDialog` this is hand-rolled, so both need
    // wiring here instead of coming for free.
    scrim.append(&content);
    scrim.set_can_focus(true);

    // Starts faded out and scaled down, then immediately (next frame)
    // eases up to full size and opacity — a scale-from-center zoom, not
    // just a fade. Same reasoning as the featured carousels' wrap fade
    // for why this is a short timeout and not `idle_add_local_once`:
    // idle callbacks run before the next frame is even painted, so the
    // hidden state would be removed before the compositor ever rendered
    // it — no frame would show the shrunk/faded state, so there'd be
    // nothing for the transition to animate *from*. `AdwDialog` used to
    // give this open/close transition for free; a hand-rolled scrim
    // needs it wired explicitly on both ends.
    scrim.add_css_class("screenshot-lightbox-scrim-hidden");
    content.add_css_class("screenshot-lightbox-content-hidden");
    // Visible and hit-testable for as long as the lightbox is up — set
    // back to hidden/non-targeting by `close` below once its fade-out
    // finishes, so the page underneath is clickable again immediately
    // after and doesn't pay for an invisible full-window widget sitting
    // over it the rest of the time.
    scrim.set_visible(true);
    scrim.set_can_target(true);
    scrim.grab_focus();
    let scrim_for_open = scrim.clone();
    let content_for_open = content.clone();
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
        scrim_for_open.remove_css_class("screenshot-lightbox-scrim-hidden");
        content_for_open.remove_css_class("screenshot-lightbox-content-hidden");
    });

    // Guards against a second close (e.g. Escape while the close button's
    // click is still fading out) hiding the scrim a second time.
    let closing = Rc::new(Cell::new(false));
    let scrim_for_close = scrim.clone();
    let content_for_close = content.clone();
    let close = move || {
        if closing.replace(true) {
            return;
        }
        scrim_for_close.add_css_class("screenshot-lightbox-scrim-hidden");
        content_for_close.add_css_class("screenshot-lightbox-content-hidden");
        let scrim_for_close = scrim_for_close.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            scrim_for_close.set_visible(false);
            scrim_for_close.set_can_target(false);
        });
    };

    if real_pages > 1 {
        let target = Rc::new(Cell::new(start_pos + 1));
        carousel.scroll_to(&carousel.nth_page(start_pos + 1), false);
        install_screenshot_loop(&carousel, real_pages, target.clone());

        let carousel_for_prev = carousel.clone();
        let target_for_prev = target.clone();
        prev_button.connect_clicked(move |_| {
            let prev_index = target_for_prev.get() - 1;
            target_for_prev.set(prev_index);
            carousel_for_prev.scroll_to(&carousel_for_prev.nth_page(prev_index), true);
        });

        let carousel_for_next = carousel.clone();
        next_button.connect_clicked(move |_| {
            let next_index = target.get() + 1;
            target.set(next_index);
            carousel_for_next.scroll_to(&carousel_for_next.nth_page(next_index), true);
        });
    }

    // Attached to the scrim itself, not just the carousel overlay, so a
    // click anywhere on the dimmed backdrop around the photo closes it
    // too. The arrows and close button are separate widgets on top and
    // claim their own clicks first, so this only fires for clicks that
    // land on the photo or the backdrop.
    let click = gtk::GestureClick::new();
    let close_for_click = close.clone();
    click.connect_released(move |_, _, _, _| close_for_click());
    scrim.add_controller(click);

    let key = gtk::EventControllerKey::new();
    let close_for_key = close.clone();
    key.connect_key_pressed(move |_, keyval, _, _| {
        if keyval == gtk::gdk::Key::Escape {
            close_for_key();
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    scrim.add_controller(key);
}

/// A swipeable strip of screenshots, each fetched in the background so a
/// slow or unreachable image host never blocks the page. Images that fail
/// to download simply leave their slot out rather than showing a broken
/// placeholder.
/// Clears `screenshot_slot` and, if the preview toggle currently allows
/// it, fills it with the GitHub fallback card. Used both for the initial
/// render and every time the toggle changes while the page is open.
fn render_github_fallback_card(screenshot_slot: &gtk::Box, url: &str) {
    while let Some(child) = screenshot_slot.first_child() {
        screenshot_slot.remove(&child);
    }
    if github_preview_enabled(screenshot_slot) {
        screenshot_slot.append(&screenshot_carousel(&[url.to_string()]));
    }
}

/// Keeps the fallback card in sync with the "win.github-preview" toggle
/// for as long as this detail page is open, instead of only reflecting
/// whatever the setting happened to be when the GitHub lookup completed.
///
/// Captures `screenshot_slot` weakly: the action lives on the window for
/// the whole session, so a strong capture here would keep every detail
/// page's screenshot area alive in memory for as long as the app runs.
/// With a weak capture, once the page is popped and the widget is
/// dropped, the callback simply becomes a no-op instead of a leak.
fn watch_github_preview_toggle(screenshot_slot: &gtk::Box, url: String) {
    let Some(action) = lookup_window_action(screenshot_slot, "github-preview") else {
        return;
    };
    let weak_slot = screenshot_slot.downgrade();
    action.connect_notify_local(Some("state"), move |_, _| {
        if let Some(screenshot_slot) = weak_slot.upgrade() {
            render_github_fallback_card(&screenshot_slot, &url);
        }
    });
}

fn screenshot_carousel(urls: &[String]) -> gtk::Widget {
    let carousel = adw::Carousel::new();
    carousel.set_height_request(340);
    carousel.set_spacing(12);
    carousel.set_hexpand(true);

    let capped: Vec<String> = urls.iter().take(8).cloned().collect();
    let real_pages = capped.len() as u32;
    // Shared with the lightbox opened by clicking any one of these, so it
    // can step to the next/previous screenshot without needing to
    // re-fetch anything — filled in below as each download finishes,
    // `None` while still in flight or if it failed. Sized to the real
    // pages only — the wrap-loop's boundary clones below duplicate a real
    // page's *picture*, not its slot in this list.
    let paths: Rc<RefCell<Vec<Option<PathBuf>>>> = Rc::new(RefCell::new(vec![None; capped.len()]));

    // One picture per URL, wired to fetch its file in the background and
    // (for a real page, not a wrap-loop boundary clone) to open the
    // lightbox on click once that file exists.
    let build_picture = |url: String, click_index: Option<usize>| {
        let picture = gtk::Picture::new();
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.add_css_class("screenshot");

        let picture_for_fetch = picture.clone();
        let carousel_for_removal = carousel.clone();
        let paths = paths.clone();
        runtime::spawn_blocking(
            move || crate::portage::media::fetch(&url),
            move |path| {
                let Some(path) = path else {
                    carousel_for_removal.remove(&picture_for_fetch);
                    return;
                };
                picture_for_fetch.set_filename(Some(&path));
                if let Some(index) = click_index {
                    paths.borrow_mut()[index] = Some(path);

                    // Click to enlarge. Wired up only once the file exists,
                    // so there is never a click that opens an empty dialog.
                    let click = gtk::GestureClick::new();
                    let picture_for_click = picture_for_fetch.clone();
                    let paths = paths.clone();
                    click.connect_released(move |_, _, _, _| {
                        present_full_screenshot(&picture_for_click, paths.clone(), index);
                    });
                    picture_for_fetch.add_controller(click);
                    picture_for_fetch.set_cursor_from_name(Some("pointer"));
                }
            },
        );
        picture
    };

    // A duplicate of the *last* real screenshot glued on before the first,
    // and a duplicate of the *first* glued on after the last — the same
    // wrap-loop technique as the landing page's featured carousels (see
    // `install_carousel_behavior` in `mod.rs`) and the screenshot lightbox
    // (see `install_screenshot_loop` above), applied here so scrolling
    // past either end of this carousel loops around instead of stopping
    // dead. Neither clone gets a click handler — they're only ever on
    // screen mid-gesture, immediately swapped for the real page they
    // duplicate once the scroll settles.
    if real_pages > 1 {
        carousel.append(&build_picture(capped[capped.len() - 1].clone(), None));
    }
    let mut dots = Vec::with_capacity(capped.len());
    for (index, url) in capped.iter().enumerate() {
        carousel.append(&build_picture(url.clone(), Some(index)));
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("carousel-dot");
        dots.push(dot);
    }
    if real_pages > 1 {
        carousel.append(&build_picture(capped[0].clone(), None));
    }
    if let Some(first) = dots.first() {
        first.add_css_class("carousel-dot-active");
    }

    let dots_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    dots_row.set_halign(gtk::Align::Center);
    dots_row.set_margin_top(8);
    for dot in &dots {
        dots_row.append(dot);
    }

    if real_pages > 1 {
        // Skips straight past the leading clone at index 0 to the real
        // first page at index 1 — nothing has been shown yet at this
        // point, so there's nothing for an animation to visibly interrupt.
        carousel.scroll_to(&carousel.nth_page(1), false);
        // Unlike the landing page's featured carousels, screenshots rarely
        // fill the carousel's reserved height exactly, so a 200px deadzone
        // at the top and bottom lets the page scroll normally there instead
        // of capturing the wheel over empty letterboxed margin.
        super::install_carousel_behavior_with_deadzone(&carousel, real_pages, dots, 200.0);
    }
    // With only one screenshot there's nothing to page through, so unlike
    // the landing page's featured carousels this one deliberately does NOT
    // swallow the scroll wheel here — with no controller attached at all,
    // wheel events fall straight through to the page's own scroller
    // instead of getting stuck over a carousel that can't move.

    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    column.append(&carousel);
    if real_pages > 1 {
        column.append(&dots_row);
    }

    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.add_css_class("screenshot-carousel-panel");
    // The same built-in style the "Download Size"/"Build Time" facts row
    // uses — layered underneath our own class so its background wins over
    // `@card_bg_color` there, matching exactly whatever this GTK theme
    // actually renders `.card` as instead of assuming the two are the same
    // color.
    panel.add_css_class("card");
    panel.append(&column);
    panel.upcast()
}

/// How many characters of `body` text to keep in its collapsed form —
/// enough to read as a real preview (a handful of lines at this page's
/// width) without ballooning into most of the description.
const DESCRIPTION_COLLAPSE_BUDGET: usize = 320;

/// Cuts `text` at the sentence boundary nearest to (but not past)
/// `budget` characters, so a collapsed preview never stops mid-sentence —
/// unlike Pango's own line-based ellipsis, which clips wherever the pixel
/// width runs out regardless of where a word or sentence happens to end.
/// Returns `None` if `text` already fits within the budget whole, since
/// then there's nothing to collapse.
fn truncate_at_sentence(text: &str, budget: usize) -> Option<String> {
    if text.chars().count() <= budget {
        return None;
    }

    // A sentence boundary is `.`/`!`/`?` immediately followed by
    // whitespace or the end of the string — tracks the byte offset just
    // past the last one found before `budget` characters in.
    let mut cut = None;
    for (chars_seen, (byte_i, ch)) in text.char_indices().enumerate() {
        if chars_seen >= budget {
            break;
        }
        if matches!(ch, '.' | '!' | '?') {
            let next = text[byte_i + ch.len_utf8()..].chars().next();
            if next.is_none() || next.is_some_and(char::is_whitespace) {
                cut = Some(byte_i + ch.len_utf8());
            }
        }
    }

    let cut = cut?;
    if text[cut..].trim_start().is_empty() {
        return None;
    }
    Some(text[..cut].trim_end().to_string())
}

/// One of the four at-a-glance facts across the top of the detail page.
/// Centred placeholder text for an expand panel that has nothing to show
/// yet (still calculating) or nothing to show at all (no dependencies).
/// Fills in the long-description block: `text`'s first paragraph becomes
/// the bold heading (read as a one-line tagline, the same shape GNOME
/// Software's own description uses), the rest becomes the collapsed body
/// with a "Show More" toggle beneath it. Whichever of the three call
/// sites for this (local AppStream/metadata.xml, Flathub, GitHub) answers
/// first wins — later ones check `body_heading` is still empty before
/// overwriting anything.
fn set_description(
    body_heading: &gtk::Label,
    body: &gtk::Label,
    show_more: &gtk::Button,
    body_text: &Rc<RefCell<(String, String)>>,
    text: &str,
) {
    let mut paragraphs = text.splitn(2, "\n\n");
    let heading = paragraphs.next().unwrap_or_default();
    let rest = paragraphs.next().unwrap_or_default();

    // A single-paragraph description has no real tagline to pull out —
    // forcing its one paragraph into the bold heading slot would leave the
    // (empty, invisible) body doing nothing. Only split off a heading when
    // there's a genuine second paragraph to be the body.
    let full = if rest.is_empty() {
        body_heading.set_visible(false);
        heading
    } else {
        body_heading.set_text(heading);
        body_heading.set_visible(true);
        rest
    };

    match truncate_at_sentence(full, DESCRIPTION_COLLAPSE_BUDGET) {
        Some(collapsed) => {
            *body_text.borrow_mut() = (collapsed.clone(), full.to_string());
            body.set_text(&collapsed);
            body.set_visible(true);
            show_more.set_visible(true);
            show_more.set_label("Show More");
        }
        None => {
            *body_text.borrow_mut() = (full.to_string(), full.to_string());
            body.set_text(full);
            body.set_visible(!full.is_empty());
            show_more.set_visible(false);
        }
    }
}

fn status_placeholder(text: &str) -> gtk::Widget {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_margin_top(8);
    label.set_margin_bottom(8);
    label.upcast()
}

/// One row in a dependency breakdown: the package and version, plus
/// whatever the caller wants said about it (a size, a build time, a
/// New/Reinstall marker).
fn dependency_row(pkg: &emerge::PendingPackage, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(format!("{}-{}", pkg.atom, pkg.version)).subtitle(subtitle).build();
    row.add_prefix(&gtk::Image::from_icon_name(if pkg.is_new {
        "list-add-symbolic"
    } else {
        "view-refresh-symbolic"
    }));
    row
}

/// A capped, scrollable list of dependency rows with a plain-language
/// header line above it. Capped at 15 visible rows — a large package's
/// dependency tree can run into the hundreds, and nobody is reading a
/// 200-row list — with the header's own count covering the full total.
fn dependency_list(header: &str, rows: Vec<adw::ActionRow>) -> gtk::Widget {
    let header_label = gtk::Label::new(Some(header));
    header_label.set_xalign(0.0);
    header_label.add_css_class("dim-label");

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let shown = rows.len().min(15);
    for row in rows.into_iter().take(15) {
        list.append(&row);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.add_css_class("tile-expand-panel");
    column.append(&header_label);
    column.append(&list);
    if shown == 15 {
        // The count implied by `header` may be larger than what's shown;
        // said so explicitly rather than leaving a silently-truncated list.
        let more = gtk::Label::new(Some("Showing the largest 15"));
        more.add_css_class("dim-label");
        more.add_css_class("caption");
        more.set_xalign(0.0);
        column.append(&more);
    }
    column.upcast()
}

/// The Download Size tile's expand panel: every package that needs
/// fetching, largest first, with the grand total spelled out above them —
/// the same total the tile itself shows, just broken into its parts.
fn size_breakdown_panel(packages: &[emerge::PendingPackage]) -> gtk::Widget {
    if packages.is_empty() {
        return status_placeholder("Nothing to download");
    }
    let mut sorted: Vec<&emerge::PendingPackage> = packages.iter().collect();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.download_kib.unwrap_or(0)));
    let total: u64 = packages.iter().filter_map(|p| p.download_kib).sum();

    let rows = sorted
        .into_iter()
        .map(|pkg| {
            let subtitle =
                pkg.download_kib.map(emerge::format_size_kib).unwrap_or_else(|| "Already cached".to_string());
            dependency_row(pkg, &subtitle)
        })
        .collect();

    dependency_list(&format!("{} packages · {} total", packages.len(), emerge::format_size_kib(total)), rows)
}

/// The Install Method tile's expand panel: every package involved in the
/// operation, marked as newly pulled in versus a reinstall/upgrade of
/// something already present — what "Built from source, N packages" on
/// the tile itself actually consists of.
fn method_breakdown_panel(packages: &[emerge::PendingPackage]) -> gtk::Widget {
    if packages.is_empty() {
        return status_placeholder("Nothing else needs building");
    }
    let new_count = packages.iter().filter(|p| p.is_new).count();
    let rows = packages
        .iter()
        .map(|pkg| dependency_row(pkg, if pkg.is_new { "New dependency" } else { "Reinstall / upgrade" }))
        .collect();
    dependency_list(&format!("{} packages · {new_count} new", packages.len()), rows)
}

/// The Build Time tile's expand panel: per-package measured averages from
/// `qlop`, for whichever of the dependencies have been built on this
/// machine before. Takes already-resolved `(package, seconds)` pairs
/// rather than fetching them itself, since that's a batch of blocking
/// `qlop` subprocess calls best done once in the background and cached,
/// not re-run on every expand.
fn time_breakdown_panel(entries: &[(emerge::PendingPackage, Option<u64>)]) -> gtk::Widget {
    if entries.is_empty() {
        return status_placeholder("Nothing else needs building");
    }
    let known: u64 = entries.iter().filter_map(|(_, secs)| *secs).sum();
    let measured_count = entries.iter().filter(|(_, secs)| secs.is_some()).count();
    let rows = entries
        .iter()
        .map(|(pkg, secs)| {
            let subtitle = match secs {
                Some(s) => build_time::format_duration(*s),
                None => "Never built here".to_string(),
            };
            dependency_row(pkg, &subtitle)
        })
        .collect();
    let header = if measured_count == 0 {
        format!("{} packages · none built here before", entries.len())
    } else {
        format!("{} packages · {measured_count} measured, {} known", entries.len(), build_time::format_duration(known))
    };
    dependency_list(&header, rows)
}

/// The Version tile's expand panel: every version the tree offers, newest
/// first, each installable with a click. `recommended` — the newest
/// version confirmed to actually resolve — is marked once probing
/// finishes; until then (or if every version was tried and all conflict)
/// none is.
fn version_list_panel(
    versions: &[String],
    installed_version: Option<&str>,
    recommended: Option<&str>,
    still_checking: bool,
    on_pick: Rc<dyn Fn(String)>,
) -> gtk::Widget {
    if versions.is_empty() {
        return status_placeholder("Loading versions…");
    }
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    for version in versions.iter().rev() {
        let row = adw::ActionRow::builder().title(version.as_str()).build();
        if Some(version.as_str()) == installed_version {
            row.add_suffix(&gtk::Image::from_icon_name("object-select-symbolic"));
            row.set_subtitle("Installed");
        } else if Some(version.as_str()) == recommended {
            let badge = gtk::Label::new(Some("Recommended"));
            badge.add_css_class("accent");
            badge.add_css_class("caption-heading");
            row.add_suffix(&badge);
            row.set_activatable(true);
            let on_pick = on_pick.clone();
            let version = version.clone();
            row.connect_activated(move |_| on_pick(version.clone()));
        } else {
            row.set_activatable(true);
            let on_pick = on_pick.clone();
            let version = version.clone();
            row.connect_activated(move |_| on_pick(version.clone()));
        }
        list.append(&row);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.add_css_class("tile-expand-panel");
    if still_checking {
        let checking = gtk::Label::new(Some("Checking which versions install cleanly…"));
        checking.add_css_class("dim-label");
        checking.set_xalign(0.0);
        column.append(&checking);
    }
    column.append(&list);
    column.upcast()
}

/// One of the four fact tiles. Returns the clickable button itself (for
/// wiring up its expand panel) and the subtitle label (for updating it as
/// data arrives asynchronously).
///
/// Built as a `gtk::Button` rather than a plain `gtk::Box`: every fact
/// here can be broken down further (which packages make up a download
/// total, which versions are available, ...), so the tile needs real
/// hover/press feedback and a click target, not just to look like one.
/// The "⋯" hint fades in on hover via CSS alone (`.info-tile-hint` /
/// `:hover`) as the affordance that there's more underneath.
fn info_tile(icon_name: &str, title: &str, subtitle: &str) -> (gtk::Button, gtk::Label) {
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(20);
    // Fixed size and centre alignment, or the round icon backdrop stretches
    // to the tallest tile in the row and renders as a giant pill.
    let icon_holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    icon_holder.set_halign(gtk::Align::Center);
    icon_holder.set_valign(gtk::Align::Center);
    icon_holder.set_size_request(44, 44);
    icon_holder.add_css_class("info-tile-icon");
    icon.set_hexpand(true);
    icon_holder.append(&icon);

    let title_label = gtk::Label::new(Some(title));
    title_label.add_css_class("heading");
    title_label.set_wrap(true);
    title_label.set_justify(gtk::Justification::Center);

    let subtitle_label = gtk::Label::new(Some(subtitle));
    subtitle_label.add_css_class("dim-label");
    subtitle_label.add_css_class("caption");
    subtitle_label.set_wrap(true);
    subtitle_label.set_justify(gtk::Justification::Center);

    let hint = gtk::Label::new(Some("⋯"));
    hint.add_css_class("info-tile-hint");

    let tile = gtk::Box::new(gtk::Orientation::Vertical, 4);
    tile.set_hexpand(true);
    tile.set_valign(gtk::Align::Start);
    tile.append(&icon_holder);
    tile.append(&title_label);
    tile.append(&subtitle_label);
    tile.append(&hint);

    let button = gtk::Button::builder().child(&tile).css_classes(["flat", "info-tile"]).build();
    (button, subtitle_label)
}

/// Everything the "Learn More" panel can show for one package, gathered in
/// a single background pass rather than the several independent
/// `spawn_blocking` calls the up-front description enrichment uses — this
/// only ever runs once, on demand when the user actually opens the panel,
/// so there's no reason to fan its lookups out separately the way the
/// eager, must-feel-instant enrichment above does.
struct LearnMoreContent {
    man_page: Option<String>,
    appstream: String,
    terminal_trove: String,
    github_readme: String,
    github_repo: Option<String>,
}

/// Runs every "Learn More" source that isn't already known synchronously
/// (AppStream's paragraphs are — they're passed in, already collected by
/// `build` below). Blocking — call off the GTK main thread.
fn gather_learn_more(
    installed: Option<(String, String, String)>,
    name: String,
    atom: String,
    appstream_paragraphs: Vec<String>,
) -> LearnMoreContent {
    let man_page = installed
        .as_ref()
        .and_then(|(category, _, version)| crate::portage::man::lookup(category, &name, version));

    let terminal_trove =
        crate::portage::terminaltrove::lookup(&name).map(|entry| entry.description).unwrap_or_default();

    // Same preference order as the eager enrichment chain above: an exact
    // repo recorded in the ebuild's own metadata.xml beats a fuzzy name
    // search, for the same reason (a small package can share its name with
    // a much more popular, unrelated repo).
    let repo = match use_desc::github_remote_id(&atom) {
        Some(full_name) => crate::portage::github::lookup_known(&full_name),
        None => crate::portage::github::lookup(&name),
    };
    let github_repo = repo.as_ref().map(|r| r.full_name.clone());
    let github_readme = repo.map(|r| r.readme_full).unwrap_or_default();

    LearnMoreContent {
        man_page,
        appstream: appstream_paragraphs.join("\n\n"),
        terminal_trove,
        github_readme,
        github_repo,
    }
}

/// One titled block of text in the "Learn More" panel. Returns `None` for
/// an empty source rather than an empty section, so the panel only ever
/// shows the sources that actually had something to say.
fn learn_more_section(title: &str, body: &str, monospace: bool) -> Option<gtk::Widget> {
    let body = body.trim();
    if body.is_empty() {
        return None;
    }

    let heading = gtk::Label::new(Some(title));
    heading.set_xalign(0.0);
    heading.add_css_class("title-4");

    let text = gtk::Label::new(Some(body));
    text.set_xalign(0.0);
    text.set_wrap(true);
    text.set_selectable(true);
    // A selectable GtkLabel still grabs keyboard focus by default, and
    // AdwDialog focuses the first focusable widget the moment it's
    // presented — with several of these stacked in the panel, that made
    // whichever section came first render with its focus/selection
    // highlight the instant "Learn More" opened, before the user had
    // clicked anything. `can_focus(false)` stops that auto-grab; mouse-drag
    // selection (what `selectable` is actually for) doesn't need focus to
    // work.
    text.set_can_focus(false);
    if monospace {
        text.add_css_class("monospace");
        text.add_css_class("caption");
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    column.append(&heading);
    column.append(&text);
    Some(column.upcast())
}

/// Presents the gathered content in a dedicated dialog — not another
/// section stacked onto the already-long detail page, since this is
/// explicitly the "I want to go deeper" reader a click opts into, not
/// something to load unconditionally for every visit.
fn present_learn_more(anchor: &impl IsA<gtk::Widget>, display_name: &str, content: LearnMoreContent) {
    let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let body = gtk::Box::new(gtk::Orientation::Vertical, 20);
    body.set_margin_top(4);
    body.set_margin_bottom(24);
    body.set_margin_start(4);
    body.set_margin_end(4);

    let mut any_section = false;
    for (title, text, monospace) in [
        ("Manual Page", content.man_page.as_deref().unwrap_or(""), true),
        ("From the Package's Metadata", content.appstream.as_str(), false),
        ("From the GitHub README", content.github_readme.as_str(), false),
        ("Terminal Trove", content.terminal_trove.as_str(), false),
    ] {
        if let Some(section) = learn_more_section(title, text, monospace) {
            body.append(&section);
            any_section = true;
        }
    }

    if !any_section {
        body.append(
            &adw::StatusPage::builder()
                .icon_name("dialog-information-symbolic")
                .title("No Extra Documentation Found")
                .description(
                    "Nothing more turned up in this package's metadata, its man pages, GitHub, or Terminal Trove.",
                )
                .build(),
        );
    }

    if let Some(full_name) = &content.github_repo {
        body.append(&link_row("system-users-symbolic", "View on GitHub", &format!("https://github.com/{full_name}")));
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(680).child(&body).build())
        .build();
    scroller.set_margin_start(12);
    scroller.set_margin_end(12);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder()
        .title(format!("Learn More — {display_name}"))
        .content_width(640)
        .content_height(680)
        .child(&toolbar)
        .build();
    dialog.present(Some(&window));
}

fn link_row(icon_name: &str, title: &str, url: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(url).build();
    row.add_prefix(&gtk::Image::from_icon_name(icon_name));
    let open = gtk::Image::from_icon_name("adw-external-link-symbolic");
    row.add_suffix(&open);
    row.set_activatable(true);
    let title = title.to_string();
    let url = url.to_string();
    row.connect_activated(move |row| {
        super::webview::open(row, &title, &url);
    });
    row
}

/// Renders the flag toggles. Changes are written to
/// `/etc/portage/package.use/zz-portage-store` immediately; they take
/// effect on the package's next rebuild, which the hint below spells out.
fn use_flags_group(
    pkg: &PackageSummary,
    installed_pkg: Option<&InstalledPackage>,
) -> Option<adw::PreferencesGroup> {
    if pkg.iuse.is_empty() {
        return None;
    }

    let atom = pkg.atom();
    let overrides = package_use::read_managed().unwrap_or_default();
    let atom_overrides = overrides.get(&atom);

    let group = adw::PreferencesGroup::builder()
        .title("Build Options (USE)")
        .description("Turns package features on or off. Takes effect on the next build.")
        .build();

    for flag in &pkg.iuse {
        let enabled = if let Some(installed) = installed_pkg {
            installed.enabled_use.contains(&flag.name)
        } else if let Some(overridden) = atom_overrides.and_then(|m| m.get(&flag.name)) {
            *overridden
        } else {
            flag.default_enabled
        };

        let row = adw::SwitchRow::builder()
            .title(&flag.name)
            .subtitle(use_desc::describe(&atom, &flag.name).unwrap_or_default())
            .active(enabled)
            .build();

        // "Where is this flag" — the most common USE-flag confusion is a
        // value that doesn't match what was expected because something
        // higher-priority (package.use, or make.conf's own global USE=)
        // already set it. Computed lazily, only on click, rather than for
        // every flag up front — a package with a large IUSE list would
        // otherwise pay for scanning every package.use file once per
        // flag just to build the page, for information most of those
        // flags will never actually have looked up.
        let info_button = gtk::Button::from_icon_name("dialog-information-symbolic");
        info_button.add_css_class("flat");
        info_button.set_valign(gtk::Align::Center);
        info_button.set_tooltip_text(Some("Where is this flag set?"));
        {
            let atom = atom.clone();
            let flag_name = flag.name.clone();
            info_button.connect_clicked(move |button| {
                let source = crate::portage::flag_provenance::locate(&atom, &flag_name);
                let popover = gtk::Popover::new();
                let label = gtk::Label::new(Some(&source.label()));
                label.set_margin_top(8);
                label.set_margin_bottom(8);
                label.set_margin_start(12);
                label.set_margin_end(12);
                popover.set_child(Some(&label));
                popover.set_parent(button);
                // A fresh popover is built on every click rather than
                // reused, so it needs to unparent itself once closed —
                // otherwise each click would leave the previous one
                // still attached to the button, accumulating silently.
                popover.connect_closed(|popover| popover.unparent());
                popover.popup();
            });
        }
        row.add_suffix(&info_button);

        let atom = atom.clone();
        let flag_name = flag.name.clone();
        row.connect_active_notify(move |row| {
            if let Err(err) = package_use::set_flag(&atom, &flag_name, row.is_active())
                && let Some(window) = row.root().and_downcast::<gtk::Window>() {
                    let dialog = adw::AlertDialog::new(
                        Some("Couldn't save USE flag"),
                        Some(&err.to_string()),
                    );
                    dialog.add_response("ok", "OK");
                    dialog.present(Some(&window));
                }
        });
        group.add(&row);
    }
    Some(group)
}

/// One row's worth of icon + label for the "⋮" overflow menu — plain
/// widgets in a flat button rather than an `adw::ButtonRow`/`ActionRow`,
/// so the popover reads as a compact menu instead of a second settings
/// list.
fn menu_row_content(icon_name: &str, label: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.append(&gtk::Image::from_icon_name(icon_name));
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    row.append(&text);
    row
}

/// The confirm-and-build dialog shared by both sandbox-build entry
/// points (the auto-revealed pill after a failed `--pretend`, and the
/// "⋮" menu's always-available copy of the same option) — kept as one
/// function so the two can't drift into showing different wording for
/// the same action.
fn confirm_sandbox_build(button: &gtk::Button, atom: &str, display_name: &str, on_sandbox_build: &Rc<dyn Fn(String)>) {
    let Some(window) = button.root().and_downcast::<gtk::Window>() else {
        return;
    };
    let first_run = !crate::portage::sandbox::is_set_up();
    let body = if first_run {
        format!(
            "{display_name} couldn't be resolved on your system directly. This builds it \
             in an isolated, disposable Gentoo root instead — masks and keyword \
             restrictions are ignored there, but nothing on your real system is touched.\n\n\
             This is the first sandbox build: it also downloads and sets up a base system \
             first (several hundred MB, plus normal build time on top). Continue?"
        )
    } else {
        format!(
            "Builds {display_name} in the isolated sandbox root, ignoring masks and \
             keyword restrictions there. Nothing on your real system is touched. Continue?"
        )
    };
    let dialog = adw::AlertDialog::new(Some("Build in Isolated Sandbox?"), Some(&body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("build", "Build");
    dialog.set_response_appearance("build", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("build"));
    dialog.set_close_response("cancel");

    let atom = atom.to_string();
    let on_sandbox_build = on_sandbox_build.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "build" {
            on_sandbox_build(atom.clone());
        }
    });
    dialog.present(Some(&window));
}

/// Builds the package detail page: the hero, the four fact tiles, the long
/// description, upstream links and the USE flag switches.
pub fn build(
    pkg: &PackageSummary,
    installed: &HashMap<String, InstalledPackage>,
    installed_icons: &HashMap<String, PathBuf>,
    // Whether to pass `--getbinpkg` to this page's own `--pretend` run —
    // read once at open time from `self.settings` rather than through the
    // window-action-lookup trick `github_preview_enabled` uses, because
    // this feeds a job dispatched synchronously during `build()`, before
    // the page is attached to the window (so `.root()` would find
    // nothing yet — the lookup trick only works for state read *after*
    // the page is showing, like the screenshot fallback toggle is).
    prefer_binpkg: bool,
    on_install: Rc<dyn Fn(String)>,
    on_uninstall: Rc<dyn Fn(String)>,
    // Queues a build of this exact atom inside the isolated sandbox chroot
    // (see `portage::sandbox`) — offered only once the `--pretend` run
    // below has actually failed, since that's the one situation it's for:
    // a mask, keyword mask, or conflict the live system's resolver won't
    // get past.
    on_sandbox_build: Rc<dyn Fn(String)>,
    // Queues a Flatpak install of whatever match `find_flatpak_match`
    // turns up for this package — offered from the same overflow menu as
    // sandbox builds, as the other "route around Portage" option, but for
    // the opposite reason: not because Portage's resolver failed, but
    // because installing prebuilt is faster than compiling from source
    // when there's a confident Flatpak match and no Portage binary.
    on_flatpak_install: Rc<dyn Fn(flatpak::FlatpakApp)>,
    // Called exactly once, as soon as the description/screenshot
    // enrichment chain (local AppStream, then Flathub, then — only if
    // still needed — Terminal Trove and GitHub) has either filled both or
    // run out of fallbacks to try. The caller uses this to hold a loading
    // page up until the detail page actually has its content, rather than
    // pushing it right away and letting the page visibly fill in piece by
    // piece.
    on_ready: Rc<dyn Fn()>,
) -> adw::NavigationPage {
    let atom = pkg.atom();
    let installed_pkg = installed.get(&atom);

    // --- hero -------------------------------------------------------
    let icon = widgets::package_image(&pkg.category, &pkg.name, installed_icons, 128);
    icon.set_valign(gtk::Align::Start);

    // Capitalized display-only — `pkg.name`/`atom` (used for the actual
    // install/uninstall calls and everywhere else) are untouched; ebuild
    // names are conventionally lowercase, but the store's own title should
    // read like a proper app name rather than a package identifier.
    let display_name = pkg
        .name
        .get(..1)
        .map(|first| format!("{}{}", first.to_uppercase(), &pkg.name[1..]))
        .unwrap_or_else(|| pkg.name.clone());
    let name_label = gtk::Label::new(Some(&display_name));
    name_label.set_xalign(0.0);
    name_label.add_css_class("hero-title");
    name_label.set_wrap(true);

    // The short description sits right under the title — where the raw
    // category name used to be — the same "name, then a one-line tagline"
    // shape GNOME Software's own hero uses.
    let summary = gtk::Label::new(Some(&pkg.description));
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.add_css_class("dim-label");

    // Whether this button currently means "Install" or "Remove" — checked
    // live at click time rather than baking one specific action in when
    // the button's built, so `App::start_next` (mod.rs) can flip it after
    // a job finishes and have the *existing* button just start meaning
    // the other thing, instead of needing to tear down and reconnect a
    // signal handler (or rebuild the whole page) to change what a click
    // does. Also stashed on the page itself (see the `NavigationPage`
    // built at the end of this function) so that flip can happen from
    // outside.
    let installed_state = Rc::new(Cell::new(installed_pkg.is_some()));
    let action_button = gtk::Button::new();
    if installed_pkg.is_some() {
        action_button.set_label("Remove");
        action_button.add_css_class("destructive-action");
    } else {
        action_button.set_label("Install");
        action_button.add_css_class("suggested-action");
    }
    action_button.set_valign(gtk::Align::Center);
    action_button.set_halign(gtk::Align::End);
    {
        let atom = atom.clone();
        let installed_state = installed_state.clone();
        let on_install = on_install.clone();
        let display_name = display_name.clone();
        action_button.connect_clicked(move |b| {
            if installed_state.get() {
                // Removal is destructive (it also depcleans anything left
                // orphaned), so unlike Install it gets a confirmation step
                // rather than firing straight from the click — a stray
                // click here isn't something "Undo" in a toast can fix.
                let Some(window) = b.root().and_downcast::<gtk::Window>() else {
                    return;
                };
                let dialog = adw::AlertDialog::new(
                    Some("Remove this package?"),
                    Some(&format!(
                        "{display_name} will be uninstalled, along with any dependencies no longer needed by anything else."
                    )),
                );
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("remove", "Remove");
                dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                let b = b.clone();
                let atom = atom.clone();
                let on_uninstall = on_uninstall.clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "remove" {
                        b.set_sensitive(false);
                        on_uninstall(atom.clone());
                    }
                });
                dialog.present(Some(&window));
            } else {
                b.set_sensitive(false);
                on_install(atom.clone());
            }
        });
    }

    // Hidden until `App::start_next` (mod.rs) drives it directly — this
    // page has no idea a job is even running otherwise, since jobs are
    // owned and queued at the app level, not per-page. Reached from there
    // via `progress_bar`/`action_button`'s "detail-progress-bar" /
    // "detail-action-button" data keys (see the `NavigationPage` built at
    // the end of this function), the same `set_data` pattern already used
    // for the window-level scrim/stack lookups above.
    let progress_bar = gtk::ProgressBar::new();
    progress_bar.set_visible(false);
    progress_bar.set_width_request(120);

    // Hidden until a failed `--pretend` run (below) reveals it — this is
    // the escape hatch for exactly that situation, so there's nothing for
    // it to do before then. Its own dialog logic lives in
    // `confirm_sandbox_build` below, shared with the "⋮" menu's own copy
    // of this same option, so the two entry points can't drift apart.
    let sandbox_button = gtk::Button::with_label("Build in Isolated Sandbox");
    sandbox_button.add_css_class("pill");
    sandbox_button.set_visible(false);
    sandbox_button.set_tooltip_text(Some(&format!(
        "Builds this package in a separate, disposable Gentoo root that ignores masks and \
         keyword restrictions — nothing about your real system's configuration changes. \
         A small pool of up to {} reused build instances shares one base system, so a \
         package only gets its own fresh instance if it actually conflicts with something \
         already built in an earlier one. First use downloads that base system (several \
         hundred MB) and can take a while.",
        crate::portage::sandbox::MAX_SANDBOX_INSTANCES
    )));
    sandbox_button.connect_clicked({
        let atom = atom.clone();
        let display_name = display_name.clone();
        let on_sandbox_build = on_sandbox_build.clone();
        move |button| confirm_sandbox_build(button, &atom, &display_name, &on_sandbox_build)
    });

    // --- overflow menu: alternate install routes ---------------------
    //
    // Two ways around the normal Portage install, offered from the same
    // "⋮" menu next to Install rather than as more always-visible
    // buttons crowding the hero: building in the isolated sandbox (for
    // when Portage's own resolver won't get past a mask or conflict) and
    // installing via Flatpak instead (for when compiling from source is
    // slower than just grabbing a prebuilt Flatpak of the same app).
    // Both rows exist unconditionally; only the Flatpak one starts
    // hidden, since whether it applies depends on two async answers
    // (a confident Flatpak match existing, and Portage having no binary
    // for this package) that haven't come back yet when the menu is
    // built.
    let overflow_menu_button = gtk::MenuButton::new();
    overflow_menu_button.set_icon_name("view-more-symbolic");
    overflow_menu_button.set_tooltip_text(Some("More install options"));
    overflow_menu_button.add_css_class("flat");
    overflow_menu_button.set_valign(gtk::Align::Center);

    let sandbox_menu_row = gtk::Button::builder().child(&menu_row_content("system-run-symbolic", "Build in Isolated Sandbox")).build();
    sandbox_menu_row.add_css_class("flat");
    sandbox_menu_row.connect_clicked({
        let atom = atom.clone();
        let display_name = display_name.clone();
        let on_sandbox_build = on_sandbox_build.clone();
        let overflow_menu_button = overflow_menu_button.clone();
        move |button| {
            overflow_menu_button.popdown();
            confirm_sandbox_build(button, &atom, &display_name, &on_sandbox_build);
        }
    });

    let flatpak_menu_row = gtk::Button::builder().child(&menu_row_content("folder-download-symbolic", "Install via Flatpak Instead")).build();
    flatpak_menu_row.add_css_class("flat");
    flatpak_menu_row.set_visible(false);
    // Filled in once `find_flatpak_match` (below) answers — the row is
    // hidden until then, so a click on it always has real data behind it.
    let flatpak_match: Rc<RefCell<Option<flatpak::FlatpakApp>>> = Rc::new(RefCell::new(None));
    flatpak_menu_row.connect_clicked({
        let flatpak_match = flatpak_match.clone();
        let on_flatpak_install = on_flatpak_install.clone();
        let overflow_menu_button = overflow_menu_button.clone();
        move |_| {
            overflow_menu_button.popdown();
            if let Some(app) = flatpak_match.borrow().clone() {
                on_flatpak_install(app);
            }
        }
    });

    let overflow_column = gtk::Box::new(gtk::Orientation::Vertical, 2);
    overflow_column.set_margin_top(6);
    overflow_column.set_margin_bottom(6);
    overflow_column.set_margin_start(6);
    overflow_column.set_margin_end(6);
    overflow_column.append(&sandbox_menu_row);
    overflow_column.append(&flatpak_menu_row);
    let overflow_popover = gtk::Popover::new();
    overflow_popover.set_child(Some(&overflow_column));
    overflow_menu_button.set_popover(Some(&overflow_popover));

    // Whether Portage has no binary for this package — set once the
    // `--pretend` run below answers (see `apply_pretend_result`); the
    // Flatpak row only makes sense to offer as a *faster* alternative
    // when the normal path would mean compiling from source, not when a
    // Portage binary is already the plan.
    let needs_source_build = Rc::new(Cell::new(false));

    // Looked up in the background, off the critical path for the page
    // itself opening — a name-based `flatpak search` plus the same
    // confidence check `backend::merge_search_results` uses for search
    // results, just for this one package. Reveals `flatpak_menu_row`
    // only once both this and the pretend result (whichever answers
    // second) agree the option is worth showing.
    {
        let package_name = pkg.name.clone();
        let flatpak_match_write = flatpak_match.clone();
        let flatpak_menu_row_write = flatpak_menu_row.clone();
        let needs_source_build_read = needs_source_build.clone();
        runtime::spawn_blocking(
            move || flatpak::search(&package_name).ok().and_then(|hits| backend::find_flatpak_match(&package_name, &hits)),
            move |found| {
                if let Some(app) = found {
                    *flatpak_match_write.borrow_mut() = Some(app);
                    flatpak_menu_row_write.set_visible(needs_source_build_read.get());
                }
            },
        );
    }

    // Install/Remove plus the "⋮" overflow menu sit side by side — the
    // menu is a permanent fixture next to the primary action, not
    // something that only appears once there's a reason for it.
    let action_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    action_row.append(&action_button);
    action_row.append(&overflow_menu_button);

    let action_column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    action_column.set_valign(gtk::Align::Center);
    action_column.set_halign(gtk::Align::End);
    action_column.append(&action_row);
    action_column.append(&progress_bar);
    action_column.append(&sandbox_button);

    let hero_text = gtk::Box::new(gtk::Orientation::Vertical, 6);
    hero_text.set_hexpand(true);
    hero_text.set_valign(gtk::Align::Center);
    hero_text.append(&name_label);
    hero_text.append(&summary);

    // Icon, then title+tagline, then the Install/Remove button (with its
    // hidden progress bar riding along underneath) — its Center valign
    // lines it up with `hero_text`'s own vertical centering, landing it
    // between the title and the tagline rather than pinned to either one.
    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 20);
    hero.append(&icon);
    hero.append(&hero_text);
    hero.append(&action_column);

    // Its own clamped, inset section — separate from `content` below —
    // so the screenshot panel between them can sit outside both clamps
    // and bleed to the window's real edges instead of just this column's.
    let content_top = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content_top.set_margin_top(24);
    content_top.set_margin_start(12);
    content_top.set_margin_end(12);

    // Hidden until the `--pretend` run below knows a download size to
    // check free space against — revealed (or left hidden) each time that
    // answers, in `apply_pretend_result`.
    let disk_space_banner = adw::Banner::new("");
    content_top.append(&disk_space_banner);

    // As `disk_space_banner`, but for blocker conflicts — portage's own
    // "[blocks B] cat/pkg (is hard blocking cat/other-1.0)" is famously
    // terse about what to actually do about it; this turns it into
    // plain "X wants A, Y wants B — pick one" language instead. No
    // autofix (unlike USE/keyword/license, there's no single obviously
    // correct choice here — that's the user's call), just a clear
    // explanation instead of the one-line "Conflicts with another
    // package" the Install Method tile alone can show.
    let blocker_banner = adw::Banner::new("");
    content_top.append(&blocker_banner);
    content_top.append(&hero);

    // Slot for artwork that may arrive from Flathub later; stays empty and
    // invisible if nothing turns up. Deliberately outside both `content`
    // clamps (added directly to `page` near the bottom of this function),
    // so screenshot_carousel()'s panel can reach the window's real edges.
    let screenshot_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // No horizontal margin on `content` itself: individual sections carry
    // their own 12px inset instead, matching `content_top` above.
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_bottom(24);
    content.set_margin_start(12);
    content.set_margin_end(12);

    // The long description: a bold intro line (its first paragraph, read
    // as a tagline) followed by the rest, collapsed to a handful of lines
    // with a "Show More" toggle beneath — the same shape GNOME Software's
    // own description block uses, rather than a wall of text.
    let body_heading = gtk::Label::new(None);
    body_heading.set_xalign(0.0);
    body_heading.set_wrap(true);
    body_heading.add_css_class("title-4");
    body_heading.add_css_class("description-heading");
    body_heading.set_visible(false);
    content.append(&body_heading);

    let body = gtk::Label::new(None);
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.add_css_class("description-body");
    body.set_visible(false);
    content.append(&body);

    let show_more = gtk::Button::with_label("Show More");
    show_more.add_css_class("pill");
    show_more.set_halign(gtk::Align::Center);
    show_more.set_visible(false);
    content.append(&show_more);

    // (collapsed, full) text for whichever description last answered —
    // `set_description` fills this in; the click handler below just swaps
    // between the two rather than re-deriving either.
    let body_text: Rc<RefCell<(String, String)>> = Rc::new(RefCell::new((String::new(), String::new())));
    let body_expanded = Cell::new(false);
    let body_for_toggle = body.clone();
    let body_text_for_toggle = body_text.clone();
    let show_more_for_toggle = show_more.clone();
    show_more.connect_clicked(move |_| {
        let expanded = !body_expanded.get();
        body_expanded.set(expanded);
        let (collapsed, full) = &*body_text_for_toggle.borrow();
        if expanded {
            body_for_toggle.set_text(full);
            show_more_for_toggle.set_label("Show Less");
        } else {
            body_for_toggle.set_text(collapsed);
            show_more_for_toggle.set_label("Show More");
        }
    });

    // Declared here, populated in its usual place further down, but
    // available now so the network-enrichment closures below can append a
    // GitHub row to it once their lookup returns.
    let details = gtk::ListBox::new();
    details.add_css_class("boxed-list");
    details.set_selection_mode(gtk::SelectionMode::None);

    // Local sources first — the package's own AppStream file, then the
    // Gentoo maintainer's metadata.xml. Both describe this exact ebuild, so
    // they beat anything fetched from elsewhere.
    let appstream = installed_pkg
        .and_then(|p| appstream::lookup(&p.category, &p.name, &p.version))
        .unwrap_or_default();

    if !appstream.screenshots.is_empty() {
        screenshot_slot.append(&screenshot_carousel(&appstream.screenshots));
    }

    let local_text = if !appstream.paragraphs.is_empty() {
        Some(appstream.paragraphs.join("\n\n"))
    } else {
        use_desc::long_description(&atom).filter(|long| *long != pkg.description)
    };
    if let Some(text) = &local_text {
        set_description(&body_heading, &body, &show_more, &body_text, text);
    }

    // Whatever the tree couldn't supply, ask Flathub for — it covers GUI
    // apps well. Only artwork and prose are taken from it: the flatpak is a
    // different build of the same upstream program, so its versions and
    // sizes would be wrong here. Tracked as cells rather than the original
    // plain bools because what's still missing can only be known *after*
    // Flathub answers, and that answer decides whether GitHub gets asked
    // next.
    let description_filled = Rc::new(Cell::new(local_text.is_some()));
    let screenshots_filled = Rc::new(Cell::new(!appstream.screenshots.is_empty()));
    let needs_icon = installed_icons.get(&atom).is_none()
        && crate::portage::icons::resolve_by_name(&pkg.name).is_none();

    {
        let on_ready = on_ready.clone();
        let name = pkg.name.clone();
        let name_for_github = pkg.name.clone();
        let atom_for_github = atom.clone();
        let hero_icon = icon.clone();
        let body_heading = body_heading.clone();
        let body = body.clone();
        let show_more = show_more.clone();
        let body_text = body_text.clone();
        let screenshot_slot = screenshot_slot.clone();
        let details = details.clone();
        let description_filled = description_filled.clone();
        let screenshots_filled = screenshots_filled.clone();
        runtime::spawn_blocking(
            move || crate::portage::flathub::lookup(&name),
            move |app| {
                if let Some(app) = &app {
                    if !description_filled.get() && !app.description.is_empty() {
                        set_description(&body_heading, &body, &show_more, &body_text, &app.description);
                        description_filled.set(true);
                    }
                    if !screenshots_filled.get() && !app.screenshots.is_empty() {
                        screenshot_slot.append(&screenshot_carousel(&app.screenshots));
                        screenshots_filled.set(true);
                    }
                    if needs_icon
                        && let Some(url) = app.icon.clone() {
                            let hero_icon = hero_icon.clone();
                            runtime::spawn_blocking(
                                move || crate::portage::media::fetch(&url),
                                move |path| {
                                    if let (Some(path), Some(image)) =
                                        (path, hero_icon.downcast_ref::<gtk::Image>())
                                    {
                                        image.set_from_file(Some(&path));
                                        image.remove_css_class("icon-fallback");
                                    }
                                },
                            );
                        }
                }

                // Neither AppStream nor Flathub knows this package: the
                // common reason is that it's a command-line tool, which has
                // no desktop entry to submit to either. From here it's two
                // fallbacks deep, tried in order of quality: Terminal Trove
                // first, since a site built specifically to showcase
                // terminal tools has an actual demo screenshot and a
                // human-written summary; GitHub last, asked only for
                // whatever gap is still left, since a repo's generic
                // social-preview card is a weaker substitute for either.
                if !description_filled.get() || !screenshots_filled.get() {
                    let on_ready = on_ready.clone();
                    let name_tt = name_for_github.clone();
                    let name_gh = name_for_github.clone();
                    let atom_gh_outer = atom_for_github.clone();
                    let body_heading = body_heading.clone();
                    let body = body.clone();
                    let show_more = show_more.clone();
                    let body_text = body_text.clone();
                    let screenshot_slot = screenshot_slot.clone();
                    let details = details.clone();
                    let description_filled = description_filled.clone();
                    let screenshots_filled = screenshots_filled.clone();
                    runtime::spawn_blocking(
                        move || crate::portage::terminaltrove::lookup(&name_tt),
                        move |entry| {
                            if let Some(entry) = entry {
                                if !description_filled.get() && !entry.description.is_empty() {
                                    set_description(&body_heading, &body, &show_more, &body_text, &entry.description);
                                    description_filled.set(true);
                                }
                                if !screenshots_filled.get() && !entry.screenshot.is_empty() {
                                    screenshot_slot.append(&screenshot_carousel(&[entry.screenshot]));
                                    screenshots_filled.set(true);
                                }
                            }

                            if !description_filled.get() || !screenshots_filled.get() {
                                let on_ready = on_ready.clone();
                                let body_heading = body_heading.clone();
                                let body = body.clone();
                                let show_more = show_more.clone();
                                let body_text = body_text.clone();
                                let screenshot_slot = screenshot_slot.clone();
                                let details = details.clone();
                                let description_filled = description_filled.clone();
                                let screenshots_filled = screenshots_filled.clone();
                                let name_gh = name_gh.clone();
                                let atom_gh = atom_gh_outer.clone();
                                runtime::spawn_blocking(
                                    move || {
                                        // Prefer the exact repo the ebuild's own
                                        // maintainer recorded over guessing by
                                        // name: a fuzzy search for a small
                                        // library like `dev-ruby/git` finds the
                                        // 60k-star `git/git` version control
                                        // system instead, because it's the far
                                        // more popular repo with a matching name.
                                        match use_desc::github_remote_id(&atom_gh) {
                                            Some(full_name) => crate::portage::github::lookup_known(&full_name),
                                            None => crate::portage::github::lookup(&name_gh),
                                        }
                                    },
                                    move |repo| {
                                        let Some(repo) = repo else {
                                            on_ready();
                                            return;
                                        };

                                        let text = if !repo.readme_summary.is_empty() {
                                            Some(repo.readme_summary.clone())
                                        } else if !repo.description.is_empty() {
                                            Some(repo.description.clone())
                                        } else {
                                            None
                                        };
                                        if !description_filled.get()
                                            && let Some(text) = text {
                                                set_description(&body_heading, &body, &show_more, &body_text, &text);
                                            }
                                        if !screenshots_filled.get() {
                                            if !repo.readme_screenshots.is_empty() {
                                                // A real screenshot from the README always
                                                // wins, and isn't affected by the preview
                                                // toggle — that setting only ever governs
                                                // the fallback card below.
                                                screenshot_slot
                                                    .append(&screenshot_carousel(&repo.readme_screenshots));
                                            } else {
                                                // No screenshot of its own: the social-preview
                                                // card stands in instead, but only while the
                                                // toggle allows it — it's the one image here
                                                // whose relevance to the actual software can't
                                                // be verified, since a maintainer can set it to
                                                // literally anything in their repo settings.
                                                // Wired to react live to the toggle for as long
                                                // as this page stays open, rather than only
                                                // reflecting whatever it was set to when the
                                                // page opened.
                                                render_github_fallback_card(
                                                    &screenshot_slot,
                                                    &repo.fallback_card,
                                                );
                                                watch_github_preview_toggle(
                                                    &screenshot_slot,
                                                    repo.fallback_card.clone(),
                                                );
                                            }
                                        }

                                        let row = adw::ActionRow::builder()
                                            .title("GitHub")
                                            .subtitle(format!("★ {} · {}", repo.stars, repo.full_name))
                                            .build();
                                        row.add_prefix(&gtk::Image::from_icon_name("system-users-symbolic"));
                                        let suffix = gtk::Image::from_icon_name("adw-external-link-symbolic");
                                        row.add_suffix(&suffix);
                                        row.set_activatable(true);
                                        let url = format!("https://github.com/{}", repo.full_name);
                                        let github_title = format!("GitHub — {}", repo.full_name);
                                        row.connect_activated(move |row| {
                                            super::webview::open(row, &github_title, &url);
                                        });
                                        details.append(&row);
                                        on_ready();
                                    },
                                );
                            } else {
                                on_ready();
                            }
                        },
                    );
                } else {
                    on_ready();
                }
            },
        );
    }

    // --- fact tiles -------------------------------------------------
    let version_text = match installed_pkg {
        Some(installed) if installed.version == pkg.latest_version => installed.version.clone(),
        Some(installed) => format!("{} → {}", installed.version, pkg.latest_version),
        None if pkg.latest_version.is_empty() => "Unknown".to_string(),
        None => pkg.latest_version.clone(),
    };
    let (size_tile, size_value) = info_tile(
        "folder-download-symbolic",
        "Download Size",
        "Calculating…",
    );
    let (method_tile, method_value) = info_tile(
        "applications-engineering-symbolic",
        "Install Method",
        "Checking…",
    );
    // Both of these need the download size, so they stay pending until the
    // `--pretend` run below answers.
    let (time_tile, time_value) = info_tile(
        "preferences-system-time-symbolic",
        "Build Time",
        "Calculating…",
    );
    let (version_tile, _) = info_tile("software-update-available-symbolic", "Version", &version_text);

    // Homogeneous so the four facts split the width evenly — which is also
    // why there are no separator widgets between them: a homogeneous box
    // would give each separator a full column's worth of space.
    let tiles = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tiles.set_homogeneous(true);
    tiles.add_css_class("card");
    for tile in [&size_tile, &method_tile, &time_tile, &version_tile] {
        tiles.append(tile);
    }
    content.append(&tiles);

    // --- expand panel --------------------------------------------------
    // Shared by all four tiles rather than one per tile: only one fact is
    // ever being looked into at a time, and a single panel that swaps its
    // content reads as "here's more about what you clicked" instead of
    // four independent accordions competing for attention.
    let expand_content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let expand_revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .child(&expand_content)
        .build();
    content.append(&expand_revealer);

    fn swap_expand_content(container: &gtk::Box, revealer: &gtk::Revealer, widget: gtk::Widget) {
        while let Some(child) = container.first_child() {
            container.remove(&child);
        }
        container.append(&widget);
        revealer.set_reveal_child(true);
    }

    /// Kicks off the qlop batch behind the Build Time panel and shows the
    /// result once it lands — called both from the tile's own click (the
    /// common case) and, if the tile was opened before `--pretend`
    /// answered, from the point where it finally does, so an early click
    /// gets the same live update instead of being stuck on a placeholder.
    fn start_time_breakdown_fetch(
        expand_content: &gtk::Box,
        expand_revealer: &gtk::Revealer,
        expanded_tile: &Rc<Cell<Option<u8>>>,
        time_breakdown_cache: &Rc<RefCell<Option<TimeBreakdown>>>,
        packages: Vec<emerge::PendingPackage>,
    ) {
        swap_expand_content(expand_content, expand_revealer, status_placeholder("Checking build history…"));
        let expand_content = expand_content.clone();
        let expand_revealer = expand_revealer.clone();
        let expanded_tile = expanded_tile.clone();
        let time_breakdown_cache = time_breakdown_cache.clone();
        runtime::spawn_blocking(
            move || {
                let atoms: Vec<String> = packages.iter().map(|pkg| pkg.atom.clone()).collect();
                let mut averages = crate::portage::qlop::average_merge_seconds_batch(&atoms);
                packages
                    .into_iter()
                    .map(|pkg| {
                        let seconds = averages.remove(&pkg.atom).map(|(secs, _)| secs);
                        (pkg, seconds)
                    })
                    .collect::<Vec<_>>()
            },
            move |entries| {
                *time_breakdown_cache.borrow_mut() = Some(entries.clone());
                // Only redraw if this panel is still the one showing — the
                // user may have switched to a different tile while the
                // qlop batch was running.
                if expanded_tile.get() == Some(2) {
                    swap_expand_content(&expand_content, &expand_revealer, time_breakdown_panel(&entries));
                }
            },
        );
    }

    // Which tile (0=size, 1=method, 2=time, 3=version) is currently
    // expanded, so a second click on the same tile collapses it instead of
    // redrawing the same content, and switching tiles doesn't require
    // tracking four separate open/closed booleans.
    let expanded_tile: Rc<Cell<Option<u8>>> = Rc::new(Cell::new(None));

    // Populated once the `--pretend` run below answers — what the expand
    // panels above are built from. `pretend_ready` flips once that's safe
    // to read; a click on a tile before then shows a "still checking"
    // placeholder instead of an empty list.
    let pending_packages: Rc<RefCell<Vec<emerge::PendingPackage>>> = Rc::new(RefCell::new(Vec::new()));
    let pretend_ready = Rc::new(Cell::new(false));

    // Download Size — breakdown of what accounts for the total.
    {
        let expand_content = expand_content.clone();
        let expand_revealer = expand_revealer.clone();
        let expanded_tile = expanded_tile.clone();
        let pending_packages = pending_packages.clone();
        let pretend_ready = pretend_ready.clone();
        size_tile.connect_clicked(move |_| {
            if expanded_tile.get() == Some(0) {
                expand_revealer.set_reveal_child(false);
                expanded_tile.set(None);
                return;
            }
            expanded_tile.set(Some(0));
            let widget = if pretend_ready.get() {
                size_breakdown_panel(&pending_packages.borrow())
            } else {
                status_placeholder("Still checking…")
            };
            swap_expand_content(&expand_content, &expand_revealer, widget);
        });
    }

    // Install Method — which dependencies are new versus reinstalled.
    {
        let expand_content = expand_content.clone();
        let expand_revealer = expand_revealer.clone();
        let expanded_tile = expanded_tile.clone();
        let pending_packages = pending_packages.clone();
        let pretend_ready = pretend_ready.clone();
        method_tile.connect_clicked(move |_| {
            if expanded_tile.get() == Some(1) {
                expand_revealer.set_reveal_child(false);
                expanded_tile.set(None);
                return;
            }
            expanded_tile.set(Some(1));
            let widget = if pretend_ready.get() {
                method_breakdown_panel(&pending_packages.borrow())
            } else {
                status_placeholder("Still checking…")
            };
            swap_expand_content(&expand_content, &expand_revealer, widget);
        });
    }

    // Build Time — per-dependency measured averages, fetched once (a batch
    // of `qlop` calls) and cached, since re-running it on every expand
    // would mean shelling out again each time the panel reopens.
    let time_breakdown_cache: Rc<RefCell<Option<TimeBreakdown>>> = Rc::new(RefCell::new(None));
    {
        let expand_content = expand_content.clone();
        let expand_revealer = expand_revealer.clone();
        let expanded_tile = expanded_tile.clone();
        let pending_packages = pending_packages.clone();
        let pretend_ready = pretend_ready.clone();
        let time_breakdown_cache = time_breakdown_cache.clone();
        time_tile.connect_clicked(move |_| {
            if expanded_tile.get() == Some(2) {
                expand_revealer.set_reveal_child(false);
                expanded_tile.set(None);
                return;
            }
            expanded_tile.set(Some(2));

            if !pretend_ready.get() {
                // Nothing to show yet — the `--pretend` run that fills
                // `pending_packages` hasn't answered. Left as "still
                // checking" rather than started here: once it does answer,
                // that same code path (below, past the job) notices this
                // tile is the one open and starts the qlop batch itself.
                swap_expand_content(&expand_content, &expand_revealer, status_placeholder("Still checking…"));
                return;
            }
            if let Some(cached) = time_breakdown_cache.borrow().as_ref() {
                swap_expand_content(&expand_content, &expand_revealer, time_breakdown_panel(cached));
                return;
            }

            start_time_breakdown_fetch(
                &expand_content,
                &expand_revealer,
                &expanded_tile,
                &time_breakdown_cache,
                pending_packages.borrow().clone(),
            );
        });
    }

    // Version — every available version, newest first, with the newest
    // one that actually resolves marked Recommended. Fetched once and
    // cached the same way as Build Time.
    let version_list_cache: Rc<RefCell<Option<Vec<String>>>> = Rc::new(RefCell::new(None));
    let recommended_version: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    {
        let expand_content = expand_content.clone();
        let expand_revealer = expand_revealer.clone();
        let expanded_tile = expanded_tile.clone();
        let version_list_cache = version_list_cache.clone();
        let recommended_version = recommended_version.clone();
        let atom_for_versions = atom.clone();
        let installed_version = installed_pkg.map(|p| p.version.clone());
        let on_install_for_version = on_install.clone();

        version_tile.connect_clicked(move |_| {
            if expanded_tile.get() == Some(3) {
                expand_revealer.set_reveal_child(false);
                expanded_tile.set(None);
                return;
            }
            expanded_tile.set(Some(3));

            let make_picker = {
                let on_install = on_install_for_version.clone();
                let atom = atom_for_versions.clone();
                move || -> Rc<dyn Fn(String)> {
                    let on_install = on_install.clone();
                    let atom = atom.clone();
                    Rc::new(move |version: String| on_install(format!("={atom}-{version}")))
                }
            };

            if let Some(versions) = version_list_cache.borrow().clone() {
                let widget = version_list_panel(
                    &versions,
                    installed_version.as_deref(),
                    recommended_version.borrow().as_deref(),
                    false,
                    make_picker(),
                );
                swap_expand_content(&expand_content, &expand_revealer, widget);
                return;
            }

            swap_expand_content(&expand_content, &expand_revealer, status_placeholder("Loading versions…"));
            let atom_fetch = atom_for_versions.clone();
            let expand_content2 = expand_content.clone();
            let expand_revealer2 = expand_revealer.clone();
            let expanded_tile2 = expanded_tile.clone();
            let version_list_cache2 = version_list_cache.clone();
            let recommended_version2 = recommended_version.clone();
            let installed_version2 = installed_version.clone();
            let make_picker2 = make_picker.clone();
            runtime::spawn_blocking(
                move || {
                    let versions = crate::portage::eix::list_versions(&atom_fetch).unwrap_or_default();
                    // Newest few only: probing every historical version is
                    // both slow (one `emerge --pretend` subprocess each)
                    // and pointless, since nobody wants a five-year-old
                    // version recommended to them. Stops at the first
                    // clean resolve, which — checked newest-first — is by
                    // definition the newest one that actually installs.
                    let recommended = versions
                        .iter()
                        .rev()
                        .take(3)
                        .find(|v| emerge::pretend_version_succeeds(&atom_fetch, v))
                        .cloned();
                    (versions, recommended)
                },
                move |(versions, recommended)| {
                    *version_list_cache2.borrow_mut() = Some(versions.clone());
                    *recommended_version2.borrow_mut() = recommended.clone();
                    if expanded_tile2.get() == Some(3) {
                        let widget = version_list_panel(
                            &versions,
                            installed_version2.as_deref(),
                            recommended.as_deref(),
                            false,
                            make_picker2(),
                        );
                        swap_expand_content(&expand_content2, &expand_revealer2, widget);
                    }
                },
            );
        });
    }

    // Filling the two unknown tiles needs portage's resolver, so kick off a
    // `--pretend` run and update them when it answers.
    // Two sources race to fill the time tile, and either order is fine:
    // a real measurement always wins, and the size-based estimate only
    // fills the gap when this package has never been merged here.
    let measured = Rc::new(std::cell::Cell::new(false));
    {
        let atom = atom.clone();
        let measured = measured.clone();
        let time_value = time_value.clone();
        runtime::spawn_blocking(
            move || crate::portage::qlop::average_merge_seconds(&atom),
            move |result| {
                if let Some((seconds, merges)) = result {
                    measured.set(true);
                    time_value.set_text(&format!(
                        "{} · {}",
                        build_time::format_duration(seconds),
                        build_time::merges_label(merges)
                    ));
                }
            },
        );
    }

    let lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let collect = lines.clone();
    let size_value_clone = size_value.clone();
    let method_value_clone = method_value.clone();
    let time_value_clone = time_value.clone();
    let prebuilt = emerge::is_prebuilt(&pkg.name);
    let measured = measured.clone();
    let pending_packages_write = pending_packages.clone();
    let pretend_ready_write = pretend_ready.clone();
    let expand_content_write = expand_content.clone();
    let expand_revealer_write = expand_revealer.clone();
    let expanded_tile_write = expanded_tile.clone();
    let time_breakdown_cache_write = time_breakdown_cache.clone();
    let atom_for_pretend_cache = atom.clone();
    let version_for_pretend_cache = pkg.latest_version.clone();
    let lines_for_apply = lines.clone();
    let sandbox_button_write = sandbox_button.clone();
    let disk_space_banner_write = disk_space_banner.clone();
    let blocker_banner_write = blocker_banner.clone();
    let needs_source_build_write = needs_source_build.clone();
    let flatpak_menu_row_write = flatpak_menu_row.clone();
    let flatpak_match_read = flatpak_match.clone();
    // Factored out of the `spawn_job` call below so a cache hit (see
    // `emerge::cached_pretend`) can run the exact same "now show it"
    // logic immediately, without a job — a `--pretend` run does a full
    // dependency resolution (often the single slowest thing this app
    // does), and re-opening a page for an atom/version pair already
    // visited this session has no reason to pay that wait twice for an
    // answer that can't have changed.
    let apply_pretend_result = move |success: bool| {
        // Parsed either way: a resolver failure still prints the package
        // list and download total before it gives up, so a failed run
        // usually knows the size even though it can't proceed.
        let lines = lines_for_apply.borrow();
        let preview: InstallPreview = emerge::parse_pretend_output(&lines);
        *pending_packages_write.borrow_mut() = emerge::parse_pretend_packages(&lines);
        pretend_ready_write.set(true);

        // If Download Size, Install Method or Build Time was opened
        // before this answered, it's showing a "still checking"
        // placeholder — refresh it now instead of leaving the user to
        // close and reopen the tile to see the real content.
        match expanded_tile_write.get() {
            Some(0) => swap_expand_content(
                &expand_content_write,
                &expand_revealer_write,
                size_breakdown_panel(&pending_packages_write.borrow()),
            ),
            Some(1) => swap_expand_content(
                &expand_content_write,
                &expand_revealer_write,
                method_breakdown_panel(&pending_packages_write.borrow()),
            ),
            Some(2) if time_breakdown_cache_write.borrow().is_none() => start_time_breakdown_fetch(
                &expand_content_write,
                &expand_revealer_write,
                &expanded_tile_write,
                &time_breakdown_cache_write,
                pending_packages_write.borrow().clone(),
            ),
            _ => {}
        }

        size_value_clone.set_text(&match preview.download_kib {
            Some(kib) => emerge::format_size_kib(kib),
            None if success => "Nothing to download".to_string(),
            None => "Unknown".to_string(),
        });

        match preview.download_kib.and_then(crate::portage::diskspace::low_space_warning) {
            Some(warning) => {
                disk_space_banner_write.set_title(&warning);
                disk_space_banner_write.set_revealed(true);
            }
            None => disk_space_banner_write.set_revealed(false),
        }

        // Offered only on an actual resolver failure — a package that
        // resolves fine on the live system has no reason to route around
        // it via the sandbox.
        sandbox_button_write.set_visible(!success);

        let blockers = emerge::parse_blockers(&lines);
        if let Some(first) = blockers.first() {
            let severity = if first.hard { "conflicts" } else { "may conflict" };
            let wants = first.wanted_by.join(", ");
            let extra = if blockers.len() > 1 { format!(" (+{} more)", blockers.len() - 1) } else { String::new() };
            blocker_banner_write.set_title(&format!(
                "{} {severity} with {wants}{extra} — remove or replace one to continue",
                first.atom
            ));
            blocker_banner_write.set_revealed(true);
        } else {
            blocker_banner_write.set_revealed(false);
        }

        // `prebuilt` (the `-bin`-named-ebuild heuristic) only catches
        // packages that are *always* a prebuilt unpack. A package that
        // isn't named that way can still resolve entirely without
        // compiling — every line in the operation came back `[binary`
        // rather than `[ebuild` — when the binhost (or a configured
        // `binrepos.conf` remote) happens to have a matching build for
        // this exact version. `parse_pretend_output` already tells the
        // two apart (`will_compile` is only set by an `[ebuild` line), so
        // this is free once `--getbinpkg` is actually part of the
        // `--pretend` run (see `pretend_install_job`) — without it, no
        // remote repo is even asked, and this stays name-heuristic-only.
        let binhost_prebuilt = !preview.will_compile && preview.packages_to_build > 0;
        let effectively_prebuilt = prebuilt || binhost_prebuilt;

        // Only meaningful once the resolver actually succeeded — a
        // failed run's "would it compile" answer isn't trustworthy (it
        // may not have gotten far enough to know), and the sandbox
        // option already covers "the resolver failed" on its own.
        needs_source_build_write.set(success && !effectively_prebuilt);
        flatpak_menu_row_write.set_visible(needs_source_build_write.get() && flatpak_match_read.borrow().is_some());

        method_value_clone.set_text(&if !success {
            emerge::failure_reason(&lines)
        } else if prebuilt {
            "Prebuilt".to_string()
        } else if preview.will_compile {
            match preview.packages_to_build {
                0 | 1 => "Built from source".to_string(),
                n => format!("Built from source, {n} packages"),
            }
        } else {
            "Prebuilt binary".to_string()
        });

        emerge::store_pretend(&atom_for_pretend_cache, &version_for_pretend_cache, success, lines.clone());

        if measured.get() {
            return;
        }
        let jobs = build_time::parallel_jobs();
        time_value_clone.set_text(&match preview.download_kib {
            Some(kib) => {
                let duration = build_time::format_duration(build_time::estimate_seconds(
                    kib, effectively_prebuilt, jobs,
                ));
                // Job count only explains a *compile*; for a prebuilt
                // package it's just an unpack and mentioning threads
                // would imply a parallelism that isn't happening.
                if effectively_prebuilt {
                    format!("≈ {duration}")
                } else {
                    format!("≈ {duration} · {}", build_time::jobs_label(jobs))
                }
            }
            None if effectively_prebuilt => "Prebuilt — no build needed".to_string(),
            None => "Unknown".to_string(),
        });
    };

    if let Some((cached_success, cached_lines)) = emerge::cached_pretend(&atom, &pkg.latest_version) {
        *lines.borrow_mut() = cached_lines;
        apply_pretend_result(cached_success);
    } else {
        runtime::spawn_job(
            emerge::pretend_install_job(&atom, prefer_binpkg),
            move |line| collect.borrow_mut().push(line),
            apply_pretend_result,
        );
    }

    // --- details + upstream links ------------------------------------
    // Declared here but populated below at its usual place in reading
    // order; the enrichment closures above hold a clone and may append a
    // GitHub row to it once their lookup returns, which — since GTK simply
    // appends whatever rows exist at the time — shows up correctly however
    // late that arrives.
    let license_row = adw::ActionRow::builder()
        .title("License")
        .subtitle(if pkg.license.is_empty() { "Unknown" } else { &pkg.license })
        .build();
    license_row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
    details.append(&license_row);

    let category_row = adw::ActionRow::builder()
        .title("Portage Category")
        .subtitle(&pkg.category)
        .build();
    category_row.add_prefix(&gtk::Image::from_icon_name("package-x-generic-symbolic"));
    details.append(&category_row);

    for url in pkg.homepage.split_whitespace() {
        details.append(&link_row("web-browser-symbolic", "Project Website", url));
    }

    // Opens a dedicated reader pulling together whatever documentation
    // exists beyond the one-line description above: the package's own man
    // page (if installed), the full GitHub README, and Terminal Trove's
    // write-up — gathered lazily, only once this row is actually clicked,
    // rather than fetched for every package visited.
    let learn_more_row = adw::ActionRow::builder()
        .title("Learn More")
        .subtitle("Full documentation, man pages, and usage info")
        .build();
    learn_more_row.add_prefix(&gtk::Image::from_icon_name("accessories-dictionary-symbolic"));
    let learn_more_spinner = gtk::Spinner::new();
    learn_more_row.add_suffix(&learn_more_spinner);
    learn_more_row.set_activatable(true);
    {
        let display_name = display_name.clone();
        let name = pkg.name.clone();
        let atom = atom.clone();
        let installed = installed_pkg.map(|p| (p.category.clone(), p.name.clone(), p.version.clone()));
        let appstream_paragraphs = appstream.paragraphs.clone();
        let loading = Rc::new(Cell::new(false));
        learn_more_row.connect_activated(move |row| {
            if loading.get() {
                return;
            }
            loading.set(true);
            learn_more_spinner.start();
            let display_name = display_name.clone();
            let name = name.clone();
            let atom = atom.clone();
            let installed = installed.clone();
            let appstream_paragraphs = appstream_paragraphs.clone();
            let row_for_done = row.clone();
            let spinner_for_done = learn_more_spinner.clone();
            let loading_for_done = loading.clone();
            runtime::spawn_blocking(
                move || gather_learn_more(installed, name, atom, appstream_paragraphs),
                move |content| {
                    loading_for_done.set(false);
                    spinner_for_done.stop();
                    present_learn_more(&row_for_done, &display_name, content);
                },
            );
        });
    }
    details.append(&learn_more_row);

    // Only meaningful for something actually installed — an uninstalled
    // package has no dependents to trace at all.
    if let Some(installed) = installed_pkg {
        let why_row = adw::ActionRow::builder()
            .title("Why Is This Installed?")
            .subtitle("Trace what depends on it, up to @world")
            .activatable(true)
            .build();
        why_row.add_prefix(&gtk::Image::from_icon_name("network-workgroup-symbolic"));
        let display_name = display_name.clone();
        let atom_with_version = format!("{}-{}", atom, installed.version);
        why_row.connect_activated(move |row| {
            super::why_installed::present(row, &display_name, atom_with_version.clone());
        });
        details.append(&why_row);
    }

    let group = gtk::Box::new(gtk::Orientation::Vertical, 8);
    group.append(&widgets::section_heading("Details"));
    group.append(&details);
    content.append(&group);

    // --- USE flags ---------------------------------------------------
    if let Some(group) = use_flags_group(pkg, installed_pkg) {
        content.append(&group);
    }

    // `content_top` and `content` each get their own clamp so they read as
    // one continuous 900px column, but `screenshot_slot` sits between them
    // outside of both — the only section that spans the full page width.
    let top_clamp = adw::Clamp::builder().maximum_size(900).child(&content_top).build();
    let bottom_clamp = adw::Clamp::builder().maximum_size(900).child(&content).build();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.append(&top_clamp);
    page.append(&screenshot_slot);
    page.append(&bottom_clamp);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&page)
        .build();

    // The same "win.github-preview" toggle the main menu has, reachable
    // here too since it's most relevant exactly when you're looking at a
    // page that might be showing a repo's social-preview card in place of
    // a real screenshot — no reason to make someone leave this page to
    // turn it off. Window-scoped actions like this resolve through any
    // widget's ancestor chain, so no reference to `App` is needed here.
    let advanced_menu = gtk::gio::Menu::new();
    advanced_menu.append(Some("GitHub Page Preview"), Some("win.github-preview"));
    let menu = gtk::gio::Menu::new();
    menu.append_submenu(Some("Advanced"), &advanced_menu);
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("view-more-symbolic");
    menu_button.set_menu_model(Some(&menu));

    let header = adw::HeaderBar::new();
    header.pack_end(&menu_button);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let page = adw::NavigationPage::builder()
        .title(&pkg.name)
        .tag(&atom)
        .child(&toolbar)
        .build();
    // SAFETY: `page` owns these for its entire lifetime and nothing else
    // ever inserts under either key, so retrieval always finds exactly the
    // widgets stored here. Let `App::start_next` (mod.rs) drive this page's
    // own progress bar and re-enable its action button directly while a
    // job for this exact atom is running, without this function needing to
    // know anything about the job queue itself.
    unsafe {
        page.set_data("detail-progress-bar", progress_bar);
        page.set_data("detail-action-button", action_button);
        page.set_data("detail-installed-state", installed_state);
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_within_budget_is_not_truncated() {
        assert_eq!(truncate_at_sentence("Short and sweet.", 320), None);
    }

    #[test]
    fn cuts_after_the_last_full_sentence_before_the_budget() {
        let text = "One. Two. Three. Four. Five. Six. Seven. Eight. Nine. Ten.";
        // Budget lands inside "Four" — the cut should land after the
        // sentence before it, not mid-word or mid-sentence.
        let collapsed = truncate_at_sentence(text, 18).unwrap();
        assert_eq!(collapsed, "One. Two. Three.");
    }

    #[test]
    fn keeps_the_final_sentence_that_fits_even_without_trailing_space() {
        // The whole string is over budget, but only by a few characters —
        // the cut still has to land after "sentence.", not spill into
        // "Second." just because the sentence boundary is near the end.
        let collapsed = truncate_at_sentence("First sentence. Second.", 20).unwrap();
        assert_eq!(collapsed, "First sentence.");
    }

    #[test]
    fn no_sentence_boundary_before_the_budget_truncates_nothing() {
        // A single unbroken run of text longer than the budget with no
        // '.'/'!'/'?' anywhere to cut at — collapsing it would mean cutting
        // mid-sentence, which is exactly what this function exists to
        // avoid, so it declines instead of guessing.
        let text = "a".repeat(500);
        assert_eq!(truncate_at_sentence(&text, 320), None);
    }
}
