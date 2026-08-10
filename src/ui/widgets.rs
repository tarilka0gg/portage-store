use portage_store::portage::eix::PackageSummary;
use portage_store::portage::icons;
use portage_store::portage::installed::InstalledPackage;
use adw::prelude::*;
use std::collections::HashMap;

/// One named section of the landing page's featured area: a theme (Social,
/// Games, Create, ...) and every popular pick in it, in the App Store /
/// GNOME Software "curated" style — distinct from `CATEGORY_GROUPS` below,
/// which drives the tile grid for *browsing*. This is for *discovery*.
/// Rendered as its own carousel, paginated 3x2 (six per page), so a theme
/// isn't capped at six picks — it just gets more pages. Every atom here
/// was checked with `eix --exact` to actually exist in the tree before
/// being added.
#[derive(Clone, Copy)]
pub struct CuratedBlock {
    pub title: &'static str,
    pub atoms: &'static [&'static str],
    /// Whether this block is made of names a general audience would
    /// recognize on sight. `order_blocks_popular_first` uses this to keep
    /// mainstream picks on the carousel's early pages and push more
    /// niche/CLI-audience blocks (like "New & Updated") later, so the very
    /// first thing shown is never an unfamiliar name.
    pub popular: bool,
}

pub const CURATED_BLOCKS: &[CuratedBlock] = &[
    CuratedBlock {
        title: "Social",
        popular: true,
        atoms: &[
            "net-im/telegram-desktop",
            "net-im/discord",
            "www-client/firefox",
            "www-client/zen-bin",
            "www-client/chromium",
            "net-im/signal-desktop-bin",
            "net-im/element-desktop",
            "net-im/pidgin",
            "www-client/vivaldi",
            "www-client/opera",
            "net-im/qtox",
            "net-im/gajim",
            "net-im/dino",
            "www-client/qutebrowser",
            "www-client/falkon",
            "www-client/epiphany",
            "net-im/nheko",
            "net-im/teams-for-linux",
        ],
    },
    CuratedBlock {
        title: "Games",
        popular: true,
        atoms: &[
            "games-util/steam-launcher",
            "app-emulation/wine-staging",
            "games-util/lutris",
            "games-action/supertuxkart",
            "games-strategy/0ad",
            "games-emulation/RetroArch",
            "games-emulation/dolphin",
            "games-fps/xonotic",
            "games-emulation/pcsx2",
            "games-emulation/ppsspp",
            "games-strategy/freeciv",
            "games-strategy/wesnoth",
            "games-strategy/warzone2100",
            "games-puzzle/hexalate",
            "games-arcade/supertux",
            "games-emulation/mgba",
            "games-puzzle/pingus",
            "games-action/teeworlds",
        ],
    },
    CuratedBlock {
        title: "Create",
        popular: true,
        atoms: &[
            "app-editors/micro",
            "app-editors/neovim",
            "kde-apps/kdenlive",
            "app-editors/zed",
            "media-gfx/gimp",
            "media-video/obs-studio",
            "media-gfx/darktable",
            "media-video/shotcut",
            "media-gfx/inkscape",
            "media-gfx/krita",
            "media-gfx/blender",
            "app-office/scribus",
            "media-gfx/rawtherapee",
            "app-editors/gedit",
            "media-gfx/pinta",
            "app-editors/gnome-text-editor",
            "media-sound/ardour",
            "media-gfx/imagemagick",
        ],
    },
    CuratedBlock {
        title: "Development",
        popular: true,
        atoms: &[
            "dev-vcs/git",
            "app-editors/vscode",
            "dev-util/android-studio",
            "app-editors/sublime-text",
            "dev-util/meld",
            "app-containers/docker",
            "app-containers/podman",
            "dev-db/dbeaver-bin",
            "app-emulation/virtualbox",
            "app-emulation/qemu",
            "dev-vcs/git-cola",
            "net-analyzer/wireshark",
            "app-editors/emacs",
            "dev-vcs/tig",
            "app-emulation/libvirt",
            "app-editors/gvim",
            "dev-vcs/subversion",
            "net-analyzer/nmap",
        ],
    },
    CuratedBlock {
        title: "Multimedia",
        popular: true,
        atoms: &[
            "media-video/vlc",
            "media-sound/audacity",
            "media-sound/spotify",
            "media-sound/audacious",
            "media-video/handbrake",
            "media-video/mpv",
            "media-sound/deadbeef",
            "media-sound/lollypop",
            "media-tv/kodi",
            "media-video/celluloid",
            "media-sound/strawberry",
            "media-video/smplayer",
            "media-video/ffmpeg",
            "media-sound/cmus",
            "media-gfx/feh",
            "media-video/pitivi",
            "media-sound/qmmp",
            "media-video/avidemux",
        ],
    },
    CuratedBlock {
        title: "Productivity",
        popular: true,
        atoms: &[
            "app-office/libreoffice",
            "app-text/xournalpp",
            "app-office/onlyoffice-bin",
            "mail-client/thunderbird",
            "app-text/calibre",
            "mail-client/geary",
            "mail-client/evolution",
            "app-office/gnucash",
            "net-misc/nextcloud-client",
            "app-office/planner",
            "app-office/gnumeric",
            "app-text/tesseract",
            "app-office/wps-office",
            "mail-client/claws-mail",
            "mail-client/mailspring-bin",
            "net-misc/remmina",
            "app-misc/task",
        ],
    },
    CuratedBlock {
        title: "New & Updated",
        popular: false,
        atoms: &[
            "net-p2p/qbittorrent",
            "app-crypt/gnupg",
            "sys-process/htop",
            "x11-terms/alacritty",
            "app-editors/helix",
            "sys-apps/ripgrep",
            "app-shells/starship",
            "app-misc/tmux",
            "x11-terms/kitty",
            "sys-apps/eza",
            "sys-fs/ncdu",
            "app-misc/tealdeer",
        ],
    },
];

