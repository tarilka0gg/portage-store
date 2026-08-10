use super::{App, QueueEntry};
use portage_store::portage::overlays::{self, Overlay};
use portage_store::portage::{binrepos, config_history, emerge, make_conf, package_env, priv_write, profile_bundle, world};
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
/// Adds `page` to `stack` under `name`, plus a matching row in
/// `sidebar_list` — reusing whatever title/icon the page already set on
/// itself (`AdwPreferencesPage::title`/`icon_name`, which every
/// page-building function here already calls) rather than duplicating
/// those strings a second time just for the sidebar row.
///
/// A hand-built `ListBox` row rather than `gtk::StackSidebar`:
/// `StackSidebar` is a real GTK widget for exactly this "sidebar driving a
/// stack" job, but it's title-only by design — it has no way to show a
/// `StackPage`'s icon at all, which is the one thing this needed after
/// moving off the bottom switcher (that one showed icon + label together).
fn add_switcher_page(stack: &gtk::Stack, sidebar_list: &gtk::ListBox, page: &adw::PreferencesPage, name: &str) {
    stack.add_named(page, Some(name));

    let content = adw::ActionRow::builder().title(page.title()).activatable(true).build();
    if let Some(icon) = page.icon_name() {
        content.add_prefix(&gtk::Image::from_icon_name(&icon));
    }

    // Built explicitly (rather than `sidebar_list.append(&content)`,
    // which would auto-wrap `content` in a `GtkListBoxRow` of its own) so
    // `name` can be stashed on the actual row `connect_row_activated`
    // below receives — setting it on `content` instead would set it on
    // the wrong widget, one level too deep for the signal handler to see.
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&content));
    row.set_widget_name(name);
    sidebar_list.append(&row);
}

