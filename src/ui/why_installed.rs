use portage_store::portage::reverse_deps::{self, DepNode};
use crate::ui::runtime;
use adw::prelude::*;

/// One node's row: the atom, an `@world` badge when that's the actual
/// reason (nothing above it needed explaining), indented under whichever
/// dependent pulled it in.
fn node_widget(node: &DepNode, depth: u32) -> gtk::Widget {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_start((depth * 20) as i32);

    let bullet = gtk::Label::new(Some(if depth == 0 { "" } else { "↳" }));
    bullet.add_css_class("dim-label");
    row.append(&bullet);

    let atom_label = gtk::Label::new(Some(&node.atom));
    atom_label.set_xalign(0.0);
    atom_label.add_css_class("monospace");
    atom_label.add_css_class("caption");
    row.append(&atom_label);

    if node.in_world {
        let badge = gtk::Label::new(Some("@world"));
        badge.add_css_class("accent");
        badge.add_css_class("caption-heading");
        row.append(&badge);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
    column.append(&row);

    for child in &node.children {
        column.append(&node_widget(child, depth + 1));
    }
    if node.total_children > node.children.len() {
        let more = gtk::Label::new(Some(&format!("+{} more", node.total_children - node.children.len())));
        more.set_xalign(0.0);
        more.set_margin_start(((depth + 1) * 20) as i32 + 16);
        more.add_css_class("dim-label");
        more.add_css_class("caption");
        column.append(&more);
    }

    column.upcast()
}

/// Opens the "why is this installed" tree for one exact installed
/// version — walks `equery depends` up from it until reaching packages
/// actually selected in `@world`, which is the real answer to "why do I
/// have this" rather than a flat, unordered `@world` dump.
pub fn present(anchor: &impl IsA<gtk::Widget>, display_name: &str, atom_with_version: String) {
    let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let spinner = gtk::Spinner::new();
    spinner.set_spinning(true);
    spinner.set_width_request(32);
    spinner.set_height_request(32);
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 12);
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    loading.set_vexpand(true);
    loading.append(&spinner);
    loading.append(&gtk::Label::new(Some("Tracing dependents…")));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&loading));

    let dialog = adw::Dialog::builder()
        .title(format!("Why Is {display_name} Installed?"))
        .content_width(560)
        .content_height(640)
        .child(&toolbar)
        .build();
    dialog.present(Some(&window));

    let toolbar_for_done = toolbar.clone();
    runtime::spawn_blocking(
        move || reverse_deps::why_installed(&atom_with_version),
        move |tree| {
            let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
            column.set_margin_top(16);
            column.set_margin_bottom(24);
            column.set_margin_start(16);
            column.set_margin_end(16);

            let intro = gtk::Label::new(Some(
                "Every installed package that depends on this, traced up to whatever's actually \
                 selected in @world — that's the real reason it's here.",
            ));
            intro.set_xalign(0.0);
            intro.set_wrap(true);
            intro.add_css_class("dim-label");
            column.append(&intro);
            column.append(&node_widget(&tree, 0));

            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .vexpand(true)
                .child(&column)
                .build();
            toolbar_for_done.set_content(Some(&scroller));
        },
    );
}
