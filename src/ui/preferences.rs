use super::{App, QueueEntry};
use crate::portage::overlays::{self, Overlay};
use crate::portage::{binrepos, config_history, emerge, make_conf, priv_write, profile_bundle, world};
use crate::ui::runtime;
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The make.conf editor, presented the way GNOME Software presents its own
/// settings: a preferences dialog rather than a top-level tab, since these
/// are system-wide knobs you touch rarely.
///
/// Only the well-known variables get a row; the rest of the file is left
/// byte-for-byte alone, because make.conf routinely contains shell
/// constructs (`${COMMON_FLAGS}`, conditionals) that a naive rewrite would
/// destroy.
pub fn present(app: &Rc<App>) {
    let parent = &app.window;
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Portage Settings");

    let page = adw::PreferencesPage::new();
    page.set_title("make.conf");
    page.set_icon_name(Some("preferences-system-symbolic"));

    let raw = match make_conf::read_raw() {
        Ok(raw) => raw,
        Err(err) => {
            let group = adw::PreferencesGroup::builder()
                .title("Couldn't read make.conf")
                .description(err.to_string())
                .build();
            page.add(&group);
            dialog.add(&page);
            dialog.present(Some(parent));
            return;
        }
    };

    let vars = make_conf::parse_vars(&raw);
    let pending: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));

    let group = adw::PreferencesGroup::builder()
        .title("Global Variables")
        .description("Apply to every package. Changes are saved as root.")
        .build();

    for key in make_conf::KNOWN_VARS {
        let row = adw::EntryRow::builder()
            .title(*key)
            .text(vars.get(*key).map(String::as_str).unwrap_or(""))
            .build();

        let pending = pending.clone();
        let key = key.to_string();
        row.connect_changed(move |row| {
            let mut pending = pending.borrow_mut();
            let value = row.text().to_string();
            match pending.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = value,
                None => pending.push((key.clone(), value)),
            }
        });
        group.add(&row);
    }
    page.add(&group);

    let save = gtk::Button::with_label("Save Changes");
    save.add_css_class("suggested-action");
    save.add_css_class("pill");
    save.set_halign(gtk::Align::Center);

    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    status.set_wrap(true);

    {
        let pending = pending.clone();
        let status = status.clone();
        let raw = raw.clone();
        save.connect_clicked(move |_| {
            let edits = pending.borrow();
            if edits.is_empty() {
                status.set_text("No changes to save");
                return;
            }
            let mut updated = raw.clone();
            for (key, value) in edits.iter() {
                updated = make_conf::set_var(&updated, key, value);
            }
            match priv_write::write_file_as_root(make_conf::MAKE_CONF, &updated) {
                Ok(()) => status.set_text("Saved. Changes take effect on the next build."),
                Err(err) => status.set_text(&format!("Couldn't save: {err}")),
            }
        });
    }

    let actions = adw::PreferencesGroup::new();
    actions.add(&save);
    actions.add(&status);
    page.add(&actions);

    // A read-only summary of @world: what's actually there is the result of
    // every install, so showing it here makes the "why is this installed"
    // question answerable without a terminal.
    if let Ok(atoms) = world::read() {
        let group = adw::PreferencesGroup::builder()
            .title("@world Set")
            .description(format!(
                "{} packages you installed explicitly (the rest are dependencies)",
                atoms.len()
            ))
            .build();
        for atom in atoms.iter().take(200) {
            group.add(&adw::ActionRow::builder().title(atom).build());
        }
        page.add(&group);
    }

    // What this app has actually written to `/etc/portage`, in order —
    // every privileged write here goes through a git commit (see
    // `priv_write::write_file_as_root`), so this is a real "what did the
    // GUI do to my system" record, not just a promise. Shown only once
    // there's history to show — a fresh install that's never written
    // anything has no repo yet at all.
    if config_history::is_tracked() {
        let history_group = adw::PreferencesGroup::builder()
            .title("Change History")
            .description("Every change this app has made to /etc/portage.")
            .build();

        if config_history::can_revert() {
            let revert_button = gtk::Button::with_label("Revert Last Change");
            revert_button.add_css_class("pill");
            revert_button.set_halign(gtk::Align::Start);
            let status = gtk::Label::new(None);
            status.add_css_class("dim-label");
            status.set_wrap(true);
            {
                let status = status.clone();
                revert_button.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    match config_history::revert_last() {
                        Ok(()) => status.set_text("Reverted. Reopen Settings to see the updated history."),
                        Err(err) => {
                            status.set_text(&format!("Couldn't revert: {err}"));
                            button.set_sensitive(true);
                        }
                    }
                });
            }
            history_group.add(&revert_button);
            history_group.add(&status);
        }

        if let Ok(commits) = config_history::history(20) {
            for commit in commits {
                let row = adw::ActionRow::builder().title(commit.message).subtitle(commit.relative_time).build();
                row.add_prefix(&gtk::Image::from_icon_name("document-edit-symbolic"));
                history_group.add(&row);
            }
        }
        page.add(&history_group);
    }

    dialog.add(&page);
    dialog.add(&binpkg_page());
    dialog.add(&overlays_page());
    dialog.add(&profile_page(app));
    dialog.present(Some(parent));
}

