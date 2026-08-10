use super::*;

impl App {
    /// Whatever's worth knowing before a big, multi-package operation
    /// actually starts — each check is independently best-effort (a check
    /// that couldn't run just contributes nothing, never blocks the
    /// others) and this never stops the update on its own; it only feeds
    /// `update_all`'s confirmation dialog, which the update proceeds past
    /// on "Continue" regardless of how many warnings fired.
    pub(super) fn preflight_warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        if let Some(toolchain_atom) = emerge::atoms_touch_toolchain(&self.pending_update_atoms.borrow()) {
            let jobs = portage_store::portage::resource_limits::recommended_jobs();
            let headroom_kib = portage_store::portage::resource_limits::available_ram_kib();
            if headroom_kib.is_some_and(|kib| kib / u64::from(jobs) < portage_store::portage::resource_limits::RAM_PER_JOB_KIB) {
                warnings.push(format!(
                    "This update rebuilds {toolchain_atom}, which commonly cascades into rebuilding \
                     everything linked against it — and free RAM looks tight for that at the current \
                     job count. An unthrottled toolchain rebuild is exactly what's exhausted RAM here before."
                ));
            }
        }

        if let Some(seconds) = portage_store::portage::sync::seconds_since_last_sync()
            && seconds >= Self::STALE_SYNC_SECONDS
        {
            warnings.push(format!(
                "The package tree was last synced {} — this update's dependency resolution may be working from stale data.",
                portage_store::portage::sync::format_age(seconds)
            ));
        }

        let pending_config = self.config_protect_items.borrow().len();
        if pending_config > 0 {
            warnings.push(format!(
                "{pending_config} pending config update(s) haven't been reviewed yet — \
                 this update may add more on top before the earlier ones are resolved."
            ));
        }

