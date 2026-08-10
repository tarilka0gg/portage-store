use portage_store::portage::emerge::BlockerInfo;
use portage_store::portage::installed::InstalledPackage;
use adw::prelude::*;
use std::collections::HashMap;
use std::rc::Rc;

/// Builds `https://bugs.gentoo.org/buglist.cgi?quicksearch=<atom>` — same
/// helper as `build_failure.rs`'s (duplicated rather than shared: it's an
/// 8-line pure function, not worth a module for one more caller).
fn bugs_search_url(atom: &str) -> String {
    let encoded: String = atom
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/') { c.to_string() } else { format!("%{:02X}", c as u32) })
        .collect();
    format!("https://bugs.gentoo.org/buglist.cgi?quicksearch={encoded}")
}

/// Turns emerge's terse `[blocks B] ...` output into a structured
/// explanation with, where a real fix exists, a concrete action.
///
/// A blocker has no single deterministic fix the way a USE/keyword/license
/// relaxation does (see `PendingRelaxation` in `ui/mod.rs`) — resolving one
/// means removing or replacing a real package, a choice this app won't
/// make silently. The one case with an honest, unambiguous action is a
/// blocking atom that's actually installed on this system: removing it and
/// retrying the install is the only real exit. When the blocking atom
/// isn't installed (a same-merge-list conflict between two packages
/// neither installed yet), there's nothing this dialog can do but explain
/// it — inventing a button there would imply a fix that doesn't exist.
pub fn present(
    anchor: &impl IsA<gtk::Widget>,
    target_atom: &str,
    blockers: &[BlockerInfo],
    installed: &HashMap<String, InstalledPackage>,
    on_remove_and_retry: Rc<dyn Fn(String, String)>,
) {
    let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let status = adw::StatusPage::builder()
        .icon_name("dialog-warning-symbolic")
        .title("Conflicts With What's Installed")
        .description(format!("{target_atom} can't be installed as-is — here's what's in the way."))
        .build();

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    for blocker in blockers {
        let severity = if blocker.hard { "Hard block" } else { "Soft block" };
        let row = adw::ActionRow::builder()
            .title(&blocker.atom)
            .subtitle(format!("Needed by {}", blocker.wanted_by.join(", ")))
            .build();
        let badge = gtk::Label::new(Some(severity));
        badge.add_css_class("caption");
        badge.add_css_class(if blocker.hard { "error" } else { "warning" });
        row.add_prefix(&badge);

        if installed.contains_key(&blocker.atom) {
            let remove_button = gtk::Button::with_label("Remove & Retry Install");
            remove_button.add_css_class("destructive-action");
            remove_button.set_valign(gtk::Align::Center);
            let blocking_atom = blocker.atom.clone();
            let target_atom = target_atom.to_string();
            let on_remove_and_retry = on_remove_and_retry.clone();
            let window_for_confirm = window.clone();
            remove_button.connect_clicked(move |button| {
                // Removal is destructive, same as the detail page's own
                // Remove button — a stray click here uninstalls a real
                // package, so it gets the same confirm-first treatment.
                let dialog = adw::AlertDialog::new(
                    Some("Remove this package?"),
                    Some(&format!("{blocking_atom} will be uninstalled, then {target_atom} will be installed.")),
                );
                dialog.add_response("cancel", "Cancel");
                dialog.add_response("remove", "Remove & Retry");
                dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                dialog.set_default_response(Some("cancel"));
                dialog.set_close_response("cancel");

                let button = button.clone();
                let blocking_atom = blocking_atom.clone();
                let target_atom = target_atom.clone();
                let on_remove_and_retry = on_remove_and_retry.clone();
                dialog.connect_response(None, move |_, response| {
                    if response == "remove" {
                        button.set_sensitive(false);
                        on_remove_and_retry(blocking_atom.clone(), target_atom.clone());
                    }
                });
                dialog.present(Some(&window_for_confirm));
            });
            row.add_suffix(&remove_button);
        }

        list.append(&row);
    }

    let search_button = gtk::Button::with_label("Search bugs.gentoo.org");
    search_button.add_css_class("pill");
    search_button.set_halign(gtk::Align::Center);
    {
        let atom = target_atom.to_string();
        let url = bugs_search_url(&atom);
        search_button.connect_clicked(move |button| {
            super::webview::open(button, &format!("bugs.gentoo.org — {atom}"), &url);
        });
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
    column.set_margin_top(8);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);
    column.append(&status);
    column.append(&list);
    column.append(&search_button);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(640).child(&column).build())
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder().title("Conflicts").content_width(680).content_height(560).child(&toolbar).build();
    dialog.present(Some(&window));
}
