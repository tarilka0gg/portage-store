use crate::portage::config_protect::{self, DiffSegment, PendingUpdate};
use crate::ui::runtime;
use adw::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// One row in the pending-updates list.
fn update_row(update: &PendingUpdate) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(update.file_name())
        .subtitle(update.live_path.to_string_lossy().into_owned())
        .activatable(true)
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("text-x-generic-symbolic"));
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

/// One context (unchanged) run — plain, dim, monospace, exactly as the
/// files themselves read.
fn context_widget(lines: &[String]) -> gtk::Widget {
    let label = gtk::Label::new(Some(&lines.join("\n")));
    label.set_xalign(0.0);
    label.set_wrap(false);
    label.add_css_class("monospace");
    label.add_css_class("dim-label");
    label.add_css_class("caption");
    label.set_can_focus(false);
    label.upcast()
}

/// One resolvable change: the removed (live/"mine") lines and added
/// (proposed/"theirs") lines side by side, with a two-way toggle deciding
/// which the merged file keeps. `resolution` starts `true` (take theirs)
/// — the incoming update is, after all, what was just asked for by
/// updating in the first place; a local edit worth keeping is the
/// exception, not the default.
fn change_widget(removed: &[String], added: &[String], resolution: Rc<Cell<bool>>) -> gtk::Widget {
    let mine_label = gtk::Label::new(Some(&removed.join("\n")));
    mine_label.set_xalign(0.0);
    mine_label.set_wrap(false);
    mine_label.add_css_class("monospace");
    mine_label.add_css_class("caption");
    mine_label.add_css_class("diff-removed-line");
    mine_label.set_can_focus(false);
    mine_label.set_hexpand(true);
    mine_label.set_visible(!removed.is_empty());

    let theirs_label = gtk::Label::new(Some(&added.join("\n")));
    theirs_label.set_xalign(0.0);
    theirs_label.set_wrap(false);
    theirs_label.add_css_class("monospace");
    theirs_label.add_css_class("caption");
    theirs_label.add_css_class("diff-added-line");
    theirs_label.set_can_focus(false);
    theirs_label.set_hexpand(true);
    theirs_label.set_visible(!added.is_empty());

    let sides = gtk::Box::new(gtk::Orientation::Vertical, 4);
    sides.append(&mine_label);
    sides.append(&theirs_label);

    let keep_mine = gtk::ToggleButton::with_label("Keep Mine");
    let take_theirs = gtk::ToggleButton::with_label("Take Theirs");
    take_theirs.set_group(Some(&keep_mine));
    take_theirs.set_active(resolution.get());
    keep_mine.set_active(!resolution.get());
    keep_mine.add_css_class("caption");
    take_theirs.add_css_class("caption");

    {
        let resolution = resolution.clone();
        take_theirs.connect_toggled(move |btn| {
            if btn.is_active() {
                resolution.set(true);
            }
        });
    }
    {
        keep_mine.connect_toggled(move |btn| {
            if btn.is_active() {
                resolution.set(false);
            }
        });
    }

    let toggle_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toggle_row.add_css_class("linked");
    toggle_row.set_halign(gtk::Align::Start);
    toggle_row.set_margin_top(6);
    toggle_row.append(&keep_mine);
    toggle_row.append(&take_theirs);

    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("card");
    card.set_margin_top(4);
    card.set_margin_bottom(4);
    let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
    inner.set_margin_top(10);
    inner.set_margin_bottom(10);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    inner.append(&sides);
    inner.append(&toggle_row);
    card.append(&inner);
    card.upcast()
}

