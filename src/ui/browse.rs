use super::*;

impl App {
    pub(super) fn run_search(self: &Rc<Self>, query: String) {
        if query.trim().len() < 2 {
            self.explore_stack.set_visible_child_name("landing");
            return;
        }
        self.search_generation.set(self.search_generation.get() + 1);
        let generation = self.search_generation.get();

        let parsed = search_query::parse(&query);
        *self.search_query_use_flag.borrow_mut() = parsed.use_flag.clone();
        self.search_query_installed_only.set(parsed.installed_only);

        self.results_heading.set_text(&format!("Results for \u{201c}{query}\u{201d}"));
        self.explore_stack.set_visible_child_name("results");
        widgets::clear(&self.results_grid);
        self.clear_flatpak_section();
        self.results_spinner.start();
        self.results_spinner.set_visible(true);
        self.scroll_to_top();

        let app = self.clone();
        runtime::spawn_blocking(
            move || resolve_search(&parsed),
            move |result| {
                if generation != app.search_generation.get() {
                    return;
                }
                app.show_results(result);
            },
        );

        // Runs fully in parallel with the Portage search above, on its
        // own `spawn_blocking` call — never gates or delays Portage's own
        // render. Whenever it finishes (typically a few hundred ms, but
        // nothing here waits on that), `apply_flatpak_results` only adds
        // chips to cards already on screen and appends the "Also
        // available via Flatpak" section — it never reorders or replaces
        // what Portage already drew, so a slow Flatpak search can't make
        // already-visible results jump around.
        if self.flatpak_available.get() {
            let app = self.clone();
            runtime::spawn_blocking(
                move || (flatpak::search(&query).unwrap_or_default(), flatpak::installed().unwrap_or_default()),
                move |(hits, installed)| {
                    if generation != app.search_generation.get() {
                        return;
                    }
                    app.apply_flatpak_results(hits, installed);
                },
            );
        }
    }

