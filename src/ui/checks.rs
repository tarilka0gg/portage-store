use super::*;

impl App {
    pub(super) fn check_updates(self: &Rc<Self>) {
        self.updates_stack.set_visible_child_name("checking");
        self.updates_generation.set(self.updates_generation.get() + 1);
        let generation = self.updates_generation.get();
        let lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let collect = lines.clone();
        let app = self.clone();
        runtime::spawn_job(
            emerge::pretend_world_job(self.settings.borrow().prefer_binary_packages),
            move |line| collect.borrow_mut().push(line),
            move |_success| {
                // An overtaken check (a newer one started after this one)
                // finishing late must not clobber the newer, more
                // accurate result with what could easily be stale
                // pre-update data.
                if generation != app.updates_generation.get() {
                    return;
                }
                let atoms = emerge::parse_update_atoms(&lines.borrow());
                let download_kib = emerge::parse_pretend_output(&lines.borrow()).download_kib;
                app.show_updates(atoms, download_kib);
            },
        );
    }

    /// Checks for unread Gentoo news (GLEP-42 items — profile migrations,
    /// dropped defaults, anything portage itself would print a "N news
    /// items need reading" reminder about) and reveals `news_banner` if
    /// there are any. Re-run on startup and on manual refresh; unlike
    /// updates, nothing else in this app changes what's unread, so there's
    /// no reason to also re-check after a job finishes.
    pub(super) fn check_news(self: &Rc<Self>) {
        let app = self.clone();
        runtime::spawn_blocking(
            portage_store::portage::news::list,
            move |result| {
                let items = result.unwrap_or_default();
                let unread = items.iter().filter(|i| i.unread).count();
                *app.news_items.borrow_mut() = items;
                if unread == 0 {
                    app.news_banner.set_revealed(false);
                    return;
                }
                app.news_banner.set_title(&if unread == 1 {
                    "1 Gentoo news item to read".to_string()
                } else {
                    format!("{unread} Gentoo news items to read")
                });
                app.news_banner.set_revealed(true);
            },
        );
    }

    /// Scans every `CONFIG_PROTECT` root for pending `._cfgNNNN_name`
    /// files — updates portage wrote beside a config file it wouldn't
    /// overwrite in place — and reveals `config_protect_banner` if there
    /// are any. Re-run on startup, manual refresh, and after any
    /// mutating job succeeds, since that's exactly when new ones show up.
    pub(super) fn check_config_protect(self: &Rc<Self>) {
        let app = self.clone();
        runtime::spawn_blocking(portage_store::portage::config_protect::scan, move |items| {
            let count = items.len();
            *app.config_protect_items.borrow_mut() = items;
            if count == 0 {
                app.config_protect_banner.set_revealed(false);
                return;
            }
            app.config_protect_banner.set_title(&if count == 1 {
                "1 config file needs review".to_string()
            } else {
                format!("{count} config files need review")
            });
            app.config_protect_banner.set_revealed(true);
        });
    }

