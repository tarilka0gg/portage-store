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
    /// renders the rest as a collapsed section beneath the grid. Never
    /// touches `results_grid` itself — see `run_search` for why that
    /// matters.
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
        let boxed_list = gtk::ListBox::new();
        boxed_list.add_css_class("boxed-list");
        let expander = adw::ExpanderRow::builder()
            .title(format!("Also available via Flatpak ({})", merged.flatpak_only.len()))
            .build();
        for app in &merged.flatpak_only {
            let already_installed = installed.contains_key(&app.app_id);
            let row = widgets::flatpak_only_row(app, already_installed);
            if !already_installed {
                let app_for_click = app.clone();
                let this = self.clone();
                row.connect_activated(move |_| this.present_flatpak_detail(app_for_click.clone()));
            }
            expander.add_row(&row);
        }
        boxed_list.append(&expander);
        self.flatpak_section.append(&boxed_list);
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

    /// Opens a small confirm-and-install dialog for a Flatpak-only search
    /// hit. Deliberately not routed through `detail.rs` — that page is
    /// built entirely around Portage concepts (USE flags, `--pretend`
    /// previews, sandbox builds) that don't apply here, and forcing a
    /// Flatpak app through it would mean either a page half full of
    /// disabled Portage-only controls or a much larger rewrite of that
    /// page than this pass is scoped for.
    pub(super) fn present_flatpak_detail(self: &Rc<Self>, app: flatpak::FlatpakApp) {
        // Fetched before the dialog even opens — showing "Install" with
        // no size, then having a 1-2 GB runtime download turn out to be
        // part of it, is exactly the "your warning will lie" failure
        // mode a Flatpak-aware download size figure exists to avoid.
        let this = self.clone();
        let app_for_lookup = app.clone();
        runtime::spawn_blocking(
            move || flatpak::remote_info_size(&app_for_lookup.remote, &app_for_lookup.app_id).and_then(|(download, _)| download),
            move |download_kib| this.present_flatpak_confirm(app.clone(), download_kib),
        );
    }

    pub(super) fn present_flatpak_confirm(self: &Rc<Self>, app: flatpak::FlatpakApp, download_kib: Option<u64>) {
        // Driven by `Caps`, not hardcoded prose about Flatpak specifically
        // — this is exactly the sentence that would need to change (or
        // vanish) if a third, root-needing backend ever reused this same
        // confirm dialog.
        let caps = backend::SourceId::Flatpak.caps();
        let trust_line = if caps.sandboxed && !caps.needs_root {
            "Installs sandboxed, as your own user — no admin password needed."
        } else {
            "Installs on this system."
        };
        let mut body = if app.description.is_empty() { trust_line.to_string() } else { format!("{}\n\n{trust_line}", app.description) };
        if let Some(kib) = download_kib {
            body.push_str(&format!("\n\nDownload size: {} (may include a shared runtime not yet on this machine).", emerge::format_size_kib(kib)));
        }
        let dialog = adw::AlertDialog::new(Some(&app.name), Some(&body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("install", "Install");
        dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("install"));
        dialog.set_close_response("cancel");

        let this = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "install" {
                return;
            }
            let this = this.clone();
            let app = app.clone();
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
        });
        dialog.present(Some(&self.window));
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
