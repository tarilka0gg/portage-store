use super::App;
use portage_store::portage::{depclean, emerge, glsa, health_history, sync};
use crate::ui::runtime;
use adw::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

/// " · pending N days", or nothing at all if there's no history yet or
/// the metric only just became nonzero — a snapshot's own point-in-time
/// count already says "3 pending"; this is the extra context that turns
/// that into "3 pending, and it's been like that for 9 days", which is
/// the actual trigger for finally doing something about it.
fn pending_days_suffix(history: &[health_history::HealthSnapshot], metric: impl Fn(&health_history::HealthSnapshot) -> usize) -> String {
    match health_history::days_pending(history, metric) {
        Some(0) | None => String::new(),
        Some(1) => " · pending 1 day".to_string(),
        Some(days) => format!(" · pending {days} days"),
    }
}

/// One row: an icon, a title, a subtitle that starts as "Checking…" and
/// is filled in once its own background check answers, and an optional
/// action button revealed only once there's something worth acting on.
fn check_row(icon: &str, title: &str) -> (adw::ActionRow, gtk::Button) {
    let row = adw::ActionRow::builder().title(title).subtitle("Checking…").build();
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    let action = gtk::Button::new();
    action.add_css_class("flat");
    action.set_valign(gtk::Align::Center);
    action.set_visible(false);
    row.add_suffix(&action);
    (row, action)
}