pub fn present(app: &Rc<App>) {
    let parent = &app.window;

    // `AdwPreferencesDialog`'s own built-in page switcher is a bottom tab
    // bar with no left-sidebar mode in this libadwaita version — a plain
    // `gtk::Stack` driven by a hand-built row list gives the actual
    // left-side, icon-and-title page list instead, at the cost of
    // building the dialog shell by hand. Every individual page below is
    // still an ordinary `adw::PreferencesPage` (self-contained,
    // independently scrollable), so none of that code needed to change —
    // only how the pages get assembled together.
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.set_hexpand(true);

    let sidebar_list = gtk::ListBox::new();
    // The same style class `gtk::StackSidebar` itself uses internally —
    // gets the same selected-row highlight and background this dialog
    // had before, without inheriting that widget's title-only limitation.
    sidebar_list.add_css_class("navigation-sidebar");

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
            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            toolbar.set_content(Some(&page));
            let dialog = adw::Dialog::builder()
                .title("Portage Settings")
                .content_width(900)
                .content_height(700)
                .child(&toolbar)
                .build();
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
            .description("Every change this app has made to /etc/portage — view the diff or revert to any point.")
            .build();

        let status = gtk::Label::new(None);
        status.add_css_class("dim-label");
        status.set_wrap(true);

        if let Ok(commits) = config_history::history(50) {
            // The repo's very first commit is always the pre-app baseline
            // snapshot (see `priv_write`'s doc comments) — reverting *to*
            // it is meaningless (there's nothing before it) and reverting
            // *it* would mean undoing something this app never did, so it
            // gets a diff button but no revert button, same distinction
            // `can_revert()` already draws for the single "revert last"
            // action this replaces.
            let revertable = commits.len().saturating_sub(1);
            for (i, commit) in commits.into_iter().enumerate() {
                let row = adw::ActionRow::builder()
                    .title(&commit.message)
                    .subtitle(format!("{} · {}", commit.relative_time, commit.hash))
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("document-edit-symbolic"));

                let diff_button = gtk::Button::from_icon_name("text-x-generic-symbolic");
                diff_button.add_css_class("flat");
                diff_button.set_valign(gtk::Align::Center);
                diff_button.set_tooltip_text(Some("View diff"));
                let hash_for_diff = commit.hash.clone();
                let parent_for_diff = parent.clone();
                let message_for_diff = commit.message.clone();
                diff_button.connect_clicked(move |_| {
                    present_commit_diff(&parent_for_diff, &message_for_diff, &hash_for_diff);
                });
                row.add_suffix(&diff_button);

                if i < revertable {
                    let revert_button = gtk::Button::from_icon_name("edit-undo-symbolic");
                    revert_button.add_css_class("flat");
                    revert_button.set_valign(gtk::Align::Center);
                    revert_button.set_tooltip_text(Some("Revert to here"));
                    let hash_for_revert = commit.hash.clone();
                    let status = status.clone();
                    let parent_for_confirm = parent.clone();
                    revert_button.connect_clicked(move |button| {
                        let dialog = adw::AlertDialog::new(
                            Some("Revert to this point?"),
                            Some("This rewrites the live files under /etc/portage back to how they looked at this commit, as a new commit of its own."),
                        );
                        dialog.add_response("cancel", "Cancel");
                        dialog.add_response("revert", "Revert");
                        dialog.set_response_appearance("revert", adw::ResponseAppearance::Destructive);
                        dialog.set_default_response(Some("cancel"));
                        dialog.set_close_response("cancel");
                        let button = button.clone();
                        let hash_for_revert = hash_for_revert.clone();
                        let status = status.clone();
                        dialog.connect_response(None, move |_, response| {
                            if response != "revert" {
                                return;
                            }
                            button.set_sensitive(false);
                            match config_history::revert(&hash_for_revert) {
                                Ok(()) => status.set_text("Reverted. Reopen Settings to see the updated history."),
                                Err(err) => {
                                    status.set_text(&format!("Couldn't revert: {err}"));
                                    button.set_sensitive(true);
                                }
                            }
                        });
                        dialog.present(Some(&parent_for_confirm));
                    });
                    row.add_suffix(&revert_button);
                }

                history_group.add(&row);
            }
        }
        history_group.add(&status);
        page.add(&history_group);
    }

    add_switcher_page(&stack, &sidebar_list, &page, "make-conf");
    add_switcher_page(&stack, &sidebar_list, &binpkg_page(), "binpkg");
    add_switcher_page(&stack, &sidebar_list, &overlays_page(), "overlays");
    add_switcher_page(&stack, &sidebar_list, &env_page(), "env-files");
    add_switcher_page(&stack, &sidebar_list, &super::profile_switch::page(app), "build-profile");
    add_switcher_page(&stack, &sidebar_list, &super::kernel_page::page(app), "kernel");
    add_switcher_page(&stack, &sidebar_list, &super::audit_log_page::page(app), "privileged-actions");
    add_switcher_page(&stack, &sidebar_list, &profile_page(app), "profile-bundle");

    // The stack starts on its first-added page on its own, but the
    // sidebar's own selection doesn't follow automatically — without this
    // the list opens with nothing highlighted even though "make.conf" is
    // what's actually showing.
    if let Some(first_row) = sidebar_list.row_at_index(0) {
        sidebar_list.select_row(Some(&first_row));
    }
    let stack_for_selection = stack.clone();
    sidebar_list.connect_row_activated(move |_, row| {
        stack_for_selection.set_visible_child_name(&row.widget_name());
    });

    let sidebar_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .width_request(200)
        .child(&sidebar_list)
        .build();

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&sidebar_scroller);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&stack);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body));

    let dialog =
        adw::Dialog::builder().title("Portage Settings").content_width(900).content_height(700).child(&toolbar).build();
    dialog.present(Some(parent));
}

/// One commit's full patch — a plain, read-only `git show`, fetched
/// synchronously (a local, single-commit diff is fast enough that this
/// file's other git reads, like `history()` above, already run inline
/// rather than through `runtime::spawn_blocking`).
fn present_commit_diff(parent: &adw::ApplicationWindow, message: &str, hash: &str) {
    let text_view = gtk::TextView::new();
    text_view.set_editable(false);
    text_view.set_cursor_visible(false);
    text_view.set_monospace(true);
    text_view.set_left_margin(8);
    text_view.set_top_margin(6);
    text_view.set_bottom_margin(6);
    text_view.buffer().set_text(&config_history::diff(hash).unwrap_or_else(|err| format!("Couldn't load diff: {err}")));

    let scroller = gtk::ScrolledWindow::builder().vexpand(true).child(&text_view).build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder().title(message).content_width(700).content_height(600).child(&toolbar).build();
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
                    move || {
                        let bundle = profile_bundle::extract(&path).map_err(|e| e.to_string())?;
                        let diff = profile_bundle::diff(&bundle).map_err(|e| e.to_string())?;
                        Ok::<_, String>((bundle, diff))
                    },
                    move |result| match result {
                        Ok((bundle, diff)) => present_import_diff(&app, bundle, diff),
                        Err(err) => import_row.set_subtitle(&format!("Couldn't open: {err}")),
                    },
                );
            });
        });
    }

    page
}

