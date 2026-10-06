mod ui;

use adw::prelude::*;
use gtk::gdk;

const APP_ID: &str = "io.github.tarilka0gg.PortageStore";

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(include_str!("ui/style.css"));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// GTK normally animates popovers/revealers/etc. on its own — nothing in
/// this app needs to hand-roll that — but `gtk-enable-animations` is a
/// desktop-reported setting (normally relayed from a portal/XSettings
/// provider), and on a compositor that doesn't provide one, GTK can fall
/// back to leaving it off rather than defaulting to on. Explicitly
/// requesting it stays on is a no-op wherever it was already enabled, and
/// a real fix (popovers, revealers — including the detail page's own
/// "Show More" and expandable fact tiles — actually animating instead of
/// snapping) wherever it wasn't.
fn ensure_animations_enabled() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_enable_animations(true);
    }
}

fn main() -> gtk::glib::ExitCode {
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_startup(|_| {
        load_css();
        ensure_animations_enabled();
    });
    application.connect_activate(|app| {
        ui::App::build(app).present();
    });
    application.run()
}