/// Pushes the diff/resolve page for one pending update. `on_resolved` is
/// called once the user actually applies a decision (any of the three
/// actions), so the caller can re-scan and update its own list/banner.
fn push_resolver(nav: &adw::NavigationView, update: PendingUpdate, on_resolved: Rc<dyn Fn()>) {
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_width_request(32);
    spinner.set_height_request(32);
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 12);
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    loading.set_vexpand(true);
    loading.append(&spinner);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&loading));
    let page = adw::NavigationPage::builder().title(update.file_name()).child(&toolbar).build();
    nav.push(&page);
    // An owned handle from here on — `nav` the parameter is a borrow tied
    // to this call, but everything below needs to live inside the
    // `'static` closure `spawn_blocking` hands to the GTK main loop.
    // `NavigationView` is a cheap refcounted GObject handle, so cloning it
    // is just another reference to the same widget, not a deep copy.
    let nav = nav.clone();

    let update_for_read = update.clone();
    runtime::spawn_blocking(
        move || {
            let live = std::fs::read_to_string(&update_for_read.live_path);
            let proposed = std::fs::read_to_string(&update_for_read.proposed_path);
            (update_for_read, live, proposed)
        },
        move |(update, live, proposed)| {
            let (live, proposed) = match (live, proposed) {
                (Ok(live), Ok(proposed)) => (live, proposed),
                _ => {
                    // Not valid UTF-8 (or unreadable) — most likely a
                    // binary file under a CONFIG_PROTECT path. There's
                    // nothing to line-diff, so this falls back to the two
                    // whole-file choices only.
                    let status = status_page(&update, nav.clone(), on_resolved.clone());
                    page.set_child(Some(&status));
                    return;
                }
            };

            let segments = config_protect::diff(&live, &proposed);
            let resolutions: Vec<Rc<Cell<bool>>> =
                (0..config_protect::change_count(&segments)).map(|_| Rc::new(Cell::new(true))).collect();

            let hunks = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let mut change_index = 0;
            for segment in &segments {
                match segment {
                    DiffSegment::Context(lines) => hunks.append(&context_widget(lines)),
                    DiffSegment::Change { removed, added } => {
                        hunks.append(&change_widget(removed, added, resolutions[change_index].clone()));
                        change_index += 1;
                    }
                }
            }

            let keep_mine_all = gtk::Button::with_label("Keep Mine");
            let take_theirs_all = gtk::Button::with_label("Take Theirs");
            let save_merged = gtk::Button::with_label("Save Merged");
            save_merged.add_css_class("suggested-action");
            keep_mine_all.add_css_class("pill");
            take_theirs_all.add_css_class("pill");
            save_merged.add_css_class("pill");

            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            actions.set_halign(gtk::Align::Center);
            actions.set_margin_top(16);
            actions.append(&keep_mine_all);
            actions.append(&take_theirs_all);
            actions.append(&save_merged);

            {
                let update = update.clone();
                let nav = nav.clone();
                let on_resolved = on_resolved.clone();
                let button_for_click = keep_mine_all.clone();
                keep_mine_all.connect_clicked(move |_| {
                    apply_resolution(&nav, &button_for_click, update.clone(), on_resolved.clone(), move |u| {
                        config_protect::keep_mine(u)
                    });
                });
            }
            {
                let update = update.clone();
                let nav = nav.clone();
                let on_resolved = on_resolved.clone();
                let button_for_click = take_theirs_all.clone();
                take_theirs_all.connect_clicked(move |_| {
                    apply_resolution(&nav, &button_for_click, update.clone(), on_resolved.clone(), move |u| {
                        config_protect::take_theirs(u)
                    });
                });
            }
            {
                let update = update.clone();
                let nav = nav.clone();
                let on_resolved = on_resolved.clone();
                let segments = segments.clone();
                let resolutions = resolutions.clone();
                let button_for_click = save_merged.clone();
                save_merged.connect_clicked(move |_| {
                    let take_theirs: Vec<bool> = resolutions.iter().map(|r| r.get()).collect();
                    let merged = config_protect::build_merged(&segments, &take_theirs);
                    apply_resolution(&nav, &button_for_click, update.clone(), on_resolved.clone(), move |u| {
                        config_protect::save_merged(u, &merged)
                    });
                });
            }

            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.set_margin_top(16);
            column.set_margin_bottom(24);
            column.set_margin_start(16);
            column.set_margin_end(16);
            column.append(&hunks);
            column.append(&actions);

            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&adw::Clamp::builder().maximum_size(760).child(&column).build())
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            toolbar.set_content(Some(&scroller));
            page.set_child(Some(&toolbar));
        },
    );
}