/// The `@world` + `/etc/portage` portable-profile page — export bundles
/// the current system, import applies a bundle from a `.tar.gz` chosen
/// via a file dialog (writing `/etc/portage` as root, then queuing an
/// install of the bundle's `@world` atoms through the app's normal job
/// queue rather than blocking here).
fn profile_page(app: &Rc<App>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Profile");
    page.set_icon_name(Some("send-to-symbolic"));

    let export_group = adw::PreferencesGroup::builder()
        .title("Export")
        .description("Bundle @world and /etc/portage into one file, to set up a second machine the same way.")
        .build();
    let export_row = adw::ActionRow::builder().title("Save Profile Bundle…").activatable(true).build();
    export_row.add_prefix(&gtk::Image::from_icon_name("document-save-symbolic"));
    export_group.add(&export_row);
    page.add(&export_group);

    {
        let app_for_click = app.clone();
        let row_for_click = export_row.clone();
        export_row.connect_activated(move |_| {
            let file_dialog = gtk::FileDialog::builder().title("Save Profile Bundle").initial_name("portage-profile.tar.gz").build();
            let window = app_for_click.window.clone();
            let export_row = row_for_click.clone();
            file_dialog.save(Some(&window), gtk::gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                let export_row = export_row.clone();
                runtime::spawn_blocking(
                    move || profile_bundle::export(&path).map(|()| path).map_err(|e| e.to_string()),
                    move |result| match result {
                        Ok(path) => export_row.set_subtitle(&format!("Saved to {}", path.display())),
                        Err(err) => export_row.set_subtitle(&format!("Couldn't save: {err}")),
                    },
                );
            });
        });
    }

    let import_group = adw::PreferencesGroup::builder()
        .title("Import")
        .description("Apply another machine's exported profile: writes /etc/portage as root, then queues installing its @world packages.")
        .build();
    let import_row = adw::ActionRow::builder().title("Load Profile Bundle…").activatable(true).build();
    import_row.add_prefix(&gtk::Image::from_icon_name("document-open-symbolic"));
    import_group.add(&import_row);
    page.add(&import_group);

    {
        let app_for_click = app.clone();
        let row_for_click = import_row.clone();
        import_row.connect_activated(move |_| {
            let file_dialog = gtk::FileDialog::builder().title("Open Profile Bundle").build();
            let window = app_for_click.window.clone();
            let app = app_for_click.clone();
            let import_row = row_for_click.clone();
            file_dialog.open(Some(&window), gtk::gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                let import_row = import_row.clone();
                let app = app.clone();
                runtime::spawn_blocking(
                    move || profile_bundle::extract(&path).map_err(|e| e.to_string()),
                    move |result| match result {
                        Ok(bundle) => confirm_import(&app, bundle),
                        Err(err) => import_row.set_subtitle(&format!("Couldn't open: {err}")),
                    },
                );
            });
        });
    }

    page
}

