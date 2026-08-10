use super::App;
use portage_store::portage::audit_log;
use crate::ui::runtime;
use adw::prelude::*;
use std::rc::Rc;

/// The "Privileged Actions" preferences page — a read-only view of the
/// priv-helper's own root-appended audit log (see `audit_log.rs`), the
/// compensating control for a passwordless doas rule: nothing in-app
/// wrote this, so this page is exactly as trustworthy as the log file
/// itself, not the app's word for what it did.
pub fn page(_app: &Rc<App>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Privileged Actions");
    page.set_icon_name(Some("security-high-symbolic"));

    let loading_group = adw::PreferencesGroup::builder().title("Loading…").build();
    page.add(&loading_group);

    let page_for_load = page.clone();
    runtime::spawn_blocking(|| audit_log::read_recent(200), move |entries| {
        page_for_load.remove(&loading_group);
        render(&page_for_load, entries);
    });

    page
}

fn render(page: &adw::PreferencesPage, entries: Vec<audit_log::AuditEntry>) {
    let group = adw::PreferencesGroup::builder()
        .title("Recent Privileged Actions")
        .description("Every call the priv-helper made as root, appended by the helper itself — not by this app.")
        .build();
    page.add(&group);

    if entries.is_empty() {
        group.add(
            &adw::ActionRow::builder()
                .title("No entries")
                .subtitle("Either nothing privileged has run yet, or the log isn't readable from here.")
                .build(),
        );
        return;
    }

    for entry in entries {
        let success = entry.exit == "0";
        let row = adw::ActionRow::builder().title(&entry.cmd).subtitle(format!("{} · {}", entry.timestamp, entry.argv)).build();
        row.add_prefix(&gtk::Image::from_icon_name(if success { "emblem-ok-symbolic" } else { "dialog-error-symbolic" }));
        group.add(&row);
    }
}
