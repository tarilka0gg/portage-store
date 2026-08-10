use super::*;

impl App {
    pub(super) fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// Clears this lane's own lines from the log sheet for a fresh job —
    /// called at the start of `start_next`/`start_next_flatpak` rather
    /// than only when the drawer happens to be open, so `log_lines` never
    /// carries a previous job's output into a new one even if the sheet
    /// was never opened for it. Scoped to `source` rather than wiping the
    /// whole buffer: Portage and Flatpak jobs can run at the same time
    /// (see `flatpak_queue`'s own doc comment), and a new job starting in
    /// one lane shouldn't erase the other lane's still-relevant,
    /// still-running output.
    pub(super) fn clear_job_log(&self, source: &'static str) {
        self.log_lines.borrow_mut().retain(|l| l.source != source);
        self.render_log_drawer();
    }

    /// Appends one line to the log sheet, tagged with which lane produced
    /// it. Re-renders immediately if the sheet is currently open (a
    /// closed sheet just accumulates — no reason to touch the
    /// `TextBuffer` for content nobody's looking at yet).
    pub(super) fn append_job_log(&self, source: &'static str, text: &str) {
        let is_error = is_log_error_line(text);
        self.log_lines.borrow_mut().push(LogLine { source, text: text.to_string(), is_error });
        if self.log_drawer_revealer.reveals_child() {
            self.render_log_drawer();
        }
    }

    /// Rebuilds the log sheet's `TextBuffer` from `log_lines`, applying
    /// the "Errors Only" filter and scrolling to the bottom — the whole
    /// buffer is replaced rather than incrementally appended to, since
    /// toggling the filter needs a full re-render anyway and a rebuild is
    /// cheap even for a few thousand lines.
    pub(super) fn render_log_drawer(&self) {
        let lines = self.log_lines.borrow();
        let errors_only = self.log_errors_only.is_active();
        // Both lanes can be running at once (see `flatpak_queue`'s own
        // doc comment) — the source prefix only earns its keep when
        // there's more than one lane's output actually present, so a
        // single-lane run reads as a plain, unprefixed log like before.
        let multiple_sources = lines.iter().map(|l| l.source).collect::<std::collections::HashSet<_>>().len() > 1;
        let text: String = lines
            .iter()
            .filter(|l| !errors_only || l.is_error)
            .map(|l| if multiple_sources { format!("[{}] {}", l.source, l.text) } else { l.text.clone() })
            .collect::<Vec<_>>()
            .join("\n");
        self.log_drawer_buffer.set_text(&text);
        let end = self.log_drawer_buffer.end_iter();
        self.log_drawer_buffer.place_cursor(&end);
        self.log_drawer_scroller.vadjustment().set_value(self.log_drawer_scroller.vadjustment().upper());
    }

    pub(super) fn toggle_log_drawer(&self) {
        let opening = !self.log_drawer_revealer.reveals_child();
        self.log_drawer_revealer.set_reveal_child(opening);
        if opening {
            self.render_log_drawer();
        }
    }

    /// A real desktop notification, not just a toast — a multi-hour
    /// `@world` update is exactly the kind of job someone starts and
    /// then leaves the computer for, and a toast that's already faded by
    /// the time they're back tells them nothing.
    pub(super) fn send_notification(&self, label: &str, success: bool) {
        let Some(application) = self.window.application() else { return };
        let notification = gtk::gio::Notification::new(label);
        notification.set_body(Some(if success {
            "Finished successfully."
        } else {
            "Failed — open Portage Store for details."
        }));
        notification.set_priority(if success {
            gtk::gio::NotificationPriority::Normal
        } else {
            gtk::gio::NotificationPriority::High
        });
        // A fixed id (not the app id — this identifies the notification
        // itself) so a second job finishing while the first's
        // notification is still showing replaces it instead of stacking
        // up duplicates for jobs that have already been superseded.
        application.send_notification(Some("job-complete"), &notification);
    }

    /// Keeps a `GtkApplication` inhibitor (suspend + idle) held for exactly
    /// as long as either job lane is actually running — a multi-hour
    /// `@world` rebuild killed by the machine suspending partway through is
    /// a real loss, not just an inconvenience. Idempotent by construction
    /// (checks current state before acting), so it's safe to call from
    /// every `running`/`flatpak_running` transition point rather than
    /// threading extra "did this change" bookkeeping through each one.
    pub(super) fn sync_inhibit(&self) {
        let Some(application) = self.window.application() else { return };
        let busy = self.running.get() || self.flatpak_running.get();
        let held = self.inhibit_cookie.get();
        if busy && held.is_none() {
            let flags = gtk::ApplicationInhibitFlags::SUSPEND | gtk::ApplicationInhibitFlags::IDLE;
            let cookie = application.inhibit(Some(&self.window), flags, Some("Portage Store: build in progress"));
            self.inhibit_cookie.set(Some(cookie));
        } else if !busy && let Some(cookie) = held {
            application.uninhibit(cookie);
            self.inhibit_cookie.set(None);
        }
    }

}