/// Shows what an import would actually do (a config-file overwrite plus
/// however many packages) before touching anything — importing isn't
/// undoable the way this app's other `/etc/portage` writes are (that
/// history is per-file diffs via `config_history`; a bulk profile import
/// is a directory-wide `cp -a`, not a single tracked write).
fn confirm_import(app: &Rc<App>, bundle: profile_bundle::ExtractedBundle) {
    let body = format!(
        "This will overwrite matching files under /etc/portage with the bundle's versions, \
         then queue installing {} package(s) from its @world set. This can't be undone \
         automatically — continue?",
        bundle.atoms.len()
    );
    let dialog = adw::AlertDialog::new(Some("Import Profile Bundle?"), Some(&body));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("import", "Import");
    dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("import"));
    dialog.set_close_response("cancel");

    let app_for_response = app.clone();
    // Wrapped so the response handler (an `Fn`, not `FnOnce` — GTK's
    // signal connection requires it, even though this dialog only ever
    // fires one response in practice) can hand the plain, `Send`
    // `ExtractedBundle` off to the background thread by value instead of
    // through an `Rc`, which itself isn't `Send`.
    let bundle = Rc::new(RefCell::new(Some(bundle)));
    dialog.connect_response(None, move |_, response| {
        if response != "import" {
            return;
        }
        let Some(bundle) = bundle.borrow_mut().take() else { return };
        let app = app_for_response.clone();
        runtime::spawn_blocking(
            move || profile_bundle::import(&bundle).map(|()| bundle.atoms.clone()).map_err(|e| e.to_string()),
            move |result| match result {
                Ok(atoms) => {
                    let getbinpkg = app.settings.borrow().prefer_binary_packages;
                    app.enqueue(QueueEntry {
                        job: emerge::install_many_job(&atoms, getbinpkg),
                        label: "Installing imported profile's @world set".to_string(),
                        mutating: true,
                        retry_with_use_fix: false,
                        known_atoms: atoms,
                    });
                    app.toast("Profile imported — install queued");
                }
                Err(err) => app.toast(&format!("Import failed: {err}")),
            },
        );
    });
    dialog.present(Some(&app.window));
}

/// The binary-repository page: what's configured (a stage3's own
/// `gentoo.conf`, pointing at Gentoo's official binhost, is usually
/// already there) plus a form to add another. Rebuilt from scratch after
/// every add/remove rather than patched in place — this list is short and
/// touched rarely, so a full rebuild is simpler than tracking which row
/// maps to which repo. `AdwPreferencesPage` has no "clear all groups"
/// call and its internal child widgets aren't something to walk directly,
/// so the groups this function itself added are tracked explicitly
/// (`added_groups`) rather than discovered by traversing the page.
fn binpkg_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Binary Packages");
    page.set_icon_name(Some("folder-download-symbolic"));
    let added_groups = Rc::new(RefCell::new(Vec::new()));
    rebuild_binpkg_page(&page, &added_groups);
    page
}