/// Opens the health dashboard — everything Gentoo's own tooling would
/// otherwise require knowing five different commands (`eselect news`,
/// `etc-update`, `emerge @preserved-rebuild`, `emerge --pretend
/// --depclean`, `glsa-check`, and the tree's own sync timestamp) even
/// exist to check. A system degrades quietly in every one of these ways
/// without a single error dialog ever appearing; this is the one screen
/// that says so.
pub fn present(app: &Rc<App>) {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    let history = health_history::history();

    // --- News --------------------------------------------------------
    let (news_row, news_action) = check_row("mail-unread-symbolic", "Gentoo News");
    news_action.set_label("Read");
    {
        let app = app.clone();
        news_action.connect_clicked(move |_| {
            let app_for_refresh = app.clone();
            let on_changed: Rc<dyn Fn()> = Rc::new(move || app_for_refresh.check_news());
            super::news::present(&app.window, app.news_items.borrow().clone(), on_changed);
        });
    }
    let unread_news = app.news_items.borrow().iter().filter(|i| i.unread).count();
    if unread_news == 0 {
        news_row.set_subtitle("Up to date");
    } else {
        news_row.set_subtitle(&format!("{unread_news} unread"));
        news_action.set_visible(true);
    }
    list.append(&news_row);

    // --- Config files --------------------------------------------------
    let (config_row, config_action) = check_row("text-x-generic-symbolic", "Config File Updates");
    config_action.set_label("Review");
    {
        let app = app.clone();
        config_action.connect_clicked(move |_| {
            let app_for_refresh = app.clone();
            let on_resolved: Rc<dyn Fn()> = Rc::new(move || app_for_refresh.check_config_protect());
            super::config_update::present(&app.window, app.config_protect_items.borrow().clone(), on_resolved);
        });
    }
    let pending_config = app.config_protect_items.borrow().len();
    if pending_config == 0 {
        config_row.set_subtitle("All resolved");
    } else {
        config_row.set_subtitle(&format!(
            "{pending_config} pending{}",
            pending_days_suffix(&history, |s| s.pending_config)
        ));
        config_action.set_visible(true);
    }
    list.append(&config_row);

    // --- Sync age --------------------------------------------------------
    let (sync_row, sync_action) = check_row("view-refresh-symbolic", "Package Tree");
    sync_action.set_label("Sync Now");
    {
        let app = app.clone();
        sync_action.connect_clicked(move |_| {
            app.enqueue(super::QueueEntry {
                job: sync::sync_job(),
                label: "Syncing package tree".to_string(),
                mutating: true,
                retry_with_use_fix: false,
                known_atoms: Vec::new(),
            });
        });
    }
    match sync::seconds_since_last_sync() {
        Some(seconds) => {
            sync_row.set_subtitle(&format!("Synced {}", sync::format_age(seconds)));
            sync_action.set_visible(seconds >= App::STALE_SYNC_SECONDS);
        }
        None => sync_row.set_subtitle("Never synced"),
    }
    list.append(&sync_row);

    // --- GLSA / security -------------------------------------------------
    let (glsa_row, glsa_action) = check_row("security-high-symbolic", "Security Advisories");
    glsa_action.set_label("Review");
    {
        let app = app.clone();
        glsa_action.connect_clicked(move |_| app.view_stack.set_visible_child_name("updates"));
    }
    list.append(&glsa_row);

    // Recorded once both the GLSA count and the orphan count (below) are
    // in — a single joint snapshot per dashboard visit, not two partial
    // ones, so `days_pending` is never fed a snapshot where one metric
    // is real and the other is a placeholder.
    let glsa_count_cell: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
    let orphan_count_cell: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
    let record_snapshot = {
        let glsa_count_cell = glsa_count_cell.clone();
        let orphan_count_cell = orphan_count_cell.clone();
        move || {
            if let (Some(glsa_count), Some(orphan_count)) = (glsa_count_cell.get(), orphan_count_cell.get()) {
                health_history::record(unread_news, pending_config, glsa_count, orphan_count);
            }
        }
    };

    {
        let glsa_row = glsa_row.clone();
        let glsa_action = glsa_action.clone();
        let history = history.clone();
        let glsa_count_cell = glsa_count_cell.clone();
        let record_snapshot = record_snapshot.clone();
        runtime::spawn_blocking(glsa::list_affected, move |result| {
            let count = result.map(|entries| entries.len()).unwrap_or(0);
            if count == 0 {
                glsa_row.set_subtitle("No known vulnerabilities");
            } else {
                glsa_row.set_subtitle(&format!(
                    "{count} affecting installed packages{}",
                    pending_days_suffix(&history, |s| s.glsa_count)
                ));
                glsa_action.set_visible(true);
            }
            glsa_count_cell.set(Some(count));
            record_snapshot();
        });
    }

    // --- Preserved libraries ---------------------------------------------
    // `@preserved-rebuild`'s own pretend run answers this the same way
    // any other pretend does — a package count, not a special format.
    let (preserved_row, preserved_action) = check_row("emblem-system-symbolic", "Preserved Libraries");
    preserved_action.set_label("Rebuild");
    {
        let app = app.clone();
        preserved_action.connect_clicked(move |button| {
            button.set_sensitive(false);
            app.enqueue(super::QueueEntry {
                job: emerge::install_job(
                    "@preserved-rebuild",
                    app.settings.borrow().prefer_binary_packages,
                    app.settings.borrow().buildpkg_on_install,
                ),
                label: "Rebuilding against preserved libraries".to_string(),
                mutating: true,
                retry_with_use_fix: true,
                known_atoms: Vec::new(),
            });
        });
    }
    list.append(&preserved_row);
    {
        let preserved_row = preserved_row.clone();
        let preserved_action = preserved_action.clone();
        let lines = Rc::new(std::cell::RefCell::new(Vec::new()));
        let collect = lines.clone();
        runtime::spawn_job(
            emerge::pretend_install_job("@preserved-rebuild", false),
            move |line| collect.borrow_mut().push(line),
            move |_success| {
                let preview = emerge::parse_pretend_output(&lines.borrow());
                if preview.packages_to_build == 0 {
                    preserved_row.set_subtitle("Nothing needs rebuilding");
                } else {
                    preserved_row
                        .set_subtitle(&format!("{} package(s) need rebuilding", preview.packages_to_build));
                    preserved_action.set_visible(true);
                }
            },
        );
    }

    // --- Orphaned packages (depclean) ------------------------------------
    let (orphans_row, orphans_action) = check_row("user-trash-symbolic", "Orphaned Packages");
    orphans_action.set_label("Review");
    {
        let app = app.clone();
        orphans_action.connect_clicked(move |_| super::depclean::present(&app));
    }
    list.append(&orphans_row);
    {
        let lines = Rc::new(std::cell::RefCell::new(Vec::new()));
        let collect = lines.clone();
        let orphans_row = orphans_row.clone();
        let orphans_action = orphans_action.clone();
        let history = history.clone();
        runtime::spawn_job(depclean::pretend_job(), move |line| collect.borrow_mut().push(line), move |_success| {
            let lines = lines.borrow();
            if depclean::needs_update_first(&lines) {
                orphans_row.set_subtitle("Needs a full update first");
                // Not a real "zero orphans" answer — recording 0 here
                // would misreport an unmeasured state as a resolved one.
                // The joint snapshot for this visit just doesn't happen;
                // the next visit that actually measures it will.
                return;
            }
            let count = depclean::parse_candidates(&lines).len();
            if count == 0 {
                orphans_row.set_subtitle("Nothing to remove");
            } else {
                orphans_row.set_subtitle(&format!(
                    "{count} package(s) no longer needed{}",
                    pending_days_suffix(&history, |s| s.orphan_count)
                ));
                orphans_action.set_visible(true);
            }
            orphan_count_cell.set(Some(count));
            record_snapshot();
        });
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

    let dialog =
        adw::Dialog::builder().title("System Health").content_width(640).content_height(680).child(&toolbar).build();
    dialog.present(Some(&app.window));
}