/// The binary-file fallback: no line diff, just the two whole-file
/// choices.
fn status_page(update: &PendingUpdate, nav: adw::NavigationView, on_resolved: Rc<dyn Fn()>) -> gtk::Widget {
    let status = adw::StatusPage::builder()
        .icon_name("text-x-generic-symbolic")
        .title("Can't Preview This File")
        .description("It doesn't look like plain text, so there's nothing to line-diff — choose whole-file below.")
        .build();

    let keep_mine_all = gtk::Button::with_label("Keep Mine");
    let take_theirs_all = gtk::Button::with_label("Take Theirs");
    take_theirs_all.add_css_class("suggested-action");
    keep_mine_all.add_css_class("pill");
    take_theirs_all.add_css_class("pill");

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::Center);
    actions.append(&keep_mine_all);
    actions.append(&take_theirs_all);
    status.set_child(Some(&actions));

    {
        let update = update.clone();
        let nav = nav.clone();
        let on_resolved = on_resolved.clone();
        let button_for_click = keep_mine_all.clone();
        keep_mine_all.connect_clicked(move |_| {
            apply_resolution(&nav, &button_for_click, update.clone(), on_resolved.clone(), move |u| {
                config_protect::keep_mine(u)
            });
        });
    }
    {
        let update = update.clone();
        let nav = nav.clone();
        let button_for_click = take_theirs_all.clone();
        take_theirs_all.connect_clicked(move |_| {
            apply_resolution(&nav, &button_for_click, update.clone(), on_resolved.clone(), move |u| {
                config_protect::take_theirs(u)
            });
        });
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&status));
    toolbar.upcast()
}

/// Runs `resolve` off the main thread (it does privileged I/O via
/// `pkexec`) and, on success, pops this page back to the list and tells
/// the caller to refresh. Disables `button` while in flight so a slow
/// polkit prompt can't be double-clicked into two overlapping writes.
fn apply_resolution(
    nav: &adw::NavigationView,
    button: &gtk::Button,
    update: PendingUpdate,
    on_resolved: Rc<dyn Fn()>,
    resolve: impl FnOnce(&PendingUpdate) -> anyhow::Result<()> + Send + 'static,
) {
    button.set_sensitive(false);
    let nav = nav.clone();
    let button = button.clone();
    runtime::spawn_blocking(
        move || resolve(&update),
        move |result| {
            button.set_sensitive(true);
            if result.is_ok() {
                nav.pop();
                on_resolved();
            }
        },
    );
}

/// Opens the config-update reviewer: a list of every pending file across
/// `CONFIG_PROTECT`, each opening its diff on click.
pub fn present(anchor: &impl IsA<gtk::Widget>, updates: Vec<PendingUpdate>, on_resolved: Rc<dyn Fn()>) {
    let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    let nav = adw::NavigationView::new();

    for update in &updates {
        let row = update_row(update);
        let update = update.clone();
        let nav_for_click = nav.clone();
        let on_resolved = on_resolved.clone();
        row.connect_activated(move |_| {
            push_resolver(&nav_for_click, update.clone(), on_resolved.clone());
        });
        list.append(&row);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_margin_top(16);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);
    column.append(&list);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(640).child(&column).build())
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    let list_page = adw::NavigationPage::builder().title("Config File Updates").child(&toolbar).build();
    nav.add(&list_page);

    let dialog =
        adw::Dialog::builder().title("Config File Updates").content_width(680).content_height(760).child(&nav).build();
    dialog.present(Some(&window));
}