/// One overlay's row: name, URL (if it has one), and a switch that
/// enables (and, on enable, immediately syncs — an enabled-but-unsynced
/// overlay has no packages to actually install yet) or disables it.
fn overlay_row(overlay: &Overlay, status_prefix: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(format!("{status_prefix}{}", overlay.name))
        .subtitle(overlay.url.clone().unwrap_or_else(|| "No public repository listed".to_string()))
        .build();
    row.add_prefix(&gtk::Image::from_icon_name("folder-remote-symbolic"));

    let switch = gtk::Switch::new();
    switch.set_valign(gtk::Align::Center);
    switch.set_active(overlay.enabled);
    row.add_suffix(&switch);

    let name = overlay.name.clone();
    switch.connect_state_set(move |switch, active| {
        switch.set_sensitive(false);
        let switch_for_done = switch.clone();
        let job = if active { overlays::enable_and_sync_job(&name) } else { overlays::disable_job(&name) };
        runtime::spawn_job(
            job,
            |_line| {},
            move |success| {
                switch_for_done.set_sensitive(true);
                if !success {
                    // Leaves the switch wherever the failed job left the
                    // repo — reverting the toggle on failure would claim
                    // a state change that may not have actually happened
                    // (e.g. `eselect repository enable` could succeed
                    // while the following sync fails).
                    switch_for_done.set_state(switch_for_done.is_active());
                }
            },
        );
        gtk::glib::Propagation::Proceed
    });

    row
}

/// Rebuilds `guru_group` and `results_group`'s rows for the current
/// filter text — GURU always shown (search or not, matched or not; it's
/// pinned, not filtered), the rest capped to the first 50 matches so
/// typing a common substring doesn't build hundreds of rows for a list
/// nobody scrolls through in full anyway.
fn render_overlay_results(
    overlays: &[Overlay],
    filter: &str,
    guru_group: &adw::PreferencesGroup,
    guru_rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
    results_group: &adw::PreferencesGroup,
    result_rows: &Rc<RefCell<Vec<adw::ActionRow>>>,
) {
    for row in guru_rows.borrow_mut().drain(..) {
        guru_group.remove(&row);
    }
    for row in result_rows.borrow_mut().drain(..) {
        results_group.remove(&row);
    }

    if let Some(guru) = overlays.iter().find(|o| o.name == overlays::GURU) {
        let row = overlay_row(guru, "★ ");
        guru_group.add(&row);
        guru_rows.borrow_mut().push(row);
    }

    let filter = filter.to_lowercase();
    let matches = overlays
        .iter()
        .filter(|o| o.name != overlays::GURU && (filter.is_empty() || o.name.to_lowercase().contains(&filter)))
        .take(50);
    for overlay in matches {
        let row = overlay_row(overlay, "");
        results_group.add(&row);
        result_rows.borrow_mut().push(row);
    }
}

/// The overlays page: Gentoo's own answer to "where do I find this
/// package that isn't in the main tree" — several hundred community
/// repos, fetched (and cached) from Gentoo's own master list, with GURU
/// pinned at the top since it's the one worth knowing about by name
/// rather than found by scrolling past hundreds of personal ones.
fn overlays_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Overlays");
    page.set_icon_name(Some("folder-remote-symbolic"));

    let search = gtk::SearchEntry::new();
    search.set_placeholder_text(Some("Search overlays…"));
    let search_group = adw::PreferencesGroup::new();
    search_group.add(&search);
    page.add(&search_group);

    let loading_group = adw::PreferencesGroup::builder().title("Loading overlays…").build();
    page.add(&loading_group);

    let guru_group = adw::PreferencesGroup::builder().title("Recommended").build();
    let results_group =
        adw::PreferencesGroup::builder().title("All Overlays").description("Showing up to 50 matches.").build();
    let guru_rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));
    let result_rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));

    let overlays: Rc<RefCell<Vec<Overlay>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let page = page.clone();
        let loading_group = loading_group.clone();
        let overlays = overlays.clone();
        let guru_group = guru_group.clone();
        let results_group = results_group.clone();
        let guru_rows = guru_rows.clone();
        let result_rows = result_rows.clone();
        runtime::spawn_blocking(overlays::list, move |result| {
            page.remove(&loading_group);
            match result {
                Ok(list) => {
                    *overlays.borrow_mut() = list;
                    page.add(&guru_group);
                    page.add(&results_group);
                    render_overlay_results(&overlays.borrow(), "", &guru_group, &guru_rows, &results_group, &result_rows);
                }
                Err(err) => {
                    page.add(
                        &adw::PreferencesGroup::builder()
                            .title("Couldn't load overlays")
                            .description(err.to_string())
                            .build(),
                    );
                }
            }
        });
    }

    search.connect_search_changed(move |entry| {
        render_overlay_results(&overlays.borrow(), &entry.text(), &guru_group, &guru_rows, &results_group, &result_rows);
    });

    page
}