/// Shuffles `CURATED_BLOCKS` but keeps every `popular` block ahead of every
/// non-popular one, so the carousel's first pages are always mainstream
/// names and pages like "New & Updated" — full of CLI tools a casual user
/// won't recognize — never end up shown first. `sort_by_key` is a stable
/// sort, so the shuffle order survives within each of the two groups.
pub fn order_blocks_popular_first(blocks: &[CuratedBlock]) -> Vec<CuratedBlock> {
    let mut blocks = shuffled(blocks);
    blocks.sort_by_key(|block| !block.popular);
    blocks
}

/// A small xorshift PRNG seeded from wall-clock time, purely to reorder a
/// fixed 6-item list differently on each launch — not worth a real `rand`
/// dependency for, the same call this codebase already made for its
/// Markdown stripping and edit-distance ranking rather than pulling in a
/// crate for a few dozen lines of logic.
fn next_random(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// A fresh xorshift seed, mixing wall-clock time with a call counter.
/// Time alone repeats for calls that land in the same tick of the clock's
/// resolution — which matters here, since several `CuratedBlock`s'
/// carousels get built back-to-back within the same event-loop iteration
/// at startup; without the counter they could all draw the *same*
/// "random" seed and end up perfectly synchronized instead of staggered.
fn next_seed() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CALLS: AtomicU64 = AtomicU64::new(0);
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x2545F4914F6CDD1D);
    (time ^ call.wrapping_mul(0x9E3779B97F4A7C15)) | 1 // xorshift is undefined on a zero seed
}

/// Fisher–Yates shuffle of a fresh copy of `items`, seeded from the
/// current time so the curated rows land in a different order each time
/// the app starts, without needing to persist or reuse a seed between runs.
pub fn shuffled<T: Clone>(items: &[T]) -> Vec<T> {
    let mut state = next_seed();
    let mut items = items.to_vec();
    for i in (1..items.len()).rev() {
        let j = (next_random(&mut state) as usize) % (i + 1);
        items.swap(i, j);
    }
    items
}