        warnings
    }

    /// The actual "start the `@world` update" action — split out from
    /// `update_all`'s click handler so it can fire either immediately (no
    /// preflight warnings) or after the user clicks through the
    /// warning dialog.
    pub(super) fn start_update_all(self: &Rc<Self>) {
        // Best-effort — a known-good bookmark right before the one
        // operation most likely to touch a lot of files at once, so the
        // history browser (`preferences.rs`) has an obvious point to
        // revert back to as a group rather than one commit at a time.
        // Never blocks the actual update over a tagging failure (a
        // missing/unwritable git repo, etc.).
        runtime::spawn_blocking(|| portage_store::portage::config_history::tag_before("update-world"), |_| {});
        self.enqueue(QueueEntry {
            job: emerge::update_world_job(self.settings.borrow().prefer_binary_packages),
            label: "System update (@world)".to_string(),
            mutating: true,
            // A `--deep --newuse @world` update is at least as likely to
            // hit a required-USE/keyword/license mismatch or a circular
            // dependency as a single-package install — there was no
            // reason this was off here specifically, and leaving it off
            // meant the same auto-fix dialog a single install already
            // gets never showed up for the one job most likely to
            // actually need it.
            retry_with_use_fix: true,
            known_atoms: self.pending_update_atoms.borrow().clone(),
        });
    }

    /// The opt-in "collect trend history without needing to open the
    /// health dashboard" path — refreshes the same four visible signals
    /// (news banner, config banner, GLSA badge, orphan count) the
    /// dashboard's own checks already produce, and records one joint
    /// snapshot to `health_history` so "pending N days" has data to work
    /// from even for someone who never opens the dashboard itself.
    /// Read-only end to end, so unlike a build job there's no reason to
    /// gate this to `night_builds_only`'s off-hours window.

    pub(super) fn install(self: &Rc<Self>, atom: String) {
        self.enqueue(QueueEntry {
            job: emerge::install_job(
                &atom,
                self.settings.borrow().prefer_binary_packages,
                self.settings.borrow().buildpkg_on_install,
            ),
            label: format!("Installing {atom}"),
            mutating: true,
            retry_with_use_fix: true,
            known_atoms: vec![atom],
        });
    }

    pub(super) fn uninstall(self: &Rc<Self>, atom: String) {
        self.enqueue(QueueEntry {
            job: emerge::uninstall_job(&atom),
            label: format!("Removing {atom}"),
            mutating: true,
            retry_with_use_fix: false,
            // No `qlop` timing data exists for an uninstall — its average-
            // merge parser explicitly skips unmerge lines — so there's
            // nothing an ETA lookup here could ever find.
            known_atoms: Vec::new(),
        });
    }

    /// Queues a build of `atom` in the isolated sandbox chroot (see
    /// `portage::sandbox`) rather than the live system — offered from the
    /// detail page only once a normal `--pretend` has already failed.
    /// `mutating: false`: unlike a real install, this never touches what's
    /// actually installed on the host, so there's nothing for a rescan
    /// afterwards to pick up.
    pub(super) fn sandbox_build(self: &Rc<Self>, atom: String) {
        match portage_store::portage::sandbox::build_job(&atom) {
            Ok(job) => self.enqueue(QueueEntry {
                job,
                label: format!("Sandbox build: {atom}"),
                mutating: false,
                retry_with_use_fix: false,
                // A reasonable ETA proxy even though the build happens in
                // a chroot with its own separate emerge.log: the host's
                // own historical average for this atom (if any) is still
                // roughly the same compile work on the same hardware.
                known_atoms: vec![atom],
            }),
            Err(err) => self.toast(&format!("Couldn't start sandbox build: {err}")),
        }
    }

    /// Reinstalls `atom` at `version` from the local binpkg cache — see
    /// `binpkg::downgrade_job`. `mutating: true` like a normal install:
    /// this does change what's installed, just to an older version.
    pub(super) fn downgrade(self: &Rc<Self>, atom: String, version: String) {
        self.enqueue(QueueEntry {
            job: portage_store::portage::binpkg::downgrade_job(&atom, &version),
            label: format!("Reinstalling {atom}-{version} from cache"),
            mutating: true,
            retry_with_use_fix: false,
            known_atoms: Vec::new(),
        });
    }

    /// Removes an outright-wasted queued job — it never got to run at
    /// all, so unlike cancelling something in progress there's nothing
    /// destructive here to warn about.
    pub(super) fn cancel_queued(self: &Rc<Self>, index: usize) {
        let removed = self.queue.borrow_mut().remove(index);
        if let Some(entry) = removed {
            self.toast(&format!("{} — removed from queue", entry.label));
        }
    }

    /// Moves a queued job to the very front — the "an urgent single
    /// install shouldn't have to wait behind a multi-hour `@world`
    /// update" case. Never touches whatever's currently running; the
    /// bumped job simply becomes the *next* one `start_next` picks up.
    pub(super) fn prioritize_queued(self: &Rc<Self>, index: usize) {
        let mut queue = self.queue.borrow_mut();
        if let Some(entry) = queue.remove(index) {
            queue.push_front(entry);
        }
    }

    /// Nudges a queued job one place earlier — swaps it with whatever's
    /// immediately ahead of it. Distinct from `prioritize_queued`'s "jump
    /// to the very front": for a queue with more than two entries, "move
    /// this one ahead of just the next thing" and "skip the whole line"
    /// are different asks, and only the first one was previously
    /// possible here.
    pub(super) fn nudge_queued(self: &Rc<Self>, index: usize) {
        if index == 0 {
            return;
        }
        self.queue.borrow_mut().swap(index, index - 1);
    }

    /// Shows what's waiting behind the current job — the queue is
    /// otherwise invisible beyond a toast's passing "queued (N)" message
    /// at the moment something's added to it.
    pub(super) fn present_queue_popover(self: &Rc<Self>, anchor: &gtk::Button) {
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);

        let popover = gtk::Popover::new();

        let queue = self.queue.borrow();
        if queue.is_empty() {
            list.append(&adw::ActionRow::builder().title("Nothing queued").build());
        }
        for (index, entry) in queue.iter().enumerate() {
            let row = adw::ActionRow::builder().title(&entry.label).build();

            if index > 0 {
                let nudge = gtk::Button::from_icon_name("go-up-symbolic");
                nudge.add_css_class("flat");
                nudge.set_valign(gtk::Align::Center);
                nudge.set_tooltip_text(Some("Move up one"));
                let app = self.clone();
                let popover_for_nudge = popover.clone();
                nudge.connect_clicked(move |_| {
                    app.nudge_queued(index);
                    popover_for_nudge.popdown();
                });
                row.add_suffix(&nudge);

                let bump = gtk::Button::from_icon_name("go-top-symbolic");
                bump.add_css_class("flat");
                bump.set_valign(gtk::Align::Center);
                bump.set_tooltip_text(Some("Move to front"));
                let app = self.clone();
                let popover_for_bump = popover.clone();
                bump.connect_clicked(move |_| {
                    app.prioritize_queued(index);
                    popover_for_bump.popdown();
                });
                row.add_suffix(&bump);
            }

            let cancel = gtk::Button::from_icon_name("edit-delete-symbolic");
            cancel.add_css_class("flat");
            cancel.set_valign(gtk::Align::Center);
            cancel.set_tooltip_text(Some("Remove from queue"));
            let app = self.clone();
            let popover_for_cancel = popover.clone();
            cancel.connect_clicked(move |_| {
                app.cancel_queued(index);
                popover_for_cancel.popdown();
            });
            row.add_suffix(&cancel);

            list.append(&row);
        }
        drop(queue);

        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
        column.set_margin_top(8);
        column.set_margin_bottom(8);
        column.set_margin_start(8);
        column.set_margin_end(8);
        column.set_size_request(280, -1);
        column.append(&list);

        // Renders the exact commands the queue would actually run —
        // useful for running something headless over SSH, or just
        // double-checking what the GUI is about to do before clicking
        // through a polkit prompt. Only worth offering when there's
        // something to export.
        if !self.queue.borrow().is_empty() || !self.flatpak_queue.borrow().is_empty() {
            let export_button = gtk::Button::with_label("Export Queue as Script…");
            export_button.add_css_class("flat");
            export_button.set_margin_top(4);
            let app_for_export = self.clone();
            let popover_for_export = popover.clone();
            export_button.connect_clicked(move |_| {
                popover_for_export.popdown();
                app_for_export.export_queue_script();
            });
            column.append(&export_button);
        }

        popover.set_child(Some(&column));
        popover.set_parent(anchor);
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    }

    /// Builds a POSIX shell script covering every job currently queued in
    /// either lane, in run order, and offers it as a save file. Purely a
    /// snapshot of what's queued *right now* — a job that starts running
    /// before the save dialog closes isn't un-queued from the script,
    /// since it genuinely was part of the queue when this was asked for.
    pub(super) fn export_queue_script(self: &Rc<Self>) {
        let mut script = String::from("#!/bin/sh\nset -e\n\n");
        for entry in self.queue.borrow().iter() {
            script.push_str(&format!("# {}\n{}\n\n", entry.label, entry.job.to_shell_command()));
        }
        for entry in self.flatpak_queue.borrow().iter() {
            script.push_str(&format!("# {}\n{}\n\n", entry.label, entry.job.to_shell_command()));
        }

        let file_dialog = gtk::FileDialog::builder().title("Export Queue as Script").initial_name("portage-store-queue.sh").build();
        let app = self.clone();
        file_dialog.save(Some(&self.window), gtk::gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let Some(path) = file.path() else { return };
            let script = script.clone();
            let app = app.clone();
            runtime::spawn_blocking(
                move || {
                    std::fs::write(&path, &script)?;
                    // Best-effort — a script you can just double-click or
                    // `./run` beats one that needs a `chmod +x` first,
                    // but a filesystem that doesn't support the bit
                    // (e.g. some network mounts) shouldn't fail the
                    // whole export over it.
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(metadata) = std::fs::metadata(&path) {
                            let mut perms = metadata.permissions();
                            perms.set_mode(perms.mode() | 0o111);
                            let _ = std::fs::set_permissions(&path, perms);
                        }
                    }
                    Ok::<(), std::io::Error>(())
                },
                move |result| match result {
                    Ok(()) => app.toast("Queue exported"),
                    Err(err) => app.toast(&format!("Couldn't export queue: {err}")),
                },
            );
        });
    }

    /// Offered after a `--keep-going` `@world` update finishes with some
    /// (not all) packages failed — the whole point of `--keep-going` is
    /// that this doesn't have to mean redoing the entire update, just
    /// the part that actually broke.
    pub(super) fn present_keep_going_retry(self: &Rc<Self>, label: &str, failed_atoms: Vec<String>, total: usize) {
        let title = if failed_atoms.len() == 1 {
            "1 package failed to update".to_string()
        } else {
            format!("{} of {total} packages failed to update", failed_atoms.len())
        };
        let body = format!(
            "{label} kept going past the failure(s) below and merged everything else successfully:\n\n{}\n\n\
             Retry just these, or leave them for later?",
            failed_atoms.join("\n")
        );
        let dialog = adw::AlertDialog::new(Some(&title), Some(&body));
        dialog.add_response("dismiss", "Not Now");
        dialog.add_response("retry", "Retry Failed Only");
        dialog.set_response_appearance("retry", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("retry"));
        dialog.set_close_response("dismiss");

        let app = self.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "retry" {
                return;
            }
            let getbinpkg = app.settings.borrow().prefer_binary_packages;
            let buildpkg = app.settings.borrow().buildpkg_on_install;
            app.enqueue(QueueEntry {
                job: emerge::install_many_job(&failed_atoms, getbinpkg, buildpkg),
                label: "Retrying failed packages".to_string(),
                mutating: true,
                retry_with_use_fix: true,
                known_atoms: failed_atoms.clone(),
            });
        });
        dialog.present(Some(&self.window));
    }

    /// Sums `qlop` averages across every `known_atoms` entry still waiting
    /// behind the current job — separate from `job_eta`'s live per-job
    /// countdown, this is "how long is the whole backlog" for someone
    /// about to walk away from a queue with several jobs stacked up, not
    /// just the one at the front. Best-effort and silent about anything it
    /// can't estimate: hidden entirely if nothing queued has any known
    /// atoms or build history, same as `job_eta`.
    pub(super) fn update_queue_eta(self: &Rc<Self>) {
        let atoms: Vec<String> = self.queue.borrow().iter().flat_map(|e| e.known_atoms.iter().cloned()).collect();
        let job_queue_eta = self.job_queue_eta.clone();
        if atoms.is_empty() {
            job_queue_eta.set_visible(false);
            return;
        }
        let job_count = self.queue.borrow().len();
        runtime::spawn_blocking(
            move || portage_store::portage::qlop::average_merge_seconds_batch(&atoms),
            move |averages| {
                if averages.is_empty() {
                    job_queue_eta.set_visible(false);
                    return;
                }
                let total: u64 = averages.values().map(|(secs, _)| secs).sum();
                let estimate = portage_store::portage::build_time::format_duration(total);
                let plural = if job_count == 1 { "job" } else { "jobs" };
                job_queue_eta.set_text(&format!("Queue: ~{estimate} remaining ({job_count} {plural} behind this one)"));
                job_queue_eta.set_visible(true);
            },
        );
    }

    pub(super) fn enqueue(self: &Rc<Self>, entry: QueueEntry) {
        let label = entry.label.clone();
        self.queue.borrow_mut().push_back(entry);
        self.update_queue_eta();
        if self.running.get() {
            let pending = self.queue.borrow().len();
            self.toast(&format!("{label} — queued ({pending})"));
        } else {
            self.start_next();
        }
    }

    pub(super) fn start_next(self: &Rc<Self>) {
        let Some(mut entry) = self.queue.borrow_mut().pop_front() else {
            self.running.set(false);
            self.sync_inhibit();
            self.job_revealer.set_reveal_child(false);
            self.job_queue_eta.set_visible(false);
            self.job_run_now_button.set_visible(false);
            return;
        };
        self.update_queue_eta();

        // "Collect at night" — a mutating job (an actual build, not a
        // pretend/preview run) waits for the configured off-hours window
        // instead of starting immediately, so a long queue can be left to
        // run unattended overnight without competing with the machine
        // during the day. Deferred jobs stay at the front of the queue
        // and get rechecked periodically rather than blocking anything
        // else — an urgent non-mutating check can still run in the
        // meantime. `force_run_next` (see the "Build Now" button) is a
        // one-shot override consumed right here via `take()`.
        if entry.mutating && self.settings.borrow().night_builds_only && !in_night_window() && !self.force_run_next.take() {
            self.queue.borrow_mut().push_front(entry);
            self.running.set(false);
            self.sync_inhibit();
            self.job_label.set_text("Waiting for night hours to build…");
            self.job_progress.set_visible(false);
            self.job_eta.set_visible(false);
            self.job_log.set_text("");
            self.job_run_now_button.set_visible(true);
            self.job_revealer.set_reveal_child(true);
            let app = self.clone();
            gtk::glib::timeout_add_seconds_local(300, move || {
                if !app.running.get() {
                    app.start_next();
                }
                gtk::glib::ControlFlow::Break
            });
            return;
        }
        self.job_run_now_button.set_visible(false);

        // Resource-throttled by default (see `resource_limits::throttled`)
        // for anything that actually builds — a queued job left running
        // for hours shouldn't be the reason something else on the machine
        // starves for CPU/IO, and an unthrottled `MAKEOPTS` can exhaust
        // RAM outright on a job with enough packages to build back to
        // back.
        if entry.mutating && self.settings.borrow().throttle_builds {
            entry.job = portage_store::portage::resource_limits::throttled(entry.job);
        }

        self.running.set(true);
        self.sync_inhibit();
        self.job_label.set_text(&entry.label);
        self.job_log.set_text("");
        self.clear_job_log("portage");
        self.job_eta.set_visible(false);
        // Shared with the `on_line` closure below, so the estimate can
        // count down live as each known atom finishes instead of just
        // showing one static number computed at job start. `averages`
        // holds the per-atom lookup once the async `qlop` call returns;
        // `remaining` is the running total it's initialized from and then
        // decremented against; `last_atom` is how a transition to a new
        // atom in the `(N of M)` progress lines is detected.
        let job_eta_averages: Rc<RefCell<HashMap<String, u64>>> = Rc::new(RefCell::new(HashMap::new()));
        let job_eta_remaining: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        let job_eta_last_atom: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        if !entry.known_atoms.is_empty() {
            let job_eta = self.job_eta.clone();
            let atoms = entry.known_atoms.clone();
            let total_atoms = atoms.len();
            let job_eta_averages = job_eta_averages.clone();
            let job_eta_remaining = job_eta_remaining.clone();
            runtime::spawn_blocking(
                move || portage_store::portage::qlop::average_merge_seconds_batch(&atoms),
                move |averages| {
                    let known = averages.len();
                    if known == 0 {
                        return;
                    }
                    let total: u64 = averages.values().map(|(secs, _)| secs).sum();
                    job_eta_remaining.set(total);
                    *job_eta_averages.borrow_mut() = averages.into_iter().map(|(atom, (secs, _))| (atom, secs)).collect();
                    let estimate = portage_store::portage::build_time::format_duration(total);
                    job_eta.set_text(&if known < total_atoms {
                        format!("Estimated ≥{estimate} ({known} of {total_atoms} packages have build history)")
                    } else {
                        format!("Estimated ~{estimate}")
                    });
                    job_eta.set_visible(true);
                },
            );
        }
        self.job_progress.set_fraction(0.0);
        self.job_progress.set_text(None);
        // Visible and pulsing from the moment the job starts, not just once
        // a "Jobs: N of M" line shows up — for a job with a lot of
        // resolving to do up front (an `@world` update easily takes a
        // while before touching its first package), that line can be
        // long enough coming that the bottom bar looked like an inert
        // label with no indication anything was actually running.
        self.job_progress.set_visible(true);
        self.job_progress.pulse();
        self.job_revealer.set_reveal_child(true);

        let log_app = self.clone();
        let done_app = self.clone();
        let label = entry.label;
        let mutating = entry.mutating;
        let retry_with_use_fix = entry.retry_with_use_fix;
        let job_for_retry = entry.job.clone();
        // Install/uninstall jobs' last arg is always the atom (see
        // `install_job`/`uninstall_job`) — used to find this exact
        // package's own detail page, if it happens to be the one open
        // right now, so it can show this job's progress under its own
        // Install/Remove button instead of only in the bottom bar.
        let job_atom = entry.job.args.last().cloned();
        // `--depclean` only ever appears in `uninstall_job`'s args — used
        // to tell `refresh_detail_action_button` below which way to flip
        // once this job succeeds.
        let job_is_install = !entry.job.args.iter().any(|a| a == "--depclean");
        // Whether `--keep-going` even applies here — only `update_world_job`
        // carries it, and only a job with a real, multi-package atom list
        // up front (i.e. an `@world` update) makes "retry just what
        // failed" a meaningfully different offer from "retry the whole
        // job" a single-package install already gets via the USE-fix
        // retry path.
        let known_atom_count = entry.known_atoms.len();
        // Carried through to the USE-fix retry requeue below, so a retried
        // job keeps whatever ETA data the original enqueue already had
        // instead of silently losing it.
        let known_atoms_for_retry = entry.known_atoms.clone();

        // Real progress (a "Jobs: N of M" line) only starts appearing once
        // portage is actually building/merging — dependency resolution and
        // downloading beforehand report nothing to size a determinate bar
        // against. Pulses (GTK's own bar animates a block sliding left to
        // right, `bar.pulse()` on a timer) fill that stretch instead of the
        // bar just sitting empty; `pulsing` flips false the moment real
        // progress arrives (below) or the job ends, which is what stops
        // the timer.
        let pulsing = Rc::new(Cell::new(true));
        if let Some(atom) = &job_atom
            && let Some(page) = visible_detail_page(&self.nav, atom) {
                if let Some(button) = detail_action_button(&page) {
                    button.add_css_class("detail-action-pulsing");
                }
                if let Some(bar) = detail_progress_bar(&page) {
                    bar.set_visible(true);
                    bar.pulse();
                    let pulsing_for_timer = pulsing.clone();
                    gtk::glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
                        if !pulsing_for_timer.get() {
                            return gtk::glib::ControlFlow::Break;
                        }
                        bar.pulse();
                        gtk::glib::ControlFlow::Continue
                    });
                }
            }

        // The bottom bar's own progress indicator, independent of whichever
        // (if any) detail page is open — an `@world` update has no single
        // package's page to show progress under, so this is the only
        // animation it ever gets.
        let bottom_bar = self.job_progress.clone();
        let pulsing_for_bottom_timer = pulsing.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
            if !pulsing_for_bottom_timer.get() {
                return gtk::glib::ControlFlow::Break;
            }
            bottom_bar.pulse();
            gtk::glib::ControlFlow::Continue
        });

        // Collected alongside the log label's running "latest line" above
        // so a failure can be inspected for a fixable cause afterwards —
        // the label only ever shows the newest line, not the whole run.
        let output: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let output_for_line = output.clone();
        let pulsing_for_done = pulsing.clone();
        let job_eta_label = self.job_eta.clone();
        runtime::spawn_job(
            entry.job,
            move |line| {
                if let Some(step) = emerge::parse_step_progress(&line) {
                    // The bar itself deliberately keeps pulsing instead of
                    // switching to this as a determinate fraction: "Jobs: N
                    // of M" only advances between *whole packages*
                    // finishing, not during one's own (often multi-minute)
                    // build — showing that fraction would leave the bar
                    // looking frozen for most of the job instead of
                    // reading as "still working". The count (and, once
                    // known, which package) is still worth showing, so it
                    // goes in the bar's own text overlay instead, updated
                    // without disturbing the pulse.
                    let text = match &step.atom {
                        // `::gentoo`/`::guru`/etc. suffix trimmed — which
                        // repo an atom came from isn't part of "what's
                        // building right now" at a glance.
                        Some(atom) => format!("{} / {}: {}", step.done, step.total, atom.split("::").next().unwrap_or(atom)),
                        None => format!("{} / {}", step.done, step.total),
                    };
                    log_app.job_progress.set_text(Some(&text));

                    // Live ETA: when the atom actually being emerged
                    // changes, the *previous* one just finished — subtract
                    // its known average from the running remaining total
                    // and re-render, so the estimate actually counts down
                    // over the course of the job instead of sitting at
                    // whatever it said at the start. Packages outside
                    // `known_atoms` (transitive deps pulled in alongside an
                    // explicit install) simply have no average on hand and
                    // don't move the estimate — an approximation, not a
                    // regression, since there was no timing signal for
                    // them before this either.
                    if let Some(atom) = &step.atom {
                        let atom = atom.split("::").next().unwrap_or(atom).to_string();
                        let mut last = job_eta_last_atom.borrow_mut();
                        if last.as_deref() != Some(atom.as_str())
                            && let Some(finished) = last.replace(atom)
                        {
                            let secs = job_eta_averages.borrow().get(&finished).copied();
                            if let Some(secs) = secs {
                                let remaining = job_eta_remaining.get().saturating_sub(secs);
                                job_eta_remaining.set(remaining);
                                if remaining > 0 {
                                    job_eta_label.set_text(&format!(
                                        "~{} remaining",
                                        portage_store::portage::build_time::format_duration(remaining)
                                    ));
                                }
                            }
                        }
                    }
                }
                log_app.job_log.set_text(&line);
                log_app.append_job_log("portage", &line);
                output_for_line.borrow_mut().push(line);
            },
            move |success| {
                pulsing_for_done.set(false);
                if !success && retry_with_use_fix
                    && let Some(relaxation) = PendingRelaxation::detect(&output.borrow()) {
                        let done_app = done_app.clone();
                        let job_for_retry = job_for_retry.clone();
                        let label_for_retry = label.clone();
                        let known_atoms_for_retry = known_atoms_for_retry.clone();

                        // Shown before touching anything: this is a
                        // dependency's own relaxation, not something the
                        // user directly asked for, and applying the wrong
                        // one (an EULA accepted site-wide, say) is worth a
                        // look before it happens rather than an automatic
                        // silent fix.
                        let mut body = format!("{label_for_retry} {}\n\n", relaxation.intro());
                        for line in relaxation.body_lines() {
                            body.push_str(&line);
                            body.push('\n');
                        }
                        body.push_str("\nApply them and retry?");

                        let dialog = adw::AlertDialog::new(Some(relaxation.dialog_title()), Some(&body));
                        dialog.add_response("cancel", "Cancel");
                        dialog.add_response("apply", "Apply & Retry");
                        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
                        dialog.set_default_response(Some("apply"));
                        dialog.set_close_response("cancel");

                        let done_app_for_present = done_app.clone();
                        dialog.connect_response(None, move |_, response| {
                            if response != "apply" {
                                done_app.toast(&format!("{label_for_retry} — failed"));
                                done_app.start_next();
                                return;
                            }
                            let done_app = done_app.clone();
                            let job_for_retry = job_for_retry.clone();
                            let label_for_retry = label_for_retry.clone();
                            let known_atoms_for_retry = known_atoms_for_retry.clone();
                            let relaxation_kind = relaxation.noun_phrase();
                            let relaxation = relaxation.clone();
                            runtime::spawn_blocking(
                                move || relaxation.apply(),
                                move |result| {
                                    if result.is_ok() {
                                        // Retried once, at the front of
                                        // the queue — with
                                        // `retry_with_use_fix` false this
                                        // time, so a second failure
                                        // reports normally instead of
                                        // looping (or prompting again).
                                        done_app.queue.borrow_mut().push_front(QueueEntry {
                                            job: job_for_retry.clone(),
                                            label: label_for_retry.clone(),
                                            mutating,
                                            retry_with_use_fix: false,
                                            known_atoms: known_atoms_for_retry.clone(),
                                        });
                                        done_app.toast(&format!("{label_for_retry} — applying {relaxation_kind}, retrying"));
                                    } else {
                                        done_app.toast(&format!("{label_for_retry} — failed"));
                                    }
                                    done_app.start_next();
                                },
                            );
                        });
                        dialog.present(Some(&done_app_for_present.window));
                        return;
                    }
                // `--keep-going` (see `update_world_job`) means a big
                // `@world` update doesn't have to be all-or-nothing —
                // everything portage could still merge around a failure
                // already did. Checked only for a job that started with
                // a known multi-atom list (i.e. actually an `@world`
                // update, the one case `--keep-going` is even on): a
                // single-package install failing has nothing partial
                // about it, and stays on the existing build-failure path
                // below.
                let failed_atoms = (!success && known_atom_count > 0)
                    .then(|| emerge::parse_failed_packages(&output.borrow()))
                    .unwrap_or_default();
                // A real build failure (not the USE-flag block already
                // handled above, not a resolver issue — an ebuild phase
                // actually died) gets its own dialog with the log tail and
                // follow-up actions, rather than just a toast that's easy
                // to miss and gives no way to actually see what broke.
                // Skipped when `failed_atoms` already has the fuller,
                // multi-package picture — a single-package die-message
                // dialog would just be the *first* of potentially several
                // failures, not the whole story.
                let build_failure =
                    (!success && failed_atoms.is_empty()).then(|| emerge::parse_build_failure(&output.borrow())).flatten();
                // Sent regardless of which branch below fires — a job
                // that just finished is exactly as worth knowing about
                // whether the window's in focus or the person's stepped
                // away from a multi-hour build entirely, which a toast
                // alone (gone the moment it fades, and only ever seen if
                // this window happens to be visible right now) doesn't
                // cover.
                done_app.send_notification(&label, success);
                // Persisted off the main thread, same as every other
                // disk-writing call — see `build_log_history` for the
                // on-disk shape. Scoped to this (Portage) lane only, not
                // Flatpak: rereading a failed build's log is the actual
                // ask this exists for.
                {
                    let label = label.clone();
                    let atom = job_atom.clone();
                    let lines = output.borrow().clone();
                    runtime::spawn_blocking(move || portage_store::portage::build_log_history::record(&label, atom.as_deref(), success, &lines), |()| {});
                }
                if !failed_atoms.is_empty() {
                    done_app.present_keep_going_retry(&label, failed_atoms, known_atom_count);
                } else if let Some(failure) = build_failure {
                    build_failure::present(&done_app.window, &label, failure);
                } else {
                    done_app.toast(&if success {
                        format!("{label} — done")
                    } else {
                        format!("{label} — failed")
                    });
                }
                // This exact package's own Install/Remove button, left
                // showing its old label and no progress bar otherwise —
                // the page was already built and on screen before this
                // job ever started, so it has no way to know on its own
                // that `app.installed` just changed underneath it.
                // Flipped directly (not by rebuilding the page) rather
                // than waiting on `rescan_installed_then` — the button
                // only needs to know *this job's own* outcome, which is
                // already known here, not the full freshly-rescanned map.
                if let Some(atom) = &job_atom
                    && let Some(page) = visible_detail_page(&done_app.nav, atom) {
                        if success {
                            refresh_detail_action_button(&page, job_is_install);
                        } else {
                            if let Some(bar) = detail_progress_bar(&page) {
                                bar.set_visible(false);
                            }
                            if let Some(button) = detail_action_button(&page) {
                                button.set_sensitive(true);
                                button.remove_css_class("detail-action-pulsing");
                            }
                        }
                    }
                if mutating && success {
                    done_app.rescan_installed();
                    done_app.check_updates();
                    done_app.check_config_protect();
                    done_app.check_glsa();
                    done_app.check_sync();
                    done_app.check_preserved_rebuild();
                    // A sync is the one thing that can actually change what
                    // the eix index and any cached `--pretend` resolution
                    // say — every other mutating job (install/uninstall/
                    // update) only changes what's *installed*, which the
                    // rescans above already cover.
                    if job_for_retry.args.iter().any(|a| a == "--sync") {
                        portage_store::portage::eix::invalidate_index();
                        portage_store::portage::emerge::clear_pretend_cache();
                    }
                }
                done_app.start_next();
            },
        );
    }

}