    pub(super) fn browse_category(self: &Rc<Self>, name: &'static str, categories: &'static [&'static str]) {
        self.search_generation.set(self.search_generation.get() + 1);
        let generation = self.search_generation.get();

        self.search_entry.set_text("");
        self.search_bar.set_search_mode(false);
        self.search_query_use_flag.borrow_mut().take();
        self.search_query_installed_only.set(None);
        self.results_heading.set_text(name);
        self.explore_stack.set_visible_child_name("results");
        widgets::clear(&self.results_grid);
        // Category browsing is a curated, Portage-only showcase (like the
        // landing carousels) — it never gets a Flatpak layer, so any
        // leftover section from a previous free-text search is dropped.
        self.clear_flatpak_section();
        self.results_spinner.start();
        self.results_spinner.set_visible(true);
        self.scroll_to_top();

        let app = self.clone();
        runtime::spawn_blocking(
            move || eix::list_categories(categories).map_err(|e| e.to_string()),
            move |result| {
                if generation != app.search_generation.get() {
                    return;
                }
                app.show_results(result);
            },
        );
    }

    /// Puts the browse view back at the top. Called whenever what is on
    /// screen changes wholesale — a new search, a new category — because
    /// otherwise the previous scroll position carries over and the fresh
    /// results open somewhere in their middle.
    pub(super) fn scroll_to_top(&self) {
        self.explore_scroller.vadjustment().set_value(0.0);
    }

    /// The "take me back" gesture, double-click-on-the-tab triggered.
    ///
    /// A package detail page always pops back to the tab it was opened
    /// from first, regardless of which tab that is. From there, behaviour
    /// depends on *which* tab is showing: Explore has a real home (the
    /// category-tile landing page) to unwind to, so it goes landing, then
    /// top. Installed and Updates have no such second screen — there's
    /// nothing to navigate to below their single list — so double-clicking
    /// either just scrolls that list to the top, the same as double-
    /// clicking Explore once it's already on the landing page.
    pub(super) fn home_or_top(self: &Rc<Self>) {
        if self.nav.visible_page().and_then(|p| p.tag()).as_deref() != Some("main") {
            self.nav.pop_to_tag("main");
            return;
        }
        match self.view_stack.visible_child_name().as_deref() {
            Some("installed") => self.installed_scroller.vadjustment().set_value(0.0),
            Some("updates") => self.updates_scroller.vadjustment().set_value(0.0),
            _ => {
                if self.explore_stack.visible_child_name().as_deref() != Some("landing") {
                    self.search_entry.set_text("");
                    self.search_bar.set_search_mode(false);
                    self.browsing_landing();
                } else {
                    self.scroll_to_top();
                }
            }
        }
    }

    pub(super) fn browsing_landing(self: &Rc<Self>) {
        self.search_generation.set(self.search_generation.get() + 1);
        self.explore_stack.set_visible_child_name("landing");
        self.clear_flatpak_section();
        self.scroll_to_top();
    }

    pub(super) fn show_results(self: &Rc<Self>, result: Result<Vec<PackageSummary>, String>) {
        self.results_spinner.stop();
        self.results_spinner.set_visible(false);

        let packages = match result {
            Ok(packages) => packages,
            Err(err) => {
                widgets::clear(&self.results_grid);
                self.toast(&format!("Search failed: {err}"));
                return;
            }
        };

        *self.last_results.borrow_mut() = packages;
        self.render_filtered_results();
    }

    /// Re-applies the current `search_filters` (and sort order) to
    /// `last_results` and rebuilds `results_grid` from scratch. Called
    /// both after a fresh search/category fetch and whenever a filter
    /// control changes — filtering is pure and in-memory, so there's
    /// nothing to await here.
    pub(super) fn render_filtered_results(self: &Rc<Self>) {
        widgets::clear(&self.results_grid);

        let base_count = self.last_results.borrow().len();
        let mut filtered = eix::apply_filters(self.last_results.borrow().clone(), &self.search_filters.borrow());

        // The search box's own `use:`/`installed:` operators (see
        // `run_search`/`portage::search_query`) — kept as a separate pass
        // rather than folded into `search_filters`, since the filter
        // popover always rewrites that wholesale from its own widgets and
        // would silently clobber whatever a typed operator set.
        if let Some(flag) = self.search_query_use_flag.borrow().as_deref() {
            filtered.retain(|pkg| pkg.iuse.iter().any(|f| f.name == flag));
        }
        if let Some(installed_only) = self.search_query_installed_only.get() {
            let installed = self.installed.borrow();
            filtered.retain(|pkg| installed.contains_key(&pkg.atom()) == installed_only);
        }

        if base_count == 0 {
            self.results_heading.set_text("No results — try a different search term");
            return;
        }
        if filtered.is_empty() {
            self.results_heading.set_text("No results match the current filters");
            return;
        }
        if filtered.len() != base_count {
            self.results_heading.set_text(&format!("{} of {} results match the current filters", filtered.len(), base_count));
        }

        let installed = self.installed.borrow();
        let icons = self.icon_paths.borrow();
        let mut cards = HashMap::new();
        let chips = self.flatpak_chips.borrow();
        for pkg in filtered.into_iter().take(300) {
            let (card, _icon) = widgets::package_card(&pkg, &installed, &icons);
            if chips.contains_key(&pkg.atom()) {
                widgets::add_flatpak_chip(&card);
            }
            cards.insert(pkg.atom(), card.clone());
            self.connect_card(&card, pkg);
            self.results_grid.insert(&card, -1);
        }
        drop(chips);
        *self.search_cards.borrow_mut() = cards;
        self.scroll_to_top();
    }

    /// The funnel popover next to search results: USE flag presence,
    /// masked/unmasked, overlay-only, license substring, and sort — every
    /// control writes straight into `search_filters` and re-renders
    /// immediately via `render_filtered_results`, since filtering never
    /// needs to touch `eix` again once a result list is in hand.
    pub(super) fn build_filter_popover(self: &Rc<Self>) -> gtk::Popover {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
        column.set_margin_top(12);
        column.set_margin_bottom(12);
        column.set_margin_start(12);
        column.set_margin_end(12);
        column.set_width_request(280);

        let use_row = adw::EntryRow::builder().title("USE flag").build();
        let use_mode = gtk::DropDown::from_strings(&["Has flag", "Lacks flag"]);
        use_mode.set_margin_top(4);

        let masked_mode = gtk::DropDown::from_strings(&["Any", "Masked only", "Unmasked only"]);
        let masked_label = gtk::Label::new(Some("Masked"));
        masked_label.set_xalign(0.0);
        masked_label.add_css_class("dim-label");
        masked_label.add_css_class("caption");

        let overlay_only = gtk::CheckButton::with_label("Overlay packages only");

        let license_row = adw::EntryRow::builder().title("License contains").build();

        let sort_label = gtk::Label::new(Some("Sort by"));
        sort_label.set_xalign(0.0);
        sort_label.add_css_class("dim-label");
        sort_label.add_css_class("caption");
        let sort_mode = gtk::DropDown::from_strings(&["Relevance", "Name (A–Z)", "Name (Z–A)", "License"]);

        let reset_button = gtk::Button::with_label("Reset Filters");
        reset_button.add_css_class("flat");

        column.append(&use_row);
        column.append(&use_mode);
        column.append(&masked_label);
        column.append(&masked_mode);
        column.append(&overlay_only);
        column.append(&license_row);
        column.append(&sort_label);
        column.append(&sort_mode);
        column.append(&reset_button);

        let apply: Rc<dyn Fn()> = Rc::new({
            let app = self.clone();
            let use_row = use_row.clone();
            let use_mode = use_mode.clone();
            let masked_mode = masked_mode.clone();
            let overlay_only = overlay_only.clone();
            let license_row = license_row.clone();
            let sort_mode = sort_mode.clone();
            let filter_button = self.filter_button.clone();
            move || {
                let flag = use_row.text().trim().to_string();
                let use_flag = (!flag.is_empty())
                    .then(|| eix::UseConstraint { flag, must_be_set: use_mode.selected() == 0 });
                let masked = match masked_mode.selected() {
                    1 => Some(true),
                    2 => Some(false),
                    _ => None,
                };
                let license = license_row.text().trim().to_string();
                let sort = match sort_mode.selected() {
                    1 => eix::SortOrder::NameAsc,
                    2 => eix::SortOrder::NameDesc,
                    3 => eix::SortOrder::LicenseAsc,
                    _ => eix::SortOrder::Default,
                };
                let filters = eix::SearchFilters {
                    use_flag,
                    masked,
                    overlay_only: overlay_only.is_active(),
                    license: (!license.is_empty()).then_some(license),
                    sort,
                };
                // A visual cue that a filter is active — otherwise a
                // filtered-down result list with no obvious cause looks
                // like a bug rather than a deliberate narrowing.
                if filters.is_default() {
                    filter_button.remove_css_class("suggested-action");
                } else {
                    filter_button.add_css_class("suggested-action");
                }
                *app.search_filters.borrow_mut() = filters;
                app.render_filtered_results();
            }
        });

        {
            let apply = apply.clone();
            use_row.connect_changed(move |_| apply());
        }
        {
            let apply = apply.clone();
            use_mode.connect_selected_notify(move |_| apply());
        }
        {
            let apply = apply.clone();
            masked_mode.connect_selected_notify(move |_| apply());
        }
        {
            let apply = apply.clone();
            overlay_only.connect_toggled(move |_| apply());
        }
        {
            let apply = apply.clone();
            license_row.connect_changed(move |_| apply());
        }
        {
            let apply = apply.clone();
            sort_mode.connect_selected_notify(move |_| apply());
        }
        {
            let apply = apply.clone();
            reset_button.connect_clicked(move |_| {
                use_row.set_text("");
                use_mode.set_selected(0);
                masked_mode.set_selected(0);
                overlay_only.set_active(false);
                license_row.set_text("");
                sort_mode.set_selected(0);
                apply();
            });
        }

        let popover = gtk::Popover::new();
        popover.set_child(Some(&column));
        popover
    }

    pub(super) fn connect_card(self: &Rc<Self>, card: &gtk::Button, pkg: PackageSummary) {
        let app = self.clone();
        card.connect_clicked(move |_| app.open_detail(pkg.clone()));
    }

    pub(super) fn open_detail(self: &Rc<Self>, pkg: PackageSummary) {
        // Pushed immediately so there's something to look at while both
        // `eix::lookup` below and the detail page's own description/
        // screenshot enrichment (Flathub, then Terminal Trove/GitHub if
        // still needed) are in flight — swapped for the real page in
        // `on_ready` below once that settles, rather than pushing the real
        // page right away and letting its content visibly fill in piece by
        // piece.
        let loading_page = loading_navigation_page();
        self.nav.push(&loading_page);

        // eix's search results carry no IUSE for packages matched by
        // description, so re-look the package up to get full metadata.
        let atom = pkg.atom();
        let app = self.clone();
        runtime::spawn_blocking(
            move || eix::lookup(&atom).ok().flatten(),
            move |full| {
                let pkg = full.unwrap_or_else(|| pkg.clone());
                let install_app = app.clone();
                let uninstall_app = app.clone();
                let sandbox_app = app.clone();
                let flatpak_app = app.clone();
                let downgrade_app = app.clone();

                // `on_ready` needs the built page to push it, but the page
                // isn't built until `detail::build` returns — which itself
                // needs `on_ready` to pass in. Broken by routing the page
                // through this cell instead of capturing it directly: by
                // the time `on_ready` can actually run (asynchronously,
                // after this whole function returns), `page` below has
                // long since been stored in it.
                let held_page: Rc<RefCell<Option<adw::NavigationPage>>> = Rc::new(RefCell::new(None));
                let held_page_for_ready = held_page.clone();
                let nav_for_ready = app.nav.clone();
                let on_ready: Rc<dyn Fn()> = Rc::new(move || {
                    if let Some(page) = held_page_for_ready.borrow_mut().take() {
                        nav_for_ready.pop();
                        nav_for_ready.push(&page);
                    }
                });

                let page = detail::build(
                    &pkg,
                    &app.installed.borrow(),
                    &app.icon_paths.borrow(),
                    app.settings.borrow().prefer_binary_packages,
                    Rc::new(move |atom: String| install_app.install(atom)),
                    Rc::new(move |atom: String| uninstall_app.uninstall(atom)),
                    Rc::new(move |atom: String| sandbox_app.sandbox_build(atom)),
                    Rc::new(move |fp_app: flatpak::FlatpakApp| flatpak_app.present_flatpak_detail(fp_app)),
                    Rc::new(move |atom: String, version: String| downgrade_app.downgrade(atom, version)),
                    on_ready,
                );
                *held_page.borrow_mut() = Some(page);
            },
        );
    }
}