/// A pseudo-random integer in `min..=max` — used to stagger the featured
/// carousels' auto-advance timers so they don't all tick in lockstep.
pub fn random_range(min: u32, max: u32) -> u32 {
    let span = u64::from(max - min + 1);
    let mut state = next_seed();
    min + (next_random(&mut state) % span) as u32
}

/// Curated, human-facing groupings over Portage's ~180 raw categories, each
/// with the symbolic icon and gradient index used for its tile. Nobody
/// browsing for a package thinks in terms of `kde-frameworks` vs
/// `kde-plasma`; they think "games" or "desktop stuff".
pub struct CategoryGroup {
    pub name: &'static str,
    pub icon: &'static str,
    pub categories: &'static [&'static str],
}

/// The six activity-shaped groups GNOME Software uses. They're framed by
/// what you want to *do*, not by where portage happens to file the ebuild,
/// so each one spans several raw categories.
pub const CATEGORY_GROUPS: &[CategoryGroup] = &[
    CategoryGroup {
        name: "Create",
        icon: "applications-graphics-symbolic",
        categories: &[
            "media-gfx", "media-sound", "media-video", "media-plugins", "media-tv",
            "media-radio", "media-fonts",
        ],
    },
    CategoryGroup {
        name: "Work",
        icon: "applications-office-symbolic",
        categories: &["app-office", "app-text", "app-editors", "app-backup", "net-print"],
    },
    CategoryGroup {
        name: "Games",
        icon: "applications-games-symbolic",
        categories: &[
            "games-action", "games-arcade", "games-board", "games-emulation",
            "games-engines", "games-fps", "games-kids", "games-misc", "games-mud",
            "games-puzzle", "games-roguelike", "games-rpg", "games-server",
            "games-simulation", "games-sports", "games-strategy", "games-util",
            "app-emulation",
        ],
    },
    CategoryGroup {
        name: "Communicate",
        icon: "applications-internet-symbolic",
        categories: &[
            "net-im", "net-mail", "net-irc", "net-voip", "net-nntp", "net-p2p",
            "net-ftp", "www-client",
        ],
    },
    CategoryGroup {
        name: "Learn",
        icon: "applications-science-symbolic",
        categories: &[
            "sci-astronomy", "sci-biology", "sci-calculators", "sci-chemistry",
            "sci-electronics", "sci-geosciences", "sci-libs", "sci-mathematics",
            "sci-physics", "sci-visualization",
        ],
    },
    CategoryGroup {
        name: "Develop",
        icon: "applications-development-symbolic",
        categories: &[
            "dev-lang", "dev-util", "dev-vcs", "dev-build", "dev-debug", "dev-libs",
            "dev-python", "dev-perl", "dev-ruby", "dev-java", "dev-db", "app-shells",
            "x11-terms",
        ],
    },
];

/// Symbolic icon to fall back to when a package has no real artwork.
/// Keyed off the raw category prefix rather than the browse groups above,
/// because those only cover the six activities offered on the landing page
/// while packages from every category in the tree can show up in search.
fn fallback_icon_name(category: &str) -> &'static str {
    match category.split('-').next().unwrap_or("") {
        "games" => "applications-games-symbolic",
        "media" => "applications-multimedia-symbolic",
        "dev" => "applications-engineering-symbolic",
        "sci" => "applications-science-symbolic",
        "www" | "mail" => "web-browser-symbolic",
        "net" => "network-server-symbolic",
        "sys" => "applications-system-symbolic",
        "kde" | "gnome" | "xfce" | "lxqt" | "mate" | "x11" => "computer-symbolic",
        "acct" => "system-users-symbolic",
        "virtual" => "package-x-generic-symbolic",
        "app" => match category {
            "app-office" | "app-text" | "app-editors" => "accessories-text-editor-symbolic",
            "app-shells" => "utilities-terminal-symbolic",
            "app-crypt" | "app-forensics" => "security-high-symbolic",
            "app-emulation" => "computer-symbolic",
            _ => "applications-utilities-symbolic",
        },
        _ => "package-x-generic-symbolic",
    }
}

