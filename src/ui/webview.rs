use adw::prelude::*;

/// Opens `url` for the user to read. With the `webview` Cargo feature
/// compiled in (WebKitGTK installed — see `Cargo.toml`), that's a
/// lightweight in-app browser; without it, the system browser, same as
/// every other link in this app. One call site either way, so nothing
/// upstream needs to know which actually happened.
pub fn open(anchor: &impl IsA<gtk::Widget>, title: &str, url: &str) {
    #[cfg(feature = "webview")]
    {
        present(anchor, title, url);
    }
    #[cfg(not(feature = "webview"))]
    {
        let _ = title;
        open_externally(anchor, url);
    }
}

fn open_externally(anchor: &impl IsA<gtk::Widget>, url: &str) {
    let launcher = gtk::UriLauncher::new(url);
    let window = anchor.root().and_downcast::<gtk::Window>();
    launcher.launch(window.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
}

#[cfg(feature = "webview")]
fn present(anchor: &impl IsA<gtk::Widget>, title: &str, url: &str) {
    use webkit6::prelude::*;

    let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let webview = webkit6::WebView::new();
    webview.set_vexpand(true);
    webview.set_hexpand(true);
    webview.load_uri(url);

    let spinner = gtk::Spinner::new();
    spinner.start();
    spinner.set_width_request(32);
    spinner.set_height_request(32);
    let spinner_holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    spinner_holder.set_valign(gtk::Align::Center);
    spinner_holder.set_halign(gtk::Align::Center);
    spinner_holder.set_vexpand(true);
    spinner_holder.set_hexpand(true);
    spinner_holder.append(&spinner);

    // Swapped for the real page as soon as it commits (first paint), not
    // only once fully "Finished" — a page with slow-loading assets would
    // otherwise sit on a blank spinner long after there's already
    // something worth showing.
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.add_named(&spinner_holder, Some("loading"));
    stack.add_named(&webview, Some("page"));

    let title_label = gtk::Label::new(Some(title));
    title_label.add_css_class("heading");
    title_label.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let back = gtk::Button::from_icon_name("go-previous-symbolic");
    back.set_sensitive(false);
    back.set_tooltip_text(Some("Back"));
    let forward = gtk::Button::from_icon_name("go-next-symbolic");
    forward.set_sensitive(false);
    forward.set_tooltip_text(Some("Forward"));
    let reload = gtk::Button::from_icon_name("view-refresh-symbolic");
    reload.set_tooltip_text(Some("Reload"));
    let open_external = gtk::Button::from_icon_name("adw-external-link-symbolic");
    open_external.set_tooltip_text(Some("Open in default browser"));

    {
        let webview = webview.clone();
        back.connect_clicked(move |_| webview.go_back());
    }
    {
        let webview = webview.clone();
        forward.connect_clicked(move |_| webview.go_forward());
    }
    {
        let webview = webview.clone();
        reload.connect_clicked(move |_| webview.reload());
    }
    {
        let url = url.to_string();
        open_external.connect_clicked(move |button| open_externally(button, &url));
    }

    {
        let back = back.clone();
        let forward = forward.clone();
        let stack = stack.clone();
        webview.connect_load_changed(move |webview, event| {
            back.set_sensitive(webview.can_go_back());
            forward.set_sensitive(webview.can_go_forward());
            if matches!(event, webkit6::LoadEvent::Committed | webkit6::LoadEvent::Finished) {
                stack.set_visible_child_name("page");
            }
        });
    }
    {
        let title_label = title_label.clone();
        let fallback_title = title.to_string();
        webview.connect_title_notify(move |webview| {
            let text = webview.title().filter(|t| !t.is_empty()).map(|t| t.to_string()).unwrap_or_else(|| fallback_title.clone());
            title_label.set_text(&text);
        });
    }

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title_label));
    header.pack_start(&back);
    header.pack_start(&forward);
    header.pack_start(&reload);
    header.pack_end(&open_external);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));

    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(960)
        .content_height(720)
        .child(&toolbar)
        .build();
    dialog.present(Some(&window));
}
