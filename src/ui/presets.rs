use super::{App, QueueEntry};
use crate::ui::runtime;
use adw::prelude::*;
use portage_store::portage::preset::{self, OwnedPreset};
use std::rc::Rc;

/// One preset's row: a summary of what it does, an Export button (saves
/// it as JSON someone else can `Import` back), and an Apply button (sets
/// its USE flags, then queues its packages for install) — the same
/// two-step "apply_use_flags, then install_job" split
/// `portage::preset` itself documents.
fn preset_row(app: &Rc<App>, list: &gtk::ListBox, preset: OwnedPreset) -> adw::ActionRow {
    let subtitle = format!(
        "{} — {} package(s), {} USE flag(s)",
        preset.description,
        preset.packages.len(),
        preset.use_flags.len()
    );
    let row = adw::ActionRow::builder().title(preset.name.clone()).subtitle(subtitle).build();
    row.add_prefix(&gtk::Image::from_icon_name("package-x-generic-symbolic"));

    let button_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    button_box.set_valign(gtk::Align::Center);

    let export_button = gtk::Button::from_icon_name("document-save-symbolic");
    export_button.add_css_class("flat");
    export_button.set_tooltip_text(Some("Export as a shareable file"));
    button_box.append(&export_button);

    let apply_button = gtk::Button::with_label("Apply");
    apply_button.add_css_class("pill");
    button_box.append(&apply_button);

    row.add_suffix(&button_box);

    {
        let app = app.clone();
        let preset = preset.clone();
        let file_name = format!("{}.json", preset.name.to_lowercase().replace(' ', "-"));
        export_button.connect_clicked(move |button| {
            let file_dialog = gtk::FileDialog::builder().title("Export Preset").initial_name(&file_name).build();
            let window = app.window.clone();
            let preset = preset.clone();
            let button = button.clone();
            file_dialog.save(Some(&window), gtk::gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                let button = button.clone();
                runtime::spawn_blocking(
                    move || preset::export(&preset, &path).map_err(|e| e.to_string()),
                    move |result| {
                        if let Err(err) = result {
                            button.set_tooltip_text(Some(&format!("Couldn't export: {err}")));
                        }
                    },
                );
            });
        });
    }

    {
        let app = app.clone();
        let preset = preset.clone();
        let row = row.clone();
        apply_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            let app = app.clone();
            let preset = preset.clone();
            let row = row.clone();
            let button_for_result = button.clone();
            runtime::spawn_blocking(
                move || preset::apply_use_flags(&preset).map(|()| preset).map_err(|e| e.to_string()),
                move |result| match result {
                    Ok(preset) => {
                        if !preset.packages.is_empty() {
                            app.enqueue(QueueEntry {
                                job: preset::install_job(&preset, app.settings.borrow().prefer_binary_packages),
                                label: format!("Applying preset: {}", preset.name),
                                mutating: true,
                                retry_with_use_fix: true,
                                known_atoms: Vec::new(),
                            });
                        }
                        row.set_subtitle("Applied — packages queued");
                        button_for_result.set_sensitive(true);
                    }
                    Err(err) => {
                        row.set_subtitle(&format!("Couldn't apply: {err}"));
                        button_for_result.set_sensitive(true);
                    }
                },
            );
        });
    }

    list.append(&row);
    row
}

/// Opens the "Package Presets" dialog: the two built-in presets
/// (`preset::BUILTIN_PRESETS`) plus an Import action for a preset someone
/// else exported — reusing the exact `GtkFileDialog` save/open pattern
/// `preferences.rs`'s profile-bundle export/import already established.
pub fn present(app: &Rc<App>) {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    for builtin in preset::BUILTIN_PRESETS {
        preset_row(app, &list, OwnedPreset::from(builtin));
    }

    let intro = gtk::Label::new(Some(
        "A preset sets USE flags and queues a starter package list — it layers onto whatever's \
         already installed rather than replacing it. Export one to share it, or import one someone \
         else sent you.",
    ));
    intro.set_xalign(0.0);
    intro.set_wrap(true);
    intro.add_css_class("dim-label");

    let column = gtk::Box::new(gtk::Orientation::Vertical, 12);
    column.set_margin_top(16);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);
    column.append(&intro);
    column.append(&list);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(560).child(&column).build())
        .build();

    let import_button = gtk::Button::from_icon_name("document-open-symbolic");
    import_button.set_tooltip_text(Some("Import a preset from a file"));

    let header = adw::HeaderBar::new();
    header.pack_start(&import_button);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder().title("Package Presets").content_width(560).content_height(480).child(&toolbar).build();

    {
        let app = app.clone();
        let list = list.clone();
        import_button.connect_clicked(move |_| {
            let file_dialog = gtk::FileDialog::builder().title("Open Preset").build();
            let window = app.window.clone();
            let app = app.clone();
            let list = list.clone();
            file_dialog.open(Some(&window), gtk::gio::Cancellable::NONE, move |result| {
                let Ok(file) = result else { return };
                let Some(path) = file.path() else { return };
                let app = app.clone();
                let list = list.clone();
                runtime::spawn_blocking(
                    move || preset::import(&path).map_err(|e| e.to_string()),
                    move |result| match result {
                        Ok(preset) => {
                            preset_row(&app, &list, preset);
                        }
                        Err(err) => app.toast(&format!("Couldn't import preset: {err}")),
                    },
                );
            });
        });
    }

    dialog.present(Some(&app.window));
}