/// The package's real icon when we can find one — the `.desktop` icon of an
/// installed app, or a name match in the Papirus icon set (which covers
/// popular apps even before they're installed) — otherwise a symbolic
/// placeholder in a rounded tile so grids stay aligned.
pub fn package_image(
    category: &str,
    name: &str,
    installed_icons: &HashMap<String, std::path::PathBuf>,
    size: i32,
) -> gtk::Image {
    let atom = format!("{category}/{name}");
    let resolved = installed_icons
        .get(&atom)
        .cloned()
        .or_else(|| icons::resolve_cached(name));

    if let Some(path) = resolved {
        let image = gtk::Image::from_file(path);
        image.set_pixel_size(size);
        // Same footprint as the fallback below, so text starts at the same
        // x whether or not the package turned out to have real artwork.
        image.set_size_request(size, size);
        return image;
    }

    // Sized directly rather than wrapped in a box: GtkImage already centres
    // its icon inside its own allocation. The wrapper this replaces needed
    // an expanding child to centre, and expand flags propagate *upwards* —
    // which quietly turned the icon into a stretchy column that slid by a
    // different amount on every card, staggering the whole list.
    let image = gtk::Image::from_icon_name(fallback_icon_name(category));
    image.set_pixel_size((size as f32 * 0.55) as i32);
    image.set_size_request(size, size);
    image.add_css_class("icon-fallback");
    image
}

/// One clickable app card in a browse grid: icon, name, one-line summary —
/// the same shape GNOME Software uses for its "Editor's Choice" rows.
///
/// Also hands back the icon `gtk::Image` itself (not just the button), so a
/// caller that later fetches real artwork in the background — the featured
/// carousel does this for curated picks — can swap it in without rebuilding
/// the whole card.
pub fn package_card(
    pkg: &PackageSummary,
    installed: &HashMap<String, InstalledPackage>,
    installed_icons: &HashMap<String, std::path::PathBuf>,
) -> (gtk::Button, gtk::Image) {
    let icon = package_image(&pkg.category, &pkg.name, installed_icons, 64);
    icon.set_valign(gtk::Align::Center);

    // The character caps matter for layout, not just aesthetics: FlowBox
    // sizes its columns from each child's *natural* width, so an unbounded
    // one-line description would make every card demand the full row and
    // collapse the grid to a single column.
    let title = gtk::Label::new(Some(&pkg.name));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(20);
    title.add_css_class("package-card-title");

    let subtitle = gtk::Label::new(Some(&pkg.description));
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    subtitle.set_lines(2);
    subtitle.set_max_width_chars(22);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subtitle.add_css_class("dim-label");
    // `set_lines(2)` only caps wrapping at 2 lines, it doesn't reserve
    // space for a second line that isn't there — so a card with a short,
    // one-line description ends up shorter than one whose description
    // wraps. Inside a homogeneous FlowBox that's invisible (every card in
    // the page matches the tallest one), but across the featured
    // carousel's pages it isn't: each page is sized to its own tallest
    // card, so swiping between a page of short descriptions and one of
    // long descriptions visibly changes the carousel's height. Reserving
    // 2 lines' worth of height unconditionally keeps every card, and so
    // every page, the same height.
    subtitle.add_css_class("package-card-subtitle");

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    text.append(&title);
    text.append(&subtitle);

    // Fill, not the button's default centring: with centred content each
    // card's icon starts wherever its own text happens to end, so a column
    // of cards comes out visibly staggered.
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    row.set_halign(gtk::Align::Fill);
    row.append(&icon);
    row.append(&text);

    // Always present and always taking up its space — just invisible via
    // opacity when not installed, rather than only appended conditionally
    // (`set_visible(false)` would remove it from layout entirely, which
    // has the same problem) — FlowBox is homogeneous, so every card in a
    // grid is sized to the *widest* one. If only some cards in a grid
    // happened to carry this icon, that grid would end up wider than one
    // where none (or all) of them do, and a grid landing just over the
    // three-column threshold would silently fall back to two.
    let check = gtk::Image::from_icon_name("object-select-symbolic");
    check.set_valign(gtk::Align::Center);
    check.set_tooltip_text(Some("Installed"));
    check.set_opacity(if installed.contains_key(&pkg.atom()) { 1.0 } else { 0.0 });
    row.append(&check);

    let button = gtk::Button::builder().child(&row).build();
    button.add_css_class("card");
    button.add_css_class("package-card");
    button.set_tooltip_text(Some(&pkg.atom()));
    (button, icon)
}

