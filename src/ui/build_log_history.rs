use portage_store::portage::build_log_history::{self, LogEntry};
use adw::prelude::*;

/// The "Past Builds" list — every stored `LogEntry`, newest first, opened
/// from the log drawer header (see `App::build`). Selecting one opens
/// `present_log` for its full, phase-collapsed, searchable content.
pub fn present(anchor: &impl IsA<gtk::Widget>) {
    let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    let entries = build_log_history::index();
    if entries.is_empty() {
        list.append(&adw::ActionRow::builder().title("No builds recorded yet").subtitle("Logs are saved here once a job finishes").build());
    }
    for entry in entries {
        let icon = if entry.success { "emblem-ok-symbolic" } else { "dialog-error-symbolic" };
        let row = adw::ActionRow::builder()
            .title(&entry.label)
            .subtitle(&relative_label(entry.unix_time))
            .activatable(true)
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(icon));
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let window_for_click = window.clone();
        row.connect_activated(move |_| present_log(&window_for_click, entry.clone()));
        list.append(&row);
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(600).child(&list).build())
        .build();
    scroller.set_margin_top(8);
    scroller.set_margin_bottom(16);
    scroller.set_margin_start(16);
    scroller.set_margin_end(16);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder().title("Past Builds").content_width(560).content_height(600).child(&toolbar).build();
    dialog.present(Some(&window));
}

/// A plain-language age, not a raw timestamp — matches the relative-time
/// style `config_history.rs` already shows for `/etc/portage`'s own
/// commit history.
fn relative_label(unix_time: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(unix_time);
    let elapsed = now.saturating_sub(unix_time);
    match elapsed {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", elapsed / 60),
        3600..=86_399 => format!("{}h ago", elapsed / 3600),
        _ => format!("{}d ago", elapsed / 86_400),
    }
}

/// One stored log's full content: split into collapsible phases (see
/// `build_log_history::split_into_phases`, grouped under portage's own
/// `>>> ...` step markers), a search box that jumps to and expands the
/// first match, and a button that does the same for the first real build
/// error (reusing `emerge::parse_build_failure`'s own detection).
fn present_log(window: &gtk::Window, entry: LogEntry) {
    let dialog = adw::Dialog::builder().title(&entry.label).content_width(760).content_height(680).build();

    let Some(text) = build_log_history::read_log(&entry) else {
        let status = adw::StatusPage::builder()
            .icon_name("dialog-error-symbolic")
            .title("Couldn't read this log")
            .description("The saved log file may have been removed.")
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&status));
        dialog.set_child(Some(&toolbar));
        dialog.present(Some(window));
        return;
    };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let phases = build_log_history::split_into_phases(&lines);

    let search_entry = gtk::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search this log"));

    let jump_to_error_button = gtk::Button::with_label("Jump to First Error");
    jump_to_error_button.add_css_class("flat");

    let header_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header_row.set_margin_top(8);
    header_row.set_margin_start(16);
    header_row.set_margin_end(16);
    header_row.append(&search_entry);
    header_row.append(&jump_to_error_button);
    search_entry.set_hexpand(true);

    let phases_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    phases_box.set_margin_top(8);
    phases_box.set_margin_bottom(16);
    phases_box.set_margin_start(16);
    phases_box.set_margin_end(16);

    // One `(ExpanderRow, TextView)` per phase — kept around so the search
    // box and "jump to first error" can both expand the right one and
    // scroll its own `TextView` to a match, rather than only rendering
    // static content.
    let mut phase_widgets: Vec<(adw::ExpanderRow, gtk::TextView)> = Vec::new();
    for phase in &phases {
        let expander = adw::ExpanderRow::builder().title(&phase.label).subtitle(format!("{} lines", phase.lines.len())).build();
        expander.add_css_class("card");

        let text_view = gtk::TextView::new();
        text_view.set_editable(false);
        text_view.set_cursor_visible(false);
        text_view.set_monospace(true);
        text_view.set_left_margin(8);
        text_view.set_top_margin(6);
        text_view.set_bottom_margin(6);
        text_view.buffer().set_text(&phase.lines.join("\n"));

        let text_scroller = gtk::ScrolledWindow::builder().min_content_height(160).max_content_height(320).child(&text_view).build();
        expander.add_row(&text_scroller);

        phases_box.append(&expander);
        phase_widgets.push((expander, text_view));
    }

    let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).vexpand(true).child(&phases_box).build();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&header_row);
    column.append(&scroller);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));

    {
        let phase_widgets = phase_widgets.clone();
        search_entry.connect_activate(move |entry| {
            let needle = entry.text().to_string();
            if !needle.is_empty() {
                highlight_first_match(&phase_widgets, &needle);
            }
        });
    }

    {
        let phase_widgets = phase_widgets.clone();
        jump_to_error_button.connect_clicked(move |_| {
            let Some(error_index) = lines.iter().position(|l| {
                let trimmed = l.trim_start();
                trimmed.starts_with("* ERROR:") || trimmed.starts_with("*ERROR:")
            }) else {
                return;
            };
            // Which phase the error line landed in — `split_into_phases`
            // groups lines in the same order they came in, so walking the
            // phase lengths back up to `error_index` finds it without
            // needing to store per-line phase indices separately.
            let mut remaining = error_index;
            for (phase, (expander, text_view)) in phases.iter().zip(phase_widgets.iter()) {
                if remaining < phase.lines.len() {
                    expander.set_expanded(true);
                    let buffer = text_view.buffer();
                    let start = buffer.iter_at_line(remaining as i32).unwrap_or_else(|| buffer.start_iter());
                    text_view.scroll_to_iter(&mut start.clone(), 0.1, false, 0.0, 0.0);
                    let mut end = start;
                    end.forward_to_line_end();
                    buffer.select_range(&start, &end);
                    break;
                }
                remaining -= phase.lines.len();
            }
        });
    }

    dialog.present(Some(window));
}

/// Finds `needle` (case-insensitive) across every phase's `TextBuffer` in
/// order, expands the first phase that has a match, selects it, and
/// scrolls it into view — a single "jump to the first hit" rather than a
/// full find-next/find-previous cycle, matching what the search box here
/// actually needs to answer ("is this string in the log, and where").
fn highlight_first_match(phase_widgets: &[(adw::ExpanderRow, gtk::TextView)], needle: &str) {
    for (expander, text_view) in phase_widgets {
        let buffer = text_view.buffer();
        let start = buffer.start_iter();
        if let Some((match_start, match_end)) = start.forward_search(needle, gtk::TextSearchFlags::CASE_INSENSITIVE, None) {
            expander.set_expanded(true);
            buffer.select_range(&match_start, &match_end);
            text_view.scroll_to_iter(&mut match_start.clone(), 0.1, false, 0.0, 0.0);
            return;
        }
    }
}
