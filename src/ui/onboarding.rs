use super::{App, QueueEntry};
use crate::portage::{depclean, emerge};
use crate::ui::runtime;
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// One suggested starter package: an atom real enough to install
/// directly (every one of these — like `widgets::CURATED_BLOCKS`, which
/// several are pulled straight from — was checked with `eix --exact`
/// against the actual tree before being added here) plus a one-line
/// reason it's in this particular bundle.
struct StarterPick {
    atom: &'static str,
    blurb: &'static str,
}

struct StarterBundle {
    title: &'static str,
    description: &'static str,
    picks: &'static [StarterPick],
}

/// Four small, hand-picked starting points — not "install everything a
/// desktop/dev/gaming/server box might ever want" (several of those
/// categories already have an 18-item discovery block for that; dumping
/// all of it on someone during setup would be the opposite of a
/// considered starter set), just enough that checking a box or two
/// produces a genuinely useful first result. Every atom either comes
/// straight from `widgets::CURATED_BLOCKS` (already `eix --exact`
/// verified for the landing page) or was checked the same way here.
const STARTER_BUNDLES: &[StarterBundle] = &[
    StarterBundle {
        title: "Desktop Essentials",
        description: "A browser, an office suite, email, and a media player.",
        picks: &[
            StarterPick { atom: "www-client/firefox", blurb: "Web browser" },
            StarterPick { atom: "app-office/libreoffice", blurb: "Office suite" },
            StarterPick { atom: "mail-client/thunderbird", blurb: "Email client" },
            StarterPick { atom: "media-video/vlc", blurb: "Plays almost anything" },
        ],
    },
    StarterBundle {
        title: "Development",
        description: "Version control, an editor, and containers.",
        picks: &[
            StarterPick { atom: "dev-vcs/git", blurb: "Version control" },
            StarterPick { atom: "app-editors/vscode", blurb: "Code editor" },
            StarterPick { atom: "app-containers/docker", blurb: "Containers" },
            StarterPick { atom: "dev-util/meld", blurb: "Visual diff/merge" },
        ],
    },
    StarterBundle {
        title: "Gaming",
        description: "The two most common ways to actually launch games on Linux.",
        picks: &[
            StarterPick { atom: "games-util/steam-launcher", blurb: "Steam" },
            StarterPick { atom: "games-util/lutris", blurb: "Everything that isn't Steam" },
            StarterPick { atom: "app-emulation/wine-staging", blurb: "Windows compatibility layer" },
        ],
    },
    StarterBundle {
        title: "Server / Minimal",
        description: "The handful of tools a headless box always ends up needing.",
        picks: &[
            StarterPick { atom: "app-admin/sudo", blurb: "Privilege escalation" },
            StarterPick { atom: "sys-process/htop", blurb: "Process monitor" },
            StarterPick { atom: "app-misc/tmux", blurb: "Terminal multiplexer" },
            StarterPick { atom: "net-misc/rsync", blurb: "File sync/transfer" },
            StarterPick { atom: "app-editors/nano", blurb: "A terminal editor that needs no manual" },
        ],
    },
];