fn rebuild_binpkg_page(page: &adw::PreferencesPage, added_groups: &Rc<RefCell<Vec<adw::PreferencesGroup>>>) {
    for group in added_groups.borrow_mut().drain(..) {
        page.remove(&group);
    }

    let mut new_groups = Vec::new();
    let repos = match binrepos::read() {
        Ok(repos) => repos,
        Err(err) => {
            let error_group =
                adw::PreferencesGroup::builder().title("Couldn't read binrepos.conf").description(err.to_string()).build();
            page.add(&error_group);
            new_groups.push(error_group);
            Vec::new()
        }
    };

    let configured = adw::PreferencesGroup::builder()
        .title("Configured Repositories")
        .description("Where `--getbinpkg` looks for prebuilt packages.")
        .build();
    if repos.is_empty() {
        configured.add(&adw::ActionRow::builder().title("None configured").subtitle("Add one below").build());
    }
    for repo in &repos {
        let subtitle =
            repo.priority.map(|p| format!("{} · priority {p}", repo.sync_uri)).unwrap_or_else(|| repo.sync_uri.clone());
        let row = adw::ActionRow::builder().title(&repo.name).subtitle(subtitle).build();
        if repo.managed {
            let remove_button = gtk::Button::from_icon_name("user-trash-symbolic");
            remove_button.add_css_class("flat");
            remove_button.set_valign(gtk::Align::Center);
            remove_button.set_tooltip_text(Some("Remove"));
            let name = repo.name.clone();
            let page = page.clone();
            let added_groups = added_groups.clone();
            remove_button.connect_clicked(move |button| {
                button.set_sensitive(false);
                if binrepos::remove(&name).is_ok() {
                    rebuild_binpkg_page(&page, &added_groups);
                }
            });
            row.add_suffix(&remove_button);
        } else {
            let badge = gtk::Label::new(Some("system"));
            badge.add_css_class("dim-label");
            badge.add_css_class("caption");
            row.add_suffix(&badge);
        }
        configured.add(&row);
    }
    page.add(&configured);
    new_groups.push(configured);

    let name_row = adw::EntryRow::builder().title("Name").build();
    let uri_row = adw::EntryRow::builder().title("Sync URI").build();
    let priority_row = adw::EntryRow::builder().title("Priority (optional)").build();

    let add_button = gtk::Button::with_label("Add Repository");
    add_button.add_css_class("suggested-action");
    add_button.add_css_class("pill");
    add_button.set_halign(gtk::Align::Center);

    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    status.set_wrap(true);

    {
        let name_row = name_row.clone();
        let uri_row = uri_row.clone();
        let priority_row = priority_row.clone();
        let status = status.clone();
        let page = page.clone();
        let added_groups = added_groups.clone();
        add_button.connect_clicked(move |_| {
            let name = name_row.text().trim().to_string();
            let uri = uri_row.text().trim().to_string();
            if name.is_empty() || uri.is_empty() {
                status.set_text("Name and Sync URI are both required.");
                return;
            }
            let priority = priority_row.text().trim().parse().ok();
            match binrepos::add(&name, &uri, priority) {
                Ok(()) => rebuild_binpkg_page(&page, &added_groups),
                Err(err) => status.set_text(&format!("Couldn't save: {err}")),
            }
        });
    }

    let add_group = adw::PreferencesGroup::builder().title("Add Repository").build();
    add_group.add(&name_row);
    add_group.add(&uri_row);
    add_group.add(&priority_row);
    add_group.add(&add_button);
    add_group.add(&status);
    page.add(&add_group);
    new_groups.push(add_group);

    *added_groups.borrow_mut() = new_groups;
}
