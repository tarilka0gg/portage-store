use super::App;
use portage_store::portage::{emerge, profile};
use crate::ui::runtime;
use adw::prelude::*;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

/// The build-profile preferences page — not to be confused with
/// `preferences.rs`'s "Profile Bundle" (this app's own `@world` +
/// `/etc/portage` export/import feature). This is Portage's own
/// `eselect profile`.
pub fn page(app: &Rc<App>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Build Profile");
    page.set_icon_name(Some("preferences-system-symbolic"));

    let loading_group = adw::PreferencesGroup::builder().title("Loading profiles…").build();
    page.add(&loading_group);

    let list_group = adw::PreferencesGroup::builder()
        .title("Available Profiles")
        .description("Switching applies immediately, then previews the real impact — you can revert right after.")
        .build();

    {
        let app = app.clone();
        let page = page.clone();
        let loading_group = loading_group.clone();
        let list_group = list_group.clone();
        runtime::spawn_blocking(profile::list, move |result| {
            page.remove(&loading_group);
            match result {
                Ok(profiles) => {
                    page.add(&list_group);
                    let original_index = profiles.iter().find(|p| p.active).map(|p| p.index.clone());
                    for p in profiles {
                        let subtitle = if p.active { format!("{} · active", p.status) } else { p.status.clone() };
                        let row = adw::ActionRow::builder().title(&p.path).subtitle(subtitle).build();
                        if !p.active && let Some(original_index) = original_index.clone() {
                            let switch_button = gtk::Button::with_label("Switch");
                            switch_button.add_css_class("flat");
                            switch_button.set_valign(gtk::Align::Center);
                            let app = app.clone();
                            let target = p.clone();
                            switch_button.connect_clicked(move |button| {
                                button.set_sensitive(false);
                                start_switch(&app, target.clone(), original_index.clone());
                            });
                            row.add_suffix(&switch_button);
                        } else if p.active {
                            let badge = gtk::Image::from_icon_name("emblem-ok-symbolic");
                            row.add_suffix(&badge);
                        }
                        list_group.add(&row);
                    }
                }
                Err(err) => {
                    page.add(&adw::PreferencesGroup::builder().title("Couldn't load profiles").description(err.to_string()).build());
                }
            }
        });
    }

    page
}

/// The whole switch → preview → keep/revert flow. `original_index` is the
/// index of whatever profile was active *before* this call — captured
/// from the list already fetched in `page()`, since `eselect profile
/// list`'s own active marker moves the instant `profile::apply` succeeds,
/// so there'd be nothing left to ask for it afterward.
fn start_switch(app: &Rc<App>, target: profile::Profile, original_index: String) {
    let Some(window) = app.window.clone().upcast::<gtk::Widget>().root().and_downcast::<gtk::Window>() else {
        return;
    };
    let before = profile::resolved_use().unwrap_or_default();
    let app_for_apply = app.clone();
    let target_for_apply = target.clone();
    runtime::spawn_blocking(
        move || profile::apply(&target_for_apply.index),
        move |result| {
            if let Err(err) = result {
                app_for_apply.toast(&format!("Couldn't switch profile: {err}"));
                return;
            }
            preview_after_switch(&app_for_apply, &window, before.clone(), target.clone(), original_index.clone());
        },
    );
}

/// Runs immediately after a real, already-applied profile switch: a plain
/// `--pretend --newuse --deep @world` (the same call `check_updates`
/// already makes) reveals exactly what the new profile's USE resolution
/// would actually rebuild — the real answer, not a guess, since it's
/// portage's own resolver running against the profile that's now genuinely
/// active.
fn preview_after_switch(
    app: &Rc<App>,
    window: &gtk::Window,
    before: HashSet<String>,
    target: profile::Profile,
    original_index: String,
) {
    let getbinpkg = app.settings.borrow().prefer_binary_packages;
    let lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let collect = lines.clone();
    let app = app.clone();
    let window = window.clone();
    runtime::spawn_job(
        emerge::pretend_world_job(getbinpkg),
        move |line| collect.borrow_mut().push(line),
        move |_success| {
            let after = profile::resolved_use().unwrap_or_default();
            let pending = emerge::parse_pretend_packages(&lines.borrow());
            present_result(&app, &window, before.clone(), after, target.clone(), pending, original_index.clone());
        },
    );
}

fn present_result(
    app: &Rc<App>,
    window: &gtk::Window,
    before: HashSet<String>,
    after: HashSet<String>,
    target: profile::Profile,
    pending: Vec<emerge::PendingPackage>,
    original_index: String,
) {
    let added: Vec<&String> = {
        let mut v: Vec<&String> = after.difference(&before).collect();
        v.sort();
        v
    };
    let removed: Vec<&String> = {
        let mut v: Vec<&String> = before.difference(&after).collect();
        v.sort();
        v
    };

    let status = adw::StatusPage::builder()
        .icon_name("emblem-ok-symbolic")
        .title(format!("Switched to {}", target.path))
        .description("This is already applied — Revert undoes it, Keep This Profile leaves it as-is.")
        .build();

    let use_group = adw::PreferencesGroup::builder().title("USE Flag Changes").build();
    if added.is_empty() && removed.is_empty() {
        use_group.add(&adw::ActionRow::builder().title("No USE flags changed").build());
    } else {
        for flag in &added {
            use_group.add(&adw::ActionRow::builder().title(format!("+{flag}")).css_classes(["success"]).build());
        }
        for flag in &removed {
            use_group.add(&adw::ActionRow::builder().title(format!("-{flag}")).css_classes(["error"]).build());
        }
    }

    let impact_group = adw::PreferencesGroup::builder()
        .title("Packages Affected")
        .description(if pending.is_empty() {
            "Nothing would need rebuilding.".to_string()
        } else {
            format!("{} package(s) would be rebuilt or changed by this profile.", pending.len())
        })
        .build();
    for pkg in pending.iter().take(50) {
        impact_group.add(&adw::ActionRow::builder().title(&pkg.atom).subtitle(&pkg.version).build());
    }
    if pending.len() > 50 {
        impact_group.add(&adw::ActionRow::builder().title(format!("+ {} more", pending.len() - 50)).build());
    }

    let keep_button = gtk::Button::with_label("Keep This Profile");
    keep_button.add_css_class("suggested-action");
    keep_button.add_css_class("pill");

    let revert_button = gtk::Button::with_label("Revert");
    revert_button.add_css_class("destructive-action");
    revert_button.add_css_class("pill");

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::Center);
    actions.append(&revert_button);
    actions.append(&keep_button);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
    column.set_margin_top(8);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);
    column.append(&status);
    column.append(&use_group);
    column.append(&impact_group);
    column.append(&actions);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(640).child(&column).build())
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder().title("Profile Switched").content_width(680).content_height(640).child(&toolbar).build();

    {
        let dialog = dialog.clone();
        keep_button.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let dialog = dialog.clone();
        let app = app.clone();
        revert_button.connect_clicked(move |_| {
            let app = app.clone();
            let dialog = dialog.clone();
            let original_index = original_index.clone();
            runtime::spawn_blocking(
                move || profile::apply(&original_index),
                move |result| {
                    match result {
                        Ok(()) => app.toast("Reverted to the previous profile"),
                        Err(err) => app.toast(&format!("Couldn't revert: {err}")),
                    }
                    dialog.close();
                },
            );
        });
    }

    dialog.present(Some(window));
}
