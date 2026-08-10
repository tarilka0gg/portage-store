use super::{App, QueueEntry};
use portage_store::portage::eclean::{self, Target};
use crate::ui::runtime;
use adw::prelude::*;
use std::rc::Rc;

/// One target's row: a live preview (fetched in the background — running
/// `eclean-* --pretend --deep` isn't instant, so this never blocks
/// opening the dialog) plus the button to actually clean it.
fn target_row(app: &Rc<App>, target: Target) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(target.label()).subtitle("Checking…").build();
    row.add_prefix(&gtk::Image::from_icon_name("user-trash-symbolic"));

    let clean_button = gtk::Button::with_label("Clean Up");
    clean_button.add_css_class("pill");
    clean_button.set_valign(gtk::Align::Center);
    clean_button.set_sensitive(false);
    row.add_suffix(&clean_button);

    {
        let row = row.clone();
        let app = app.clone();
        clean_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            app.enqueue(QueueEntry {
                job: eclean::clean_job(target),
                label: format!("Cleaning up {}", target.label()),
                mutating: false,
                retry_with_use_fix: false,
                known_atoms: Vec::new(),
            });
            row.set_subtitle("Queued");
        });
    }

    {
        let row = row.clone();
        let clean_button = clean_button.clone();
        runtime::spawn_blocking(
            move || eclean::preview(target),
            move |result| match result {
                Ok(preview) if preview.file_count > 0 => {
                    row.set_subtitle(&format!(
                        "{} files · {} reclaimable",
                        preview.file_count,
                        portage_store::portage::emerge::format_size_kib(preview.total_kib)
                    ));
                    clean_button.set_sensitive(true);
                }
                Ok(_) => row.set_subtitle("Already clean"),
                Err(err) => row.set_subtitle(&format!("Couldn't check: {err}")),
            },
        );
    }

    row
}

/// Opens the "Free Up Space" dialog — one row per cache `eclean-dist`/
/// `eclean-pkg` can clear, each previewing what it would reclaim before
/// offering to actually do it. The natural complement to the disk-space
/// warning elsewhere in the app: instead of just "not enough space", this
/// is where to go find some.
pub fn present(app: &Rc<App>) {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.append(&target_row(app, Target::Distfiles));
    list.append(&target_row(app, Target::Binpkgs));

    let intro = gtk::Label::new(Some(
        "Downloaded sources and old binary packages that are safe to remove — a fresh copy \
         would just be re-downloaded if ever needed again.",
    ));
    intro.set_xalign(0.0);
    intro.set_wrap(true);
    intro.add_css_class("dim-label");

    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_margin_top(16);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);
    column.append(&intro);
    column.append(&list);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(560).child(&column).build())
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder().title("Free Up Space").content_width(560).content_height(420).child(&toolbar).build();
    dialog.present(Some(&app.window));
}