    /// Checks `glsa-check` for security advisories affecting what's
    /// actually installed — a category no default Gentoo install surfaces
    /// anywhere without already knowing `app-portage/gentoolkit`'s
    /// `glsa-check` exists. Shown as its own section above the ordinary
    /// update list, plus a badge on the Updates tab itself so it's
    /// visible without opening the tab at all.
    pub(super) fn check_glsa(self: &Rc<Self>) {
        let app = self.clone();
        runtime::spawn_blocking(portage_store::portage::glsa::list_affected, move |result| {
            let entries = result.unwrap_or_default();

            while let Some(row) = app.security_list.first_child() {
                app.security_list.remove(&row);
            }

            if entries.is_empty() {
                app.security_section.set_visible(false);
                app.updates_view_page.set_badge_number(0);
                app.updates_view_page.set_needs_attention(false);
                return;
            }

            app.updates_view_page.set_badge_number(entries.len() as u32);
            app.updates_view_page.set_needs_attention(true);
            app.security_section.set_visible(true);

            for entry in entries {
                let row = adw::ActionRow::builder()
                    .title(format!("{} — {}", entry.id, entry.description))
                    .subtitle(entry.packages.join(", "))
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("security-high-symbolic"));

                let update_button = gtk::Button::with_label("Update");
                update_button.add_css_class("destructive-action");
                update_button.set_valign(gtk::Align::Center);
                let app_for_click = app.clone();
                let packages = entry.packages.clone();
                update_button.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    for atom in &packages {
                        app_for_click.enqueue(QueueEntry {
                            job: emerge::install_job(
                                atom,
                                app_for_click.settings.borrow().prefer_binary_packages,
                                app_for_click.settings.borrow().buildpkg_on_install,
                            ),
                            label: format!("Security update: {atom}"),
                            mutating: true,
                            retry_with_use_fix: true,
                            known_atoms: vec![atom.clone()],
                        });
                    }
                });
                row.add_suffix(&update_button);

                app.security_list.append(&row);
            }
        });
    }

    /// How stale the tree can get before "Updates" is answering from
    /// data old enough to be actively misleading — a week is a
    /// commonly-cited rule of thumb for how often Gentoo's own
    /// documentation suggests syncing, not an arbitrary number.
    pub(super) const STALE_SYNC_SECONDS: u64 = 7 * 24 * 60 * 60;

    pub(super) fn check_sync(self: &Rc<Self>) {
        let app = self.clone();
        runtime::spawn_blocking(portage_store::portage::sync::seconds_since_last_sync, move |age| {
            match age {
                Some(seconds) if seconds >= Self::STALE_SYNC_SECONDS => {
                    app.sync_banner.set_title(&format!(
                        "Package tree last synced {} — \"Updates\" may be out of date",
                        portage_store::portage::sync::format_age(seconds)
                    ));
                    app.sync_banner.set_revealed(true);
                }
                _ => app.sync_banner.set_revealed(false),
            }
        });
    }

    /// Runs `@preserved-rebuild`'s own pretend check (same call
    /// `health.rs`'s dashboard already makes on demand) and, if anything
    /// needs rebuilding, surfaces a toast with a one-click "Rebuild"
    /// action — unlike GLSA, this has no persistent badge home on any tab,
    /// so a toast is the whole surface for it. Called after every
    /// successful mutating job (see `start_next`'s `on_done`), since a
    /// merge that bumps a shared library's ABI/subslot is exactly what
    /// creates new preserved-rebuild candidates.
    pub(super) fn check_preserved_rebuild(self: &Rc<Self>) {
        let app = self.clone();
        let lines: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let collect = lines.clone();
        runtime::spawn_job(
            emerge::pretend_install_job("@preserved-rebuild", false),
            move |line| collect.borrow_mut().push(line),
            move |_success| {
                let preview = emerge::parse_pretend_output(&lines.borrow());
                if preview.packages_to_build == 0 {
                    return;
                }
                let toast = adw::Toast::new(&format!(
                    "{} package(s) need rebuilding against preserved libraries",
                    preview.packages_to_build
                ));
                toast.set_button_label(Some("Rebuild"));
                let app_for_action = app.clone();
                toast.connect_button_clicked(move |_| {
                    app_for_action.enqueue(QueueEntry {
                        job: emerge::install_job(
                            "@preserved-rebuild",
                            app_for_action.settings.borrow().prefer_binary_packages,
                            app_for_action.settings.borrow().buildpkg_on_install,
                        ),
                        label: "Rebuilding against preserved libraries".to_string(),
                        mutating: true,
                        retry_with_use_fix: true,
                        known_atoms: Vec::new(),
                    });
                });
                app.toasts.add_toast(toast);
            },
        );
    }

    /// The opt-in "collect trend history without needing to open the
    /// health dashboard" path — refreshes the same four visible signals
    /// (news banner, config banner, GLSA badge, orphan count) the
    /// dashboard's own checks already produce, and records one joint
    /// snapshot to `health_history` so "pending N days" has data to work
    /// from even for someone who never opens the dashboard itself.
    /// Read-only end to end, so unlike a build job there's no reason to
    /// gate this to `night_builds_only`'s off-hours window.
    pub(super) fn run_periodic_health_check(self: &Rc<Self>) {
        self.check_news();
        self.check_config_protect();
        self.check_glsa();
        self.check_sync();

        runtime::spawn_blocking(
            move || {
                let unread_news =
                    portage_store::portage::news::list().map(|items| items.iter().filter(|i| i.unread).count()).unwrap_or(0);
                let pending_config = portage_store::portage::config_protect::scan().len();
                let glsa_count = portage_store::portage::glsa::list_affected().map(|entries| entries.len()).unwrap_or(0);
                // Shelled out directly rather than through the job queue
                // (`depclean::pretend_job`) — this only needs a final
                // parsed count, not live streamed progress, so the
                // simpler synchronous call is enough.
                let orphan_count = std::process::Command::new("emerge")
                    .args(["--pretend", "--depclean"])
                    .output()
                    .ok()
                    .map(|output| {
                        let lines: Vec<String> =
                            String::from_utf8_lossy(&output.stdout).lines().map(String::from).collect();
                        if portage_store::portage::depclean::needs_update_first(&lines) {
                            None
                        } else {
                            Some(portage_store::portage::depclean::parse_candidates(&lines).len())
                        }
                    })
                    .unwrap_or(None);
                (unread_news, pending_config, glsa_count, orphan_count)
            },
            move |(unread_news, pending_config, glsa_count, orphan_count)| {
                // A `None` orphan count (couldn't measure it — needs a
                // full update first) skips recording entirely, same as
                // the dashboard's own joint-snapshot logic: 0 would
                // misreport an unmeasured state as a resolved one.
                if let Some(orphan_count) = orphan_count {
                    portage_store::portage::health_history::record(unread_news, pending_config, glsa_count, orphan_count);
                }
            },
        );
    }

    pub(super) fn show_updates(self: &Rc<Self>, atoms: Vec<String>, download_kib: Option<u64>) {
        while let Some(row) = self.updates_list.first_child() {
            self.updates_list.remove(&row);
        }
        // Kept around for "Update All" to hand off as `known_atoms` — the
        // one place in the app that already has the full pending-update
        // atom list on hand before the job that would need it even
        // starts, letting it show a real upfront ETA instead of none.
        *self.pending_update_atoms.borrow_mut() = atoms.clone();
        if atoms.is_empty() {
            self.updates_stack.set_visible_child_name("uptodate");
            return;
        }
        self.updates_subtitle
            .set_text(&format!("{} packages will be updated", atoms.len()));

        match download_kib.and_then(portage_store::portage::diskspace::low_space_warning) {
            Some(warning) => {
                self.updates_space_banner.set_title(&warning);
                self.updates_space_banner.set_revealed(true);
            }
            None => self.updates_space_banner.set_revealed(false),
        }
        for atom_with_version in atoms {
            let row = adw::ActionRow::builder().title(&atom_with_version).build();
            row.add_prefix(&gtk::Image::from_icon_name("software-update-available-symbolic"));

            // Lets one package be updated on its own instead of only via
            // "Update All" — useful when only one update is wanted right
            // now (e.g. everything else would pull in a long rebuild).
            let update_button = gtk::Button::with_label("Update");
            update_button.add_css_class("flat");
            update_button.set_valign(gtk::Align::Center);
            let bare_atom = strip_version_suffix(&atom_with_version);
            let app = self.clone();
            update_button.connect_clicked(move |button| {
                button.set_sensitive(false);
                app.enqueue(QueueEntry {
                    job: emerge::install_job(
                        &bare_atom,
                        app.settings.borrow().prefer_binary_packages,
                        app.settings.borrow().buildpkg_on_install,
                    ),
                    label: format!("Updating {bare_atom}"),
                    mutating: true,
                    retry_with_use_fix: true,
                    known_atoms: vec![bare_atom.clone()],
                });
            });
            row.add_suffix(&update_button);

            self.updates_list.append(&row);
        }
        self.updates_stack.set_visible_child_name("list");
    }
}
