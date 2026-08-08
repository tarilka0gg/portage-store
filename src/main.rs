mod backend;
mod flatpak;
mod portage;
mod ui;

use adw::prelude::*;
use gtk::gdk;

const APP_ID: &str = "org.gentoo.PortageStore";

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

fn main() -> gtk::glib::ExitCode {
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_startup(|_| load_css());
    application.connect_activate(|app| {
        ui::App::build(app).present();
    });
    application.run()
}
