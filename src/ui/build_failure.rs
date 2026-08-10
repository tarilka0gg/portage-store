use portage_store::portage::emerge::BuildFailure;
use crate::ui::runtime;
use adw::prelude::*;

/// How many trailing lines of the build log to show by default — enough
/// to actually see the real compiler/linker error (which is almost
/// always right at the end, where the build stopped), without dumping a
/// build log that can run to thousands of lines into the window whole.
const TAIL_LINES: usize = 40;

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Builds `https://bugs.gentoo.org/buglist.cgi?quicksearch=<atom>` — a
/// ready-made search rather than sending someone to bugs.gentoo.org empty
/// -handed to retype the atom themselves.
fn bugs_search_url(atom: &str) -> String {
    let encoded: String = atom
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/') { c.to_string() } else { format!("%{:02X}", c as u32) })
        .collect();
    format!("https://bugs.gentoo.org/buglist.cgi?quicksearch={encoded}")
}

/// Presents a failed build: the phase that died, the tail of its build
/// log (fetched lazily — `--pretend` never fails this way, only a real
/// install/update does, so there's no reason to read a multi-thousand-line
/// log file until a failure that actually has one happens), and the three
/// follow-up actions a stuck build most often needs.
pub fn present(anchor: &impl IsA<gtk::Widget>, label: &str, failure: BuildFailure) {
    let Some(window) = anchor.root().and_downcast::<gtk::Window>() else {
        return;
    };

    let phase_title = failure
        .phase
        .get(..1)
        .map(|first| format!("{}{} Failed", first.to_uppercase(), &failure.phase[1..]))
        .unwrap_or_else(|| "Build Failed".to_string());
    let status = adw::StatusPage::builder()
        .icon_name("dialog-error-symbolic")
        .title(phase_title)
        .description(format!("{label} — {} phase", failure.phase))
        .build();

    let log_label = gtk::Label::new(Some("Loading the build log…"));
    log_label.set_xalign(0.0);
    log_label.set_wrap(false);
    log_label.add_css_class("monospace");
    log_label.add_css_class("caption");
    log_label.add_css_class("dim-label");
    log_label.set_selectable(true);
    log_label.set_can_focus(false);

    let log_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .min_content_height(280)
        .child(&log_label)
        .build();
    log_scroller.add_css_class("card");

    let copy_button = gtk::Button::with_label("Copy Error");
    copy_button.add_css_class("pill");
    copy_button.set_sensitive(false);

    let search_button = gtk::Button::with_label("Search bugs.gentoo.org");
    search_button.add_css_class("pill");
    {
        let atom = failure.atom.clone();
        let url = bugs_search_url(&atom);
        search_button.connect_clicked(move |button| {
            super::webview::open(button, &format!("bugs.gentoo.org — {atom}"), &url);
        });
    }

    let open_log_button = gtk::Button::with_label("Open Full Log");
    open_log_button.add_css_class("pill");
    open_log_button.set_visible(failure.build_log_path.is_some());
    if let Some(path) = &failure.build_log_path {
        let path = path.clone();
        open_log_button.connect_clicked(move |button| {
            let launcher = gtk::UriLauncher::new(&format!("file://{path}"));
            let window = button.root().and_downcast::<gtk::Window>();
            launcher.launch(window.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
        });
    }

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::Center);
    actions.append(&copy_button);
    actions.append(&search_button);
    actions.append(&open_log_button);

    let column = gtk::Box::new(gtk::Orientation::Vertical, 16);
    column.set_margin_top(8);
    column.set_margin_bottom(24);
    column.set_margin_start(16);
    column.set_margin_end(16);
    column.append(&status);
    column.append(&log_scroller);
    column.append(&actions);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(700).child(&column).build())
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder().title("Build Failed").content_width(720).content_height(680).child(&toolbar).build();
    dialog.present(Some(&window));

    match &failure.build_log_path {
        Some(path) => {
            let path = path.clone();
            let log_label = log_label.clone();
            let copy_button = copy_button.clone();
            runtime::spawn_blocking(
                move || portage_store::portage::emerge::read_build_log(&path),
                move |result| match result {
                    Ok(text) => {
                        let tail_text = tail(&text, TAIL_LINES);
                        log_label.set_text(&tail_text);
                        log_label.remove_css_class("dim-label");
                        copy_button.set_sensitive(true);
                        let full_text = text;
                        copy_button.connect_clicked(move |button| {
                            button.display().clipboard().set_text(&full_text);
                        });
                    }
                    Err(err) => {
                        log_label.set_text(&format!("Couldn't read the build log: {err}"));
                    }
                },
            );
        }
        None => {
            log_label.set_text("Portage didn't report a build log location for this failure.");
        }
    }
}
