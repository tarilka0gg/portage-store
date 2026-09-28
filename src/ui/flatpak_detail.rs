use super::{page_box, scrolled};
use adw::prelude::*;
use portage_store::flatpak;
use portage_store::portage::{emerge, flathub};
use std::rc::Rc;

/// The same "four facts in a row" tile `detail.rs`'s `info_tile` builds for
/// a Portage package — same CSS classes (`info-tile`/`info-tile-icon`), so
/// it sits in the same visual row style — but static: nothing here expands
/// into a breakdown panel the way Portage's own version-picker/dependency
/// tiles do, since a Flatpak app has no per-package breakdown to show.
fn fact_tile(icon_name: &str, title: &str, subtitle: &str) -> gtk::Widget {
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(20);
    let icon_holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    icon_holder.set_halign(gtk::Align::Center);
    icon_holder.set_valign(gtk::Align::Center);
    icon_holder.set_size_request(44, 44);
    icon_holder.add_css_class("info-tile-icon");
    // Without this, a plain GtkBox packs its child at its natural size
    // starting from the leading edge rather than centering it — the exact
    // bug that left this tile's icon sitting off to one side instead of
    // centered in its round backdrop. `detail.rs`'s own `info_tile` sets
    // this on its icon for the same reason; dropped when this tile was
    // adapted from it.
    icon.set_hexpand(true);
    icon_holder.append(&icon);

    let title_label = gtk::Label::new(Some(title));
    title_label.add_css_class("heading");
    title_label.set_wrap(true);
    title_label.set_justify(gtk::Justification::Center);

    let subtitle_label = gtk::Label::new(Some(subtitle));
    subtitle_label.add_css_class("dim-label");
    subtitle_label.add_css_class("caption");
    subtitle_label.set_wrap(true);
    subtitle_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    subtitle_label.set_max_width_chars(20);
    subtitle_label.set_justify(gtk::Justification::Center);

    let tile = gtk::Box::new(gtk::Orientation::Vertical, 4);
    tile.set_hexpand(true);
    tile.set_valign(gtk::Align::Start);
    tile.add_css_class("flat");
    tile.add_css_class("info-tile");
    tile.append(&icon_holder);
    tile.append(&title_label);
    tile.append(&subtitle_label);
    tile.upcast()
}

