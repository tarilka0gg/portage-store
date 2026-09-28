use super::*;

impl App {
    pub(super) fn clear_flatpak_section(&self) {
        while let Some(child) = self.flatpak_section.first_child() {
            self.flatpak_section.remove(&child);
        }
        self.flatpak_chips.borrow_mut().clear();
    }

    /// Reconciles a Flatpak search's hits against the Portage results
    /// already on screen (`backend::merge_search_results`): pins a
    /// "Also on Flatpak" chip onto every card with a confident match, and
    /// renders the rest as a card grid beneath the Portage results — the
    /// same `package_card`/`grid()` treatment those get, via
    /// `widgets::flatpak_card`, not a visually separate secondary list.
    /// Never touches `results_grid` itself — see `run_search` for why
    /// that matters.
    pub(super) fn apply_flatpak_results(self: &Rc<Self>, hits: Vec<flatpak::FlatpakApp>, installed: HashMap<String, flatpak::InstalledFlatpak>) {
        let merged = backend::merge_search_results(&self.last_results.borrow(), &hits);
        let cards = self.search_cards.borrow();
        for atom in merged.chips.keys() {
            if let Some(card) = cards.get(atom) {
                widgets::add_flatpak_chip(card);
            }
        }
        drop(cards);
        *self.flatpak_chips.borrow_mut() = merged.chips;

        self.clear_flatpak_section_only_content();
        if merged.flatpak_only.is_empty() {
            return;
        }
        let heading = widgets::section_heading(&format!("Also available via Flatpak ({})", merged.flatpak_only.len()));
        let grid = widgets::grid();
        for app in &merged.flatpak_only {
            let already_installed = installed.contains_key(&app.app_id);
            let card = widgets::flatpak_card(app, already_installed);
            if !already_installed {
                let app_for_click = app.clone();
                let this = self.clone();
                card.connect_clicked(move |_| this.present_flatpak_detail(app_for_click.clone()));
            }
            grid.insert(&card, -1);
        }
        self.flatpak_section.append(&heading);
        self.flatpak_section.append(&grid);
    }

    /// Like `clear_flatpak_section`, but leaves `flatpak_chips` alone —
    /// `apply_flatpak_results` just repopulated it and clears the section
    /// widget itself right after, so wiping the map it only just set
    /// would be self-defeating.
    pub(super) fn clear_flatpak_section_only_content(&self) {
        while let Some(child) = self.flatpak_section.first_child() {
            self.flatpak_section.remove(&child);
        }
    }

    /// Opens a real navigation page for a Flatpak-only search hit — the
    /// same "push a page" treatment `browse::open_detail` gives a Portage
    /// package, via `flatpak_detail::build`. That page is deliberately
    /// much smaller than `detail.rs`'s (no USE flags, no `--pretend`
    /// preview, no sandbox builds — none of that applies to a Flatpak
    /// app), not a cut-down copy of it.
    pub(super) fn present_flatpak_detail(self: &Rc<Self>, app: flatpak::FlatpakApp) {
        // Pushed immediately so there's something to look at while the
        // lookups below are in flight — same pattern `browse::open_detail`
        // uses while `eix::lookup` runs.
        let loading_page = loading_navigation_page();
        self.nav.push(&loading_page);

        let this = self.clone();
        let app_for_lookup = app.clone();
        runtime::spawn_blocking(
            move || {
                let download_kib = flatpak::remote_info_size(&app_for_lookup.remote, &app_for_lookup.app_id).and_then(|(download, _)| download);
                let flathub = portage_store::portage::flathub::lookup_by_app_id(&app_for_lookup.app_id);
                (download_kib, flathub)
            },
            move |(download_kib, flathub)| {
                let install_app = this.clone();
                let page = flatpak_detail::build(
                    &app,
                    download_kib,
                    flathub,
                    Rc::new(move |app: flatpak::FlatpakApp| install_app.install_flatpak(app)),
                );
                this.nav.pop();
                this.nav.push(&page);
            },
        );
    }

    /// Sets up `flathub` (if it isn't already) and queues the actual
    /// install — the click handler behind the detail page's Install
    /// button, previously the response handler on the confirm dialog this
    /// page replaced.
    fn install_flatpak(self: &Rc<Self>, app: flatpak::FlatpakApp) {
        let this = self.clone();
        runtime::spawn_blocking(
            move || flatpak::ensure_user_flathub().map(|()| app).map_err(|e| e.to_string()),
            move |result| match result {
                Ok(app) => this.enqueue_flatpak(FlatpakQueueEntry {
                    job: flatpak::install_job(&app.remote, &app.app_id),
                    label: format!("Installing {} (Flatpak)", app.name),
                }),
                Err(err) => this.toast(&format!("Couldn't set up Flatpak: {err}")),
            },
        );
    }

    pub(super) fn enqueue_flatpak(self: &Rc<Self>, entry: FlatpakQueueEntry) {
        let label = entry.label.clone();
        self.flatpak_queue.borrow_mut().push_back(entry);
        if self.flatpak_running.get() {
            let pending = self.flatpak_queue.borrow().len();
            self.toast(&format!("{label} — queued ({pending})"));
        } else {
            self.start_next_flatpak();
        }
    }

    /// Flatpak's own lock domain — see `flatpak_queue` on `App`. Mirrors
    /// `start_next` in shape but with none of the Portage-specific
    /// machinery (no ETA, no USE-flag retry, no resource throttling —
    /// none of it applies to a rootless, sandboxed, already-prebuilt
    /// install) and can run at the same time as a Portage job is going.
    pub(super) fn start_next_flatpak(self: &Rc<Self>) {
        let Some(entry) = self.flatpak_queue.borrow_mut().pop_front() else {
            self.flatpak_running.set(false);
            self.sync_inhibit();
            self.flatpak_job_revealer.set_reveal_child(false);
            return;
        };

        self.flatpak_running.set(true);
        self.sync_inhibit();
        self.flatpak_job_label.set_text(&entry.label);
        self.flatpak_job_progress.set_fraction(0.0);
        self.flatpak_job_progress.set_text(None);
        self.flatpak_job_revealer.set_reveal_child(true);
        self.clear_job_log("flatpak");

        let done_app = self.clone();
        let label = entry.label;
        let progress_bar = self.flatpak_job_progress.clone();
        let log_app = self.clone();
        runtime::spawn_job(
            entry.job,
            move |line| {
                if let Some(progress) = flatpak::parse_progress(&line) {
                    progress_bar.set_fraction(progress.percent as f64 / 100.0);
                    progress_bar.set_text(Some(&format!("{}%", progress.percent)));
                }
                log_app.append_job_log("flatpak", &line);
            },
            move |success| {
                done_app.send_notification(&label, success);
                done_app.toast(&if success { format!("{label} — done") } else { format!("{label} — failed") });
                done_app.start_next_flatpak();
            },
        );
    }

}
