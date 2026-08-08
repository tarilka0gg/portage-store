use crate::portage::news::{self, NewsItem};
use crate::ui::runtime;
use adw::prelude::*;
use std::rc::Rc;

/// Builds one row for the news list page — an unread item gets a small
/// accent dot prefix (matching the update rows' own icon-prefix
/// convention elsewhere in the app) so it's obvious at a glance which
/// ones still need attention without having to open each one.
fn item_row(item: &NewsItem) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(&item.title).subtitle(&item.posted).activatable(true).build();
    row.add_prefix(&gtk::Image::from_icon_name(if item.unread {
        "mail-unread-symbolic"
    } else {
        "mail-read-symbolic"
    }));
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

/// Pushes the reader page for one item onto `nav`, fetching its body in
/// the background (a `--raw` read, which previews without marking it
/// read — see `portage::news::read`). `on_read_state_changed` is called
/// once the "Mark as Read" button actually changes something, so the
/// caller (the list page, and ultimately the app-level banner) can
/// refresh its own unread count.
fn push_reader(
    nav: &adw::NavigationView,
    item: NewsItem,
    on_read_state_changed: Rc<dyn Fn()>,
) {
    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_width_request(32);
    spinner.set_height_request(32);
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 12);
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    loading.set_vexpand(true);
    loading.append(&spinner);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&loading));
    let page = adw::NavigationPage::builder().title(&item.title).child(&toolbar).build();
    nav.push(&page);

    let number = item.number;
    let unread_at_open = item.unread;
    let page_for_ready = page.clone();
    runtime::spawn_blocking(
        move || news::read(number),
        move |result| {
            let body_text = match result {
                Ok(body) => body.body,
                Err(err) => format!("Couldn't load this item: {err}"),
            };

            let text = gtk::Label::new(Some(&body_text));
            text.set_xalign(0.0);
            text.set_wrap(true);
            text.set_selectable(true);
            text.set_can_focus(false);
            text.set_margin_top(4);

            let mark_read_button = gtk::Button::with_label("Mark as Read");
            mark_read_button.add_css_class("pill");
            mark_read_button.set_halign(gtk::Align::Start);
            mark_read_button.set_margin_top(16);
            mark_read_button.set_visible(unread_at_open);
            {
                let mark_read_button = mark_read_button.clone();
                let on_read_state_changed = on_read_state_changed.clone();
                mark_read_button.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    let on_read_state_changed = on_read_state_changed.clone();
                    let button = button.clone();
                    runtime::spawn_blocking(
                        move || news::mark_read(number),
                        move |result| {
                            if result.is_ok() {
                                button.set_visible(false);
                                on_read_state_changed();
                            } else {
                                button.set_sensitive(true);
                            }
                        },
                    );
                });
            }

            let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
            column.set_margin_top(20);
            column.set_margin_bottom(24);
            column.set_margin_start(16);
            column.set_margin_end(16);
            column.append(&text);
            column.append(&mark_read_button);

            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&adw::Clamp::builder().maximum_size(640).child(&column).build())
                .build();

            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&adw::HeaderBar::new());
            toolbar.set_content(Some(&scroller));
            page_for_ready.set_child(Some(&toolbar));
        },
    );
}

/// Opens the news reader: a list of every known item (unread ones marked),
/// each opening its full text on click. `on_unread_count_changed` fires
/// whenever an item actually gets marked read, so the caller can refresh
/// the app-level banner without needing to poll or re-open this dialog.
pub fn present(anchor: &impl IsA<gtk::Widget>, items: Vec<NewsItem>, on_unread_count_changed: Rc<dyn Fn()>) {
    let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    let nav = adw::NavigationView::new();

    for item in &items {
        let row = item_row(item);
        let item = item.clone();
        let nav_for_click = nav.clone();
        let on_unread_count_changed = on_unread_count_changed.clone();
        row.connect_activated(move |_| {
            push_reader(&nav_for_click, item.clone(), on_unread_count_changed.clone());
        });
        list.append(&row);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.set_margin_top(16);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);
    column.append(&list);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(640).child(&column).build())
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    let list_page = adw::NavigationPage::builder().title("Gentoo News").child(&toolbar).build();
    nav.add(&list_page);

    let dialog = adw::Dialog::builder().title("Gentoo News").content_width(560).content_height(680).child(&nav).build();
    dialog.present(Some(&window));
}