/// Adds a small "Also on Flatpak" pill to an already-built `package_card`
/// — purely additive metadata on a card that's still, fundamentally, the
/// Portage result. Called only when `backend::merge_search_results` found
/// a confident match, never unconditionally, so a card never claims a
/// Flatpak build exists when the merge wasn't sure enough to say so.
pub fn add_flatpak_chip(card: &gtk::Button) {
    let Some(row) = card.child().and_downcast::<gtk::Box>() else { return };
    let chip = gtk::Label::new(Some(&format!("Also on {}", portage_store::backend::SourceId::Flatpak.label())));
    chip.add_css_class("source-chip");
    chip.set_valign(gtk::Align::Center);
    row.insert_child_after(&chip, row.last_child().as_ref());
}

/// A card for a Flatpak-only search hit — the "Also available via
/// Flatpak" section's own rows, which never had a Portage result to piggy
/// back a chip onto. Deliberately a plainer `ActionRow`, not a full grid
/// card: this section is meant to read as a secondary, optional list
/// tucked below the real (Portage) results, not visually competing with
/// them for attention.
pub fn flatpak_only_row(app: &portage_store::flatpak::FlatpakApp, already_installed: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(&app.name).subtitle(&app.description).build();
    row.add_prefix(&gtk::Image::from_icon_name("package-x-generic-symbolic"));
    let chip_text = if already_installed { "Installed".to_string() } else { portage_store::backend::SourceId::Flatpak.label().to_string() };
    let chip = gtk::Label::new(Some(&chip_text));
    chip.add_css_class("source-chip");
    chip.set_valign(gtk::Align::Center);
    row.add_suffix(&chip);
    // Already-installed rows aren't a "tap to install" affordance —
    // this pass has no uninstall/manage flow for Flatpak apps yet, so
    // there's nothing useful for a tap to do here.
    row.set_activatable(!already_installed);
    row
}

/// Loads `name` straight from Papirus's own symbolic category icons rather
/// than through `gtk::Image::from_icon_name`, which resolves against
/// whichever icon theme is *actually active* on the desktop — Papirus
/// ships these under a nonstandard `{size}/symbolic/{context}/` nesting
/// (rather than plain `{size}/{context}/`), so it isn't reliably found via
/// GTK's normal theme fallback chain even when Papirus is installed.
/// `IconPaintable::for_file` plus forcing `is_symbolic` keeps the same
/// automatic recolor-to-currentColor behavior `from_icon_name` gives
/// symbolic icons — it's the same paintable type either way, just pointed
/// at an explicit file instead of a theme lookup.
fn papirus_symbolic_icon(name: &str) -> Option<gtk::Image> {
    let path = format!("/usr/share/icons/Papirus/32x32/symbolic/categories/{name}.svg");
    if !std::path::Path::new(&path).exists() {
        return None;
    }
    let file = gtk::gio::File::for_path(&path);
    let paintable = gtk::IconPaintable::for_file(&file, 32, 1);
    paintable.set_is_symbolic(true);
    Some(gtk::Image::from_paintable(Some(&paintable)))
}