/// Opens the first-run wizard: a quick, already-real system scan (reusing
/// the exact same orphan-package check the health dashboard uses, not a
/// second implementation of it) followed by the starter bundle
/// checklist. `on_dismissed` is called exactly once, whether the wizard
/// is skipped or finished — the caller uses it to persist
/// `onboarding_shown` so this doesn't reappear on the next launch.
pub fn present(app: &Rc<App>, on_dismissed: Rc<dyn Fn()>) {
    let dialog = adw::Dialog::builder().title("Welcome to Portage Store").content_width(600).content_height(700).build();

    let column = gtk::Box::new(gtk::Orientation::Vertical, 20);
    column.set_margin_top(16);
    column.set_margin_bottom(28);
    column.set_margin_start(16);
    column.set_margin_end(16);

    let hero = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let title = gtk::Label::new(Some("Let's get this system set up"));
    title.add_css_class("title-2");
    title.set_xalign(0.0);
    title.set_wrap(true);
    let subtitle = gtk::Label::new(Some(
        "A quick look at what's already here, and a few starting points if you're not sure where to search first.",
    ));
    subtitle.add_css_class("dim-label");
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    hero.append(&title);
    hero.append(&subtitle);
    column.append(&hero);

    // --- quick scan ----------------------------------------------------
    // Reuses `app.pending_update_atoms` (already populated by the
    // startup `check_updates()` call — no second `--pretend --deep
    // @world` run just to ask the same question again) and a fresh
    // orphan-package check, the same one `health.rs`'s own dashboard
    // uses. Not a new subsystem, just this screen borrowing two answers
    // the app was already computing at startup anyway.
    let scan_group = adw::PreferencesGroup::builder().title("Quick Scan").build();
    let world_row = adw::ActionRow::builder()
        .title("Installed by You")
        .subtitle(format!("{} packages on @world", app.installed.borrow().len()))
        .build();
    world_row.add_prefix(&gtk::Image::from_icon_name("emblem-ok-symbolic"));
    scan_group.add(&world_row);

    let updates_pending = app.pending_update_atoms.borrow().len();
    let updates_row = adw::ActionRow::builder()
        .title("Updates")
        .subtitle(if updates_pending == 0 { "Up to date".to_string() } else { format!("{updates_pending} pending") })
        .build();
    updates_row.add_prefix(&gtk::Image::from_icon_name("software-update-available-symbolic"));
    scan_group.add(&updates_row);

    let orphans_row = adw::ActionRow::builder().title("Orphaned Packages").subtitle("Checking…").build();
    orphans_row.add_prefix(&gtk::Image::from_icon_name("user-trash-symbolic"));
    scan_group.add(&orphans_row);
    {
        let lines = Rc::new(RefCell::new(Vec::new()));
        let collect = lines.clone();
        let orphans_row = orphans_row.clone();
        runtime::spawn_job(depclean::pretend_job(), move |line| collect.borrow_mut().push(line), move |_success| {
            let lines = lines.borrow();
            if depclean::needs_update_first(&lines) {
                orphans_row.set_subtitle("Update first to check");
                return;
            }
            let count = depclean::parse_candidates(&lines).len();
            orphans_row.set_subtitle(&if count == 0 {
                "Nothing to clean up".to_string()
            } else {
                format!("{count} package(s) no longer needed")
            });
        });
    }
    column.append(&scan_group);

    // --- starter bundles -------------------------------------------------
    let bundles_heading = gtk::Label::new(Some("Starter Bundles"));
    bundles_heading.add_css_class("heading");
    bundles_heading.set_xalign(0.0);
    bundles_heading.set_margin_top(4);
    column.append(&bundles_heading);
    let bundles_sub = gtk::Label::new(Some("Nothing here is required — check whatever's actually useful, or none of it."));
    bundles_sub.add_css_class("dim-label");
    bundles_sub.add_css_class("caption");
    bundles_sub.set_xalign(0.0);
    column.append(&bundles_sub);

    let checkboxes: Rc<RefCell<Vec<(&'static str, gtk::CheckButton)>>> = Rc::new(RefCell::new(Vec::new()));
    for bundle in STARTER_BUNDLES {
        let group = adw::PreferencesGroup::builder().title(bundle.title).description(bundle.description).build();
        for pick in bundle.picks {
            let row = adw::ActionRow::builder().title(pick.atom).subtitle(pick.blurb).build();
            let check = gtk::CheckButton::new();
            check.set_valign(gtk::Align::Center);
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));
            group.add(&row);
            checkboxes.borrow_mut().push((pick.atom, check));
        }
        column.append(&group);
    }

    // --- actions -----------------------------------------------------
    let skip_button = gtk::Button::with_label("Skip");
    skip_button.add_css_class("pill");
    let finish_button = gtk::Button::with_label("Install Selected & Finish");
    finish_button.add_css_class("suggested-action");
    finish_button.add_css_class("pill");

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::Center);
    actions.set_margin_top(8);
    actions.append(&skip_button);
    actions.append(&finish_button);
    column.append(&actions);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&adw::Clamp::builder().maximum_size(600).child(&column).build())
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroller));
    dialog.set_child(Some(&toolbar));

    {
        let dialog = dialog.clone();
        let on_dismissed = on_dismissed.clone();
        skip_button.connect_clicked(move |_| {
            on_dismissed();
            dialog.close();
        });
    }
    {
        let app = app.clone();
        let dialog = dialog.clone();
        finish_button.connect_clicked(move |_| {
            let selected: Vec<String> =
                checkboxes.borrow().iter().filter(|(_, c)| c.is_active()).map(|(atom, _)| atom.to_string()).collect();
            if !selected.is_empty() {
                let getbinpkg = app.settings.borrow().prefer_binary_packages;
                app.enqueue(QueueEntry {
                    job: emerge::install_many_job(&selected, getbinpkg),
                    label: "Installing starter picks".to_string(),
                    mutating: true,
                    retry_with_use_fix: true,
                    known_atoms: selected.clone(),
                });
                app.toast(&format!("{} starter package(s) queued", selected.len()));
            }
            on_dismissed();
            dialog.close();
        });
    }

    dialog.present(Some(&app.window));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_starter_pick_has_no_duplicate_atom_across_bundles() {
        let mut atoms: Vec<&str> = STARTER_BUNDLES.iter().flat_map(|b| b.picks.iter().map(|p| p.atom)).collect();
        let count = atoms.len();
        atoms.sort_unstable();
        atoms.dedup();
        assert_eq!(atoms.len(), count, "a starter pick atom appears in more than one bundle");
    }

    #[test]
    fn every_starter_pick_atom_is_well_formed() {
        for bundle in STARTER_BUNDLES {
            for pick in bundle.picks {
                assert!(pick.atom.contains('/'), "{} is not a category/name atom", pick.atom);
                assert!(!pick.blurb.is_empty(), "{} has no blurb", pick.atom);
            }
        }
    }
}
