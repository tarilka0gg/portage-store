use super::{App, QueueEntry};
use portage_store::portage::depclean;
use crate::ui::runtime;
use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

fn status_page(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    adw::StatusPage::builder().icon_name(icon).title(title).description(description).build()
}

/// Builds the review list once `--pretend --depclean` has answered: one
/// checkable row per candidate, unchecked by default (meaning "let this
/// one go" — depclean's own analysis already decided nothing else needs
/// it), and a "Remove N Packages" action that recomputes its own count
/// live as boxes are (un)checked.
fn build_review(app: &Rc<App>, candidates: Vec<String>) -> gtk::Widget {
    let intro = gtk::Label::new(Some(
        "These packages are no longer required by anything in @world or by each other. \
         Check any you want to keep — checking one adds it to @world, protecting it from \
         this and every future depclean, not just this run.",
    ));
    intro.set_xalign(0.0);
    intro.set_wrap(true);
    intro.add_css_class("dim-label");

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    // One shared cell per row, read at "Remove" time to know which atoms
    // were checked (protect) versus left alone (remove).
    let protect_flags: Rc<RefCell<Vec<(String, Rc<Cell<bool>>)>>> = Rc::new(RefCell::new(Vec::new()));

    for atom in &candidates {
        let row = adw::SwitchRow::builder().title(atom).subtitle("Keep this package").active(false).build();
        let flag = Rc::new(Cell::new(false));
        {
            let flag = flag.clone();
            row.connect_active_notify(move |row| flag.set(row.is_active()));
        }
        protect_flags.borrow_mut().push((atom.clone(), flag));
        list.append(&row);
    }

    let remove_button = gtk::Button::with_label(&format!("Remove {} Packages", candidates.len()));
    remove_button.add_css_class("destructive-action");
    remove_button.add_css_class("pill");
    remove_button.set_halign(gtk::Align::Center);
    remove_button.set_margin_top(8);

    {
        let app = app.clone();
        let protect_flags = protect_flags.clone();
        remove_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            let flags = protect_flags.borrow();
            let protect: Vec<String> = flags.iter().filter(|(_, f)| f.get()).map(|(a, _)| a.clone()).collect();
            let remove_count = flags.len() - protect.len();

            // Protection is permanent (added to @world) — queued first so
            // it's already in effect by the time the depclean job itself
            // runs, not just passed as a one-run `--exclude`.
            for atom in &protect {
                app.enqueue(QueueEntry {
                    job: depclean::noreplace_job(atom),
                    label: format!("Protecting {atom}"),
                    mutating: false,
                    retry_with_use_fix: false,
                    known_atoms: Vec::new(),
                });
            }
            app.enqueue(QueueEntry {
                job: depclean::depclean_job(&protect),
                label: format!("Removing {remove_count} orphaned packages"),
                mutating: true,
                retry_with_use_fix: false,
                known_atoms: Vec::new(),
            });
        });
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_margin_top(16);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);
    column.append(&intro);
    column.append(&list);
    column.append(&remove_button);

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(640).child(&column).build())
        .build()
        .upcast()
}

/// Opens the full-system orphan review — distinct from the per-package
/// `--depclean <atom>` a single Remove already uses, this is the sweep
/// that considers *everything* Portage's dependency graph no longer
/// justifies keeping. The single most dangerous routine operation in
/// Gentoo if run blind, which is why this is a review with per-package
/// exclusion rather than a button that just runs `--depclean`.
pub fn present(app: &Rc<App>) {
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&status_page(
        "content-loading-symbolic",
        "Checking…",
        "Resolving the full dependency graph — this can take a moment.",
    )));

    let dialog = adw::Dialog::builder()
        .title("Remove Orphaned Packages")
        .content_width(640)
        .content_height(680)
        .child(&toolbar)
        .build();
    dialog.present(Some(&app.window));

    let lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let collect = lines.clone();
    let app_for_done = app.clone();
    let toolbar_for_done = toolbar.clone();
    runtime::spawn_job(
        depclean::pretend_job(),
        move |line| collect.borrow_mut().push(line),
        move |_success| {
            let lines = lines.borrow();
            if depclean::needs_update_first(&lines) {
                toolbar_for_done.set_content(Some(&status_page(
                    "dialog-warning-symbolic",
                    "Update First",
                    "Depclean won't remove anything until the whole dependency graph resolves \
                     cleanly. Run a full system update (Update All, on the Updates tab), then \
                     try this again.",
                )));
                return;
            }
            let candidates = depclean::parse_candidates(&lines);
            if candidates.is_empty() {
                toolbar_for_done.set_content(Some(&status_page(
                    "object-select-symbolic",
                    "Nothing to Remove",
                    "No orphaned packages found.",
                )));
                return;
            }
            toolbar_for_done.set_content(Some(&build_review(&app_for_done, candidates)));
        },
    );
}