/// A gradient category tile for the landing page.
pub fn category_tile(index: usize, group: &CategoryGroup) -> gtk::Button {
    let icon = papirus_symbolic_icon(group.icon).unwrap_or_else(|| gtk::Image::from_icon_name(group.icon));
    let label = gtk::Label::new(Some(group.name));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_halign(gtk::Align::Center);
    row.append(&icon);
    row.append(&label);

    let button = gtk::Button::builder().child(&row).build();
    button.add_css_class("category-tile");
    button.add_css_class(&format!("category-tile-{}", index % CATEGORY_GROUPS.len()));
    button
}

/// A responsive grid that reflows from 3 columns down to 1 as the window
/// narrows, matching GNOME Software's behaviour.
pub fn grid() -> gtk::FlowBox {
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_max_children_per_line(3);
    flow.set_min_children_per_line(1);
    flow.set_column_spacing(12);
    flow.set_row_spacing(12);
    // Without this the flow box soaks up all the leftover height of the
    // page and hands it to its rows, inflating every card to hundreds of
    // pixels tall when there are only a handful of results.
    flow.set_valign(gtk::Align::Start);
    flow
}

pub fn section_heading(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.add_css_class("section-heading");
    label
}

pub fn clear(flow: &gtk::FlowBox) {
    while let Some(child) = flow.first_child() {
        flow.remove(&child);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_range_stays_within_bounds() {
        for _ in 0..200 {
            let n = random_range(4, 7);
            assert!((4..=7).contains(&n), "{n} outside 4..=7");
        }
    }

    #[test]
    fn random_range_can_return_a_single_value() {
        assert_eq!(random_range(3, 3), 3);
    }

    #[test]
    fn shuffled_preserves_every_element() {
        let original = [1, 2, 3, 4, 5, 6];
        let mut shuffled_copy = shuffled(&original);
        shuffled_copy.sort();
        assert_eq!(shuffled_copy, original);
    }

    #[test]
    fn shuffled_does_not_mutate_the_input() {
        let original = ["a", "b", "c"];
        let _ = shuffled(&original);
        assert_eq!(original, ["a", "b", "c"]);
    }

    #[test]
    fn popular_blocks_always_sort_ahead_of_non_popular_ones() {
        for _ in 0..20 {
            let ordered = order_blocks_popular_first(CURATED_BLOCKS);
            let first_non_popular = ordered.iter().position(|b| !b.popular);
            let last_popular = ordered.iter().rposition(|b| b.popular);
            if let (Some(first_non_popular), Some(last_popular)) = (first_non_popular, last_popular) {
                assert!(last_popular < first_non_popular, "a non-popular block sorted ahead of a popular one");
            }
        }
    }

    #[test]
    fn curated_atoms_are_well_formed_category_slash_name() {
        for block in CURATED_BLOCKS {
            for atom in block.atoms {
                assert!(atom.contains('/'), "{atom} in {} is missing a category", block.title);
            }
        }
    }

    #[test]
    fn curated_atoms_have_no_duplicates() {
        // A repeated atom would show the same package twice in the
        // featured carousel — a real bug, not just untidy data.
        let all_atoms: Vec<&str> = CURATED_BLOCKS.iter().flat_map(|b| b.atoms.iter().copied()).collect();
        let mut sorted = all_atoms.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all_atoms.len(), "CURATED_BLOCKS has a duplicate atom across blocks");
    }

    #[test]
    fn every_curated_block_has_at_least_six_picks() {
        // Six is one full 3x2 carousel page; below that a section would
        // render with obviously empty grid slots.
        for block in CURATED_BLOCKS {
            assert!(block.atoms.len() >= 6, "{} has fewer than 6 picks", block.title);
        }
    }
}