/// Shows exactly what moving to this bundle would change before touching
/// anything: which packages it has that this machine doesn't (checkable —
/// cherry-pick which of those actually get queued), which packages exist
/// only on this machine (informational; nothing here ever gets removed),
/// and how many `package.use` entries disagree. Importing isn't undoable
/// the way this app's other `/etc/portage` writes are (that history is
/// per-file diffs via `config_history`; a bulk profile import is a
/// directory-wide `cp -a`, not a single tracked write), so this is the
/// one look before it happens.
fn present_import_diff(app: &Rc<App>, bundle: profile_bundle::ExtractedBundle, diff: profile_bundle::BundleDiff) {
    let dialog = adw::Dialog::builder().title("Import Profile Bundle").content_width(560).content_height(640).build();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
    column.set_margin_top(16);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);

    let intro = gtk::Label::new(Some(
        "Writes /etc/portage as root (existing files not mentioned in the bundle are left alone), \
         then queues installing whichever packages below are checked.",
    ));
    intro.set_wrap(true);
    intro.set_xalign(0.0);
    intro.add_css_class("dim-label");
    column.append(&intro);

    // Cherry-pickable: every atom here defaults checked, but nothing
    // says a bundle from a very different machine should be installed
    // wholesale — a laptop importing a desktop's profile might
    // deliberately skip its `x11-drivers/nvidia-drivers` pick, say.
    let new_atoms_group = adw::PreferencesGroup::builder()
        .title(format!("Packages to Add ({})", diff.atoms_only_in_bundle.len()))
        .description("On the bundle's @world, not this machine's — checked ones get queued for install.")
        .build();
    let checkboxes: Rc<RefCell<Vec<(String, gtk::CheckButton)>>> = Rc::new(RefCell::new(Vec::new()));
    if diff.atoms_only_in_bundle.is_empty() {
        new_atoms_group.add(&adw::ActionRow::builder().title("Nothing — already up to date with this bundle").build());
    } else {
        for atom in &diff.atoms_only_in_bundle {
            let row = adw::ActionRow::builder().title(atom).build();
            let check = gtk::CheckButton::new();
            check.set_active(true);
            check.set_valign(gtk::Align::Center);
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));
            new_atoms_group.add(&row);
            checkboxes.borrow_mut().push((atom.clone(), check));
        }
    }
    column.append(&new_atoms_group);

    if !diff.atoms_only_here.is_empty() {
        let here_only = adw::ExpanderRow::builder()
            .title(format!("Only on This Machine ({})", diff.atoms_only_here.len()))
            .subtitle("Not touched — importing never removes packages")
            .build();
        for atom in &diff.atoms_only_here {
            here_only.add_row(&adw::ActionRow::builder().title(atom).build());
        }
        let group = adw::PreferencesGroup::new();
        group.add(&here_only);
        column.append(&group);
    }

    if !diff.use_flag_differences.is_empty() {
        let use_diffs = adw::ExpanderRow::builder()
            .title(format!("USE Flag Differences ({})", diff.use_flag_differences.len()))
            .subtitle("Applied as part of the /etc/portage write above")
            .build();
        for d in &diff.use_flag_differences {
            let format_side = |v: Option<bool>| match v {
                Some(true) => format!("+{}", d.flag),
                Some(false) => format!("-{}", d.flag),
                None => "(unset)".to_string(),
            };
            let row = adw::ActionRow::builder()
                .title(&d.atom)
                .subtitle(format!("bundle: {}  ·  here: {}", format_side(d.bundle), format_side(d.here)))
                .build();
            use_diffs.add_row(&row);
        }
        let group = adw::PreferencesGroup::new();
        group.add(&use_diffs);
        column.append(&group);
    }

    let import_button = gtk::Button::with_label("Import");
    import_button.add_css_class("suggested-action");
    import_button.add_css_class("pill");
    import_button.set_halign(gtk::Align::Center);
    column.append(&import_button);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(560).child(&column).build())
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));

    // Wrapped so the click handler (an `Fn`, not `FnOnce` — GTK requires
    // it even though this only ever fires once in practice) can hand the
    // plain, `Send` `ExtractedBundle` off to the background thread by
    // value instead of through an `Rc`, which itself isn't `Send`.
    let bundle = Rc::new(RefCell::new(Some(bundle)));
    let app_for_click = app.clone();
    let dialog_for_click = dialog.clone();
    import_button.connect_clicked(move |button| {
        let Some(bundle) = bundle.borrow_mut().take() else { return };
        button.set_sensitive(false);
        let selected_atoms: Vec<String> =
            checkboxes.borrow().iter().filter(|(_, check)| check.is_active()).map(|(atom, _)| atom.clone()).collect();
        let app = app_for_click.clone();
        dialog_for_click.close();
        runtime::spawn_blocking(
            move || profile_bundle::import(&bundle).map_err(|e| e.to_string()),
            move |result| match result {
                Ok(()) => {
                    if !selected_atoms.is_empty() {
                        let getbinpkg = app.settings.borrow().prefer_binary_packages;
                        let buildpkg = app.settings.borrow().buildpkg_on_install;
                        app.enqueue(QueueEntry {
                            job: emerge::install_many_job(&selected_atoms, getbinpkg, buildpkg),
                            label: "Installing imported profile's packages".to_string(),
                            mutating: true,
                            retry_with_use_fix: false,
                            known_atoms: selected_atoms.clone(),
                        });
                    }
                    app.toast("Profile imported");
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

/// The `/etc/portage/env` + `package.env` page — every named environment
/// override file (raw `CFLAGS`/`FEATURES`/`CC`/... shell fragments, edited
/// as free text since unlike `package.use` there's no fixed vocabulary of
/// tokens to build a structured editor around) plus which atoms reference
/// which files. Rebuilt from scratch after every change, same convention
/// `binpkg_page`/`overlays_page` already use.
fn env_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Env Files");
    page.set_icon_name(Some("text-x-script-symbolic"));
    let added_groups = Rc::new(RefCell::new(Vec::new()));
    rebuild_env_page(&page, &added_groups);
    page
}

fn rebuild_env_page(page: &adw::PreferencesPage, added_groups: &Rc<RefCell<Vec<adw::PreferencesGroup>>>) {
    for group in added_groups.borrow_mut().drain(..) {
        page.remove(&group);
    }
    let mut new_groups = Vec::new();

    // --- existing env files ---------------------------------------------
    let files_group = adw::PreferencesGroup::builder()
        .title("Environment Files")
        .description("Shell variable overrides (CFLAGS, FEATURES, CC, ...), applied to whichever atoms reference them below.")
        .build();
    let files = package_env::list_env_files().unwrap_or_default();
    if files.is_empty() {
        files_group.add(&adw::ActionRow::builder().title("None yet").subtitle("Add one below").build());
    }
    for file in &files {
        // An `ExpanderRow` rather than a plain row with a truncated
        // first-line subtitle — a one-line preview was hiding exactly
        // the part that actually mattered for a file with several
        // CFLAGS/CXXFLAGS/LDFLAGS-style lines (which is most of them).
        // Expanding shows the real, full content right there; "Edit"
        // still opens the actual text editor for changing it.
        let line_count = file.content.lines().count();
        let row = adw::ExpanderRow::builder()
            .title(&file.name)
            .subtitle(if line_count == 1 { "1 line".to_string() } else { format!("{line_count} lines") })
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("text-x-script-symbolic"));

        let content_label = gtk::Label::new(Some(if file.content.trim().is_empty() { "(empty)" } else { &file.content }));
        content_label.set_xalign(0.0);
        content_label.set_wrap(true);
        content_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        content_label.add_css_class("monospace");
        content_label.add_css_class("caption");
        content_label.set_selectable(true);
        content_label.set_margin_top(6);
        content_label.set_margin_bottom(10);
        content_label.set_margin_start(12);
        content_label.set_margin_end(12);
        row.add_row(&content_label);

        let edit_button = gtk::Button::from_icon_name("document-edit-symbolic");
        edit_button.add_css_class("flat");
        edit_button.set_valign(gtk::Align::Center);
        edit_button.set_tooltip_text(Some("Edit"));
        {
            let file = file.clone();
            let page = page.clone();
            let added_groups = added_groups.clone();
            edit_button.connect_clicked(move |button| {
                let Some(window) = button.root().and_downcast::<gtk::Window>() else { return };
                let page = page.clone();
                let added_groups = added_groups.clone();
                present_env_file_editor(&window, Some(file.clone()), Rc::new(move || rebuild_env_page(&page, &added_groups)));
            });
        }
        row.add_suffix(&edit_button);

        let remove_button = gtk::Button::from_icon_name("user-trash-symbolic");
        remove_button.add_css_class("flat");
        remove_button.set_valign(gtk::Align::Center);
        remove_button.set_tooltip_text(Some("Delete"));
        {
            let name = file.name.clone();
            let page = page.clone();
            let added_groups = added_groups.clone();
            remove_button.connect_clicked(move |button| {
                button.set_sensitive(false);
                if package_env::delete_env_file(&name).is_ok() {
                    rebuild_env_page(&page, &added_groups);
                }
            });
        }
        row.add_suffix(&remove_button);

        files_group.add(&row);
    }
    page.add(&files_group);
    new_groups.push(files_group);

    let new_name_row = adw::EntryRow::builder().title("File Name (e.g. no-lto.conf)").build();
    let add_file_button = gtk::Button::with_label("Create");
    add_file_button.add_css_class("suggested-action");
    add_file_button.add_css_class("pill");
    add_file_button.set_halign(gtk::Align::Center);
    let add_status = gtk::Label::new(None);
    add_status.add_css_class("dim-label");
    add_status.set_wrap(true);
    {
        let new_name_row = new_name_row.clone();
        let add_status = add_status.clone();
        let page = page.clone();
        let added_groups = added_groups.clone();
        add_file_button.connect_clicked(move |button| {
            let name = new_name_row.text().trim().to_string();
            if name.is_empty() || name.contains('/') {
                add_status.set_text("Give it a plain file name, no slashes.");
                return;
            }
            let Some(window) = button.root().and_downcast::<gtk::Window>() else { return };
            let page = page.clone();
            let added_groups = added_groups.clone();
            present_env_file_editor(
                &window,
                Some(package_env::EnvFile { name, content: String::new() }),
                Rc::new(move || rebuild_env_page(&page, &added_groups)),
            );
        });
    }
    let add_group = adw::PreferencesGroup::builder().title("Add Environment File").build();
    add_group.add(&new_name_row);
    add_group.add(&add_file_button);
    add_group.add(&add_status);
    page.add(&add_group);
    new_groups.push(add_group);

    // --- package.env associations ----------------------------------------
    let assoc_group = adw::PreferencesGroup::builder()
        .title("Package Associations")
        .description("Every atom -> env file mapping currently in effect, system-wide.")
        .build();
    let all_associations = package_env::read_all_associations().unwrap_or_default();
    let managed_associations = package_env::read_managed_associations().unwrap_or_default();
    if all_associations.is_empty() {
        assoc_group.add(&adw::ActionRow::builder().title("None yet").subtitle("Add one below").build());
    }
    for (atom, env_files) in &all_associations {
        let row = adw::ActionRow::builder().title(atom).subtitle(env_files.join(", ")).build();
        row.add_prefix(&gtk::Image::from_icon_name("emblem-symbolic-link-symbolic"));
        // Only this app's own managed entries are ever removable here —
        // a line from some hand-edited or other-tool file isn't this
        // app's place to delete.
        if let Some(managed_files) = managed_associations.get(atom) {
            for env_file in managed_files {
                let remove_button = gtk::Button::from_icon_name("user-trash-symbolic");
                remove_button.add_css_class("flat");
                remove_button.set_valign(gtk::Align::Center);
                remove_button.set_tooltip_text(Some(&format!("Remove {env_file}")));
                let atom = atom.clone();
                let env_file = env_file.clone();
                let page = page.clone();
                let added_groups = added_groups.clone();
                remove_button.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    if package_env::disassociate(&atom, &env_file).is_ok() {
                        rebuild_env_page(&page, &added_groups);
                    }
                });
                row.add_suffix(&remove_button);
            }
        } else {
            let badge = gtk::Label::new(Some("system"));
            badge.add_css_class("dim-label");
            badge.add_css_class("caption");
            row.add_suffix(&badge);
        }
        assoc_group.add(&row);
    }
    page.add(&assoc_group);
    new_groups.push(assoc_group);

    let atom_row = adw::EntryRow::builder().title("Atom (e.g. sys-devel/gcc)").build();
    let env_file_row = adw::ComboRow::builder().title("Env File").build();
    let env_file_names: Vec<String> = files.iter().map(|f| f.name.clone()).collect();
    let model = gtk::StringList::new(&env_file_names.iter().map(String::as_str).collect::<Vec<_>>());
    env_file_row.set_model(Some(&model));

    let add_assoc_button = gtk::Button::with_label("Associate");
    add_assoc_button.add_css_class("suggested-action");
    add_assoc_button.add_css_class("pill");
    add_assoc_button.set_halign(gtk::Align::Center);
    add_assoc_button.set_sensitive(!env_file_names.is_empty());
    let assoc_status = gtk::Label::new(if env_file_names.is_empty() { Some("Add an environment file above first.") } else { None });
    assoc_status.add_css_class("dim-label");
    assoc_status.set_wrap(true);
    {
        let atom_row = atom_row.clone();
        let env_file_row = env_file_row.clone();
        let env_file_names = env_file_names.clone();
        let assoc_status = assoc_status.clone();
        let page = page.clone();
        let added_groups = added_groups.clone();
        add_assoc_button.connect_clicked(move |_| {
            let atom = atom_row.text().trim().to_string();
            if atom.is_empty() || !atom.contains('/') {
                assoc_status.set_text("Give it a real category/name atom.");
                return;
            }
            let Some(env_file) = env_file_names.get(env_file_row.selected() as usize) else { return };
            match package_env::associate(&atom, env_file) {
                Ok(()) => rebuild_env_page(&page, &added_groups),
                Err(err) => assoc_status.set_text(&format!("Couldn't save: {err}")),
            }
        });
    }
    let add_assoc_group = adw::PreferencesGroup::builder().title("Add Association").build();
    add_assoc_group.add(&atom_row);
    add_assoc_group.add(&env_file_row);
    add_assoc_group.add(&add_assoc_button);
    add_assoc_group.add(&assoc_status);
    page.add(&add_assoc_group);
    new_groups.push(add_assoc_group);

    *added_groups.borrow_mut() = new_groups;
}

/// The env file content editor — a plain multi-line text buffer (these
/// files are shell fragments, not structured data) with Save/Cancel.
/// `file.content` empty means this is a brand new file (created on save,
/// not before) so cancelling out of a just-clicked "Create" leaves
/// nothing behind.
fn present_env_file_editor(window: &gtk::Window, file: Option<package_env::EnvFile>, on_saved: Rc<dyn Fn()>) {
    let Some(file) = file else { return };
    let dialog = adw::Dialog::builder().title(&file.name).content_width(560).content_height(480).build();

    let text_view = gtk::TextView::new();
    text_view.set_monospace(true);
    text_view.set_top_margin(8);
    text_view.set_bottom_margin(8);
    text_view.set_left_margin(8);
    text_view.set_right_margin(8);
    text_view.buffer().set_text(&file.content);

    let scroller = gtk::ScrolledWindow::builder().vexpand(true).child(&text_view).build();
    scroller.add_css_class("card");

    let hint = gtk::Label::new(Some("Shell variable assignments, one per line — e.g. CFLAGS=\"-O2 -pipe\""));
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint.set_xalign(0.0);
    hint.set_wrap(true);

    let save_button = gtk::Button::with_label("Save");
    save_button.add_css_class("suggested-action");
    save_button.add_css_class("pill");
    save_button.set_halign(gtk::Align::Center);

    let status = gtk::Label::new(None);
    status.add_css_class("dim-label");
    status.set_wrap(true);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 10);
    column.set_margin_top(12);
    column.set_margin_bottom(16);
    column.set_margin_start(12);
    column.set_margin_end(12);
    column.append(&hint);
    column.append(&scroller);
    column.append(&save_button);
    column.append(&status);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&column));
    dialog.set_child(Some(&toolbar));

    {
        let name = file.name.clone();
        let text_view = text_view.clone();
        let dialog_for_save = dialog.clone();
        save_button.connect_clicked(move |_| {
            let buffer = text_view.buffer();
            let content = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            match package_env::write_env_file(&name, &content) {
                Ok(()) => {
                    on_saved();
                    dialog_for_save.close();
                }
                Err(err) => status.set_text(&format!("Couldn't save: {err}")),
            }
        });
    }

    dialog.present(Some(window));
}
