use super::App;
use portage_store::portage::kernel;
use crate::ui::runtime;
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The "Kernel" preferences page — deliberately narrow, scoped to what a
/// real system inspection found actually deliverable without dangerous
/// guesswork: selecting which installed kernel sources `/usr/src/linux`
/// points at (the single most common "installed but not yet bootable"
/// gotcha), and a read-only view of what's actually in `/boot`. No
/// `make menuconfig` (not scriptable), no genkernel/dracut/bootloader
/// orchestration (not installed/applicable on the system this was built
/// against, and speculative elsewhere), no build automation or `/boot`
/// file deletion.
pub fn page(app: &Rc<App>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Kernel");
    page.set_icon_name(Some("applications-system-symbolic"));
    let added_groups: Rc<RefCell<Vec<adw::PreferencesGroup>>> = Rc::new(RefCell::new(Vec::new()));
    rebuild(app, &page, &added_groups);
    page
}

/// Rebuilds the whole page from scratch — called on first open and again
/// after a successful `kernel::select`, so the newly-selected target's
/// checkmark shows up without needing a separate targeted widget update.
/// `added_groups` tracks exactly which groups this function added (same
/// pattern `rebuild_binpkg_page`/`rebuild_env_page` already use in
/// `preferences.rs`), since `AdwPreferencesPage::remove` needs a
/// `PreferencesGroup` specifically, not a generic child widget. Both
/// underlying reads (`eselect kernel list`, a `/boot` directory listing)
/// go through `spawn_blocking` rather than running inline: they shell out
/// / hit the filesystem, and nothing on the GTK main thread should block
/// on that even briefly.
fn rebuild(app: &Rc<App>, page: &adw::PreferencesPage, added_groups: &Rc<RefCell<Vec<adw::PreferencesGroup>>>) {
    for group in added_groups.borrow_mut().drain(..) {
        page.remove(&group);
    }

    let loading_group = adw::PreferencesGroup::builder().title("Loading…").build();
    page.add(&loading_group);
    added_groups.borrow_mut().push(loading_group.clone());

    let app_for_targets = app.clone();
    let page_for_targets = page.clone();
    let added_groups_for_targets = added_groups.clone();
    runtime::spawn_blocking(kernel::list_targets, move |result| {
        page_for_targets.remove(&loading_group);
        added_groups_for_targets.borrow_mut().retain(|g| g != &loading_group);
        render_targets(&app_for_targets, &page_for_targets, &added_groups_for_targets, result);
        render_boot_entries(&page_for_targets, &added_groups_for_targets);
    });
}

fn render_targets(
    app: &Rc<App>,
    page: &adw::PreferencesPage,
    added_groups: &Rc<RefCell<Vec<adw::PreferencesGroup>>>,
    result: anyhow::Result<Vec<kernel::KernelTarget>>,
) {
    let targets_group = adw::PreferencesGroup::builder()
        .title("Kernel Symlink Targets")
        .description("Which installed kernel sources /usr/src/linux points at.")
        .build();
    page.add(&targets_group);
    added_groups.borrow_mut().push(targets_group.clone());

    match result {
        Ok(targets) if targets.is_empty() => {
            targets_group.add(&adw::ActionRow::builder().title("No kernel sources found").build());
        }
        Ok(targets) => {
            for target in targets {
                let row = adw::ActionRow::builder().title(&target.label).build();
                if target.selected {
                    row.add_suffix(&gtk::Image::from_icon_name("emblem-ok-symbolic"));
                } else {
                    let select_button = gtk::Button::with_label("Select");
                    select_button.add_css_class("flat");
                    select_button.set_valign(gtk::Align::Center);
                    let app = app.clone();
                    let page = page.clone();
                    let added_groups = added_groups.clone();
                    let index = target.index.clone();
                    select_button.connect_clicked(move |button| {
                        button.set_sensitive(false);
                        let app = app.clone();
                        let page = page.clone();
                        let added_groups = added_groups.clone();
                        let index = index.clone();
                        runtime::spawn_blocking(
                            move || kernel::select(&index),
                            move |result| {
                                match result {
                                    Ok(()) => app.toast("Kernel symlink updated"),
                                    Err(err) => app.toast(&format!("Couldn't switch kernel: {err}")),
                                }
                                rebuild(&app, &page, &added_groups);
                            },
                        );
                    });
                    row.add_suffix(&select_button);
                }
                targets_group.add(&row);
            }
        }
        Err(err) => {
            targets_group.add(&adw::ActionRow::builder().title("Couldn't list kernel targets").subtitle(err.to_string()).build());
        }
    }
}

fn render_boot_entries(page: &adw::PreferencesPage, added_groups: &Rc<RefCell<Vec<adw::PreferencesGroup>>>) {
    let page_for_boot = page.clone();
    let added_groups_for_boot = added_groups.clone();
    runtime::spawn_blocking(kernel::boot_entries, move |result| {
        let boot_group = adw::PreferencesGroup::builder()
            .title("Kernels in /boot")
            .description(
                "Installed, bootable kernel images — shown for reference only. \
                 Version names here don't necessarily match the symlink targets above; \
                 a locally renamed build can make the two impossible to line up automatically.",
            )
            .build();
        page_for_boot.add(&boot_group);
        added_groups_for_boot.borrow_mut().push(boot_group.clone());

        match result {
            Ok(entries) if entries.is_empty() => {
                boot_group.add(&adw::ActionRow::builder().title("Nothing found in /boot").build());
            }
            Ok(entries) => {
                for entry in entries {
                    boot_group.add(&adw::ActionRow::builder().title(&entry.version).build());
                }
            }
            Err(err) => {
                boot_group.add(&adw::ActionRow::builder().title("Couldn't read /boot").subtitle(err.to_string()).build());
            }
        }
    });
}