/// A real navigation page for a Flatpak-only search hit — the same
/// "push a page" treatment `browse::open_detail` gives a Portage package,
/// down to reusing its fact-tile row style (`fact_tile`, above) and its
/// screenshot carousel (`detail::screenshot_carousel`) fed from Flathub's
/// own AppStream catalog, looked up directly by this app's exact
/// `app_id` (`flathub::lookup_by_app_id` — reliable in a way the
/// name-guessing `flathub::lookup` a Portage page uses isn't, since a
/// Flatpak search hit already carries the id Flathub itself uses).
///
/// Still deliberately smaller than `detail::build`: no USE flags, no
/// `--pretend` preview, no sandbox-build/downgrade — none of that applies
/// to a Flatpak app.
pub fn build(
    app: &flatpak::FlatpakApp,
    download_kib: Option<u64>,
    flathub: Option<flathub::FlathubApp>,
    on_install: Rc<dyn Fn(flatpak::FlatpakApp)>,
) -> adw::NavigationPage {
    let icon = match flatpak::icon_path(&app.remote, &app.app_id) {
        Some(path) => {
            let image = gtk::Image::from_file(path);
            image.set_pixel_size(128);
            image.set_size_request(128, 128);
            image
        }
        None => {
            let image = gtk::Image::from_icon_name("package-x-generic-symbolic");
            image.set_pixel_size(70);
            image.set_size_request(128, 128);
            image.add_css_class("icon-fallback");
            image
        }
    };
    icon.set_valign(gtk::Align::Start);

    let name_label = gtk::Label::new(Some(&app.name));
    name_label.set_xalign(0.0);
    name_label.add_css_class("hero-title");
    name_label.set_wrap(true);

    let summary = gtk::Label::new(Some(&app.description));
    summary.set_xalign(0.0);
    summary.set_wrap(true);
    summary.add_css_class("dim-label");

    let install_button = gtk::Button::with_label("Install");
    install_button.add_css_class("suggested-action");
    install_button.set_valign(gtk::Align::Center);
    install_button.set_halign(gtk::Align::End);
    {
        let app_for_click = app.clone();
        install_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            button.set_label("Installing…");
            on_install(app_for_click.clone());
        });
    }

    let hero_text = gtk::Box::new(gtk::Orientation::Vertical, 4);
    hero_text.set_valign(gtk::Align::Center);
    hero_text.set_hexpand(true);
    hero_text.append(&name_label);
    hero_text.append(&summary);

    let hero = gtk::Box::new(gtk::Orientation::Horizontal, 18);
    hero.append(&icon);
    hero.append(&hero_text);
    hero.append(&install_button);

    let content = page_box();
    content.append(&hero);

    // Real screenshots first, when Flathub had any, via the exact same
    // carousel widget a Portage package's page uses — then the long-form
    // description below it, when Flathub's own AppStream catalog had one
    // richer than the one-line summary `flatpak search` returns (the same
    // "prefer the fuller text when one turns up" idea `detail.rs`'s own
    // enrichment chain follows). Same order Portage's own page uses:
    // carousel, then description.
    if let Some(flathub) = &flathub {
        if !flathub.screenshots.is_empty() {
            content.append(&super::detail::screenshot_carousel(&flathub.screenshots));
        }
        if !flathub.description.is_empty() && flathub.description != app.description {
            let body = gtk::Label::new(Some(&flathub.description));
            body.set_xalign(0.0);
            body.set_wrap(true);
            body.add_css_class("description-body");
            content.append(&body);
        }
    }

    // Driven by `Caps`, not hardcoded prose about Flatpak specifically —
    // this is exactly the sentence that would need to change (or vanish)
    // if a third, root-needing backend ever reused this page.
    let caps = portage_store::backend::SourceId::Flatpak.caps();
    let install_method = if caps.sandboxed && !caps.needs_root { "Sandboxed" } else { "System install" };

    let size_text = download_kib.map(emerge::format_size_kib).unwrap_or_else(|| "Unknown".to_string());
    let version_text = if app.version.is_empty() { "Unknown".to_string() } else { app.version.clone() };

    let tiles = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tiles.set_homogeneous(true);
    tiles.add_css_class("card");
    tiles.append(&fact_tile("folder-download-symbolic", "Download Size", &size_text));
    tiles.append(&fact_tile("security-high-symbolic", "Install Method", install_method));
    tiles.append(&fact_tile("software-update-available-symbolic", "Version", &version_text));
    tiles.append(&fact_tile("network-server-symbolic", "Source", &app.remote));
    content.append(&tiles);

    // The same "Details" list a Portage package's page ends on (license,
    // project website, wiki/bug tracker links) — here: license and
    // developer straight from Flathub's own AppStream catalog when
    // available, a homepage link when Flathub had one, and a link to this
    // app's own Flathub listing unconditionally (that URL only depends on
    // `app_id`, needing no lookup to have succeeded at all).
    let details = adw::PreferencesGroup::builder().title("Details").build();
    if let Some(flathub) = &flathub {
        if let Some(license) = &flathub.project_license {
            let row = adw::ActionRow::builder().title("License").subtitle(license).build();
            row.add_prefix(&gtk::Image::from_icon_name("dialog-information-symbolic"));
            details.add(&row);
        }
        if let Some(developer) = &flathub.developer_name {
            let row = adw::ActionRow::builder().title("Developer").subtitle(developer).build();
            row.add_prefix(&gtk::Image::from_icon_name("system-users-symbolic"));
            details.add(&row);
        }
        if let Some(homepage) = &flathub.homepage {
            details.add(&super::detail::link_row("web-browser-symbolic", "Project Website", homepage));
        }
    }
    details.add(&super::detail::link_row("software-store-symbolic", "Flathub Page", &format!("https://flathub.org/apps/{}", app.app_id)));
    content.append(&details);

    let header = adw::HeaderBar::new();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scrolled(&adw::Clamp::builder().maximum_size(700).child(&content).build())));

    adw::NavigationPage::builder().title(&app.name).tag(&app.app_id).child(&toolbar).build()
}
