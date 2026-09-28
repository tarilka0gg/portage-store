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
    /// Well-known apps from this group Papirus/hicolor ship a real icon
    /// for — one is picked at random each time `category_tile` builds this
    /// group's tile (i.e. once per app launch, since the landing page is
    /// built once in `App::build` and never rebuilt), so the tile shows an
    /// actual logo like Steam's instead of the generic symbolic glyph, and
    /// which logo it is varies restart to restart.
    pub icon_picks: &'static [&'static str],
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
        icon_picks: &["gimp", "blender", "krita", "inkscape", "kdenlive", "audacity", "obs"],
    },
    CategoryGroup {
        name: "Work",
        icon: "applications-office-symbolic",
        categories: &["app-office", "app-text", "app-editors", "app-backup", "net-print"],
        icon_picks: &["libreoffice-writer", "libreoffice-calc", "thunderbird", "evolution", "gnucash"],
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
        icon_picks: &["steam", "lutris", "wine", "dolphin-emu", "supertuxkart", "0ad"],
    },
    CategoryGroup {
        name: "Communicate",
        icon: "applications-internet-symbolic",
        categories: &[
            "net-im", "net-mail", "net-irc", "net-voip", "net-nntp", "net-p2p",
            "net-ftp", "www-client",
        ],
        icon_picks: &["telegram-desktop", "discord", "firefox", "signal-desktop", "chromium"],
    },
    CategoryGroup {
        name: "Learn",
        icon: "applications-science-symbolic",
        categories: &[
            "sci-astronomy", "sci-biology", "sci-calculators", "sci-chemistry",
            "sci-electronics", "sci-geosciences", "sci-libs", "sci-mathematics",
            "sci-physics", "sci-visualization",
        ],
        icon_picks: &["stellarium", "kalzium", "marble", "octave", "kstars"],
    },
    CategoryGroup {
        name: "Develop",
        icon: "applications-development-symbolic",
        categories: &[
            "dev-lang", "dev-util", "dev-vcs", "dev-build", "dev-debug", "dev-libs",
            "dev-python", "dev-perl", "dev-ruby", "dev-java", "dev-db", "app-shells",
            "x11-terms",
        ],
        icon_picks: &["code", "git-cola", "android-studio", "docker", "emacs"],
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
        return real_icon_image(path, size);
    }

    fallback_icon_image(fallback_icon_name(category), size)
}

/// A real icon file, loaded and sized for a grid card.
fn real_icon_image(path: std::path::PathBuf, size: i32) -> gtk::Image {
    let image = gtk::Image::from_file(path);
    image.set_pixel_size(size);
    // Same footprint as `fallback_icon_image`, so text starts at the same
    // x whether or not the package turned out to have real artwork.
    image.set_size_request(size, size);
    image
}

/// A symbolic placeholder in the same footprint a real icon would take —
/// shared by `package_image` (a category glyph) and `flatpak_card` (a
/// generic one, since a Flatpak app has no Portage category to key off).
///
/// Sized directly rather than wrapped in a box: GtkImage already centres
/// its icon inside its own allocation. The wrapper this replaces needed an
/// expanding child to centre, and expand flags propagate *upwards* — which
/// quietly turned the icon into a stretchy column that slid by a different
/// amount on every card, staggering the whole list.
fn fallback_icon_image(icon_name: &str, size: i32) -> gtk::Image {
    let image = gtk::Image::from_icon_name(icon_name);
    image.set_pixel_size((size as f32 * 0.55) as i32);
    image.set_size_request(size, size);
    image.add_css_class("icon-fallback");
    image
}

/// Hard-caps `s` to `max_chars`, truncating the string content itself
/// rather than trusting `GtkLabel`'s own `ellipsize`/`wrap`+`max-width-chars`
/// to bound its natural width. Belt-and-suspenders: those properties
/// *should* be enough on their own, but a homogeneous `FlowBox` sizes
/// every column to its single widest child, so the one real-world ebuild
/// description with an unbroken 40+ character run (a `cross-platform/
/// cross-toolkit/cross-desktop`-shaped slash-joined phrase, no spaces for
/// wrapping to break at) was enough to blow every card in a results grid
/// out to full width and collapse the whole grid to one column — this
/// removes any dependency on Pango behaving exactly as expected for that
/// case by never handing it text long enough for the question to matter.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
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
    let title = gtk::Label::new(Some(&truncate_chars(&pkg.name, 18)));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(18);
    title.add_css_class("package-card-title");

    let subtitle = gtk::Label::new(Some(&truncate_chars(&pkg.description, 90)));
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    subtitle.set_lines(2);
    subtitle.set_max_width_chars(20);
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
    //
    // Tighter than `flatpak_card`'s row (10 vs. 14) since this row also
    // carries the installed checkmark *and* the Flatpak chip below —
    // `flatpak_card` only ever has the one trailing chip, so it needs
    // less breathing room between children to land at the same total
    // width; without this, this card was consistently wider than
    // `flatpak_card`'s despite both grids sharing the exact same column
    // budget, so a window narrow enough for the Flatpak-only section to
    // show two columns could still force the Portage results down to one.
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
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
    check.set_pixel_size(16);
    check.set_valign(gtk::Align::Center);
    check.set_tooltip_text(Some("Installed"));
    check.set_opacity(if installed.contains_key(&pkg.atom()) { 1.0 } else { 0.0 });
    row.append(&check);

    // Same "always present, just invisible" reasoning as `check` just
    // above — and for the exact same reason: `add_flatpak_chip` used to
    // *insert* this only onto cards with a confident match, which is
    // precisely the "only some cards in a grid carry this" case the
    // comment above warns about. It wasn't hypothetical: a search whose
    // results included even one confidently-matched card was enough to
    // widen every column in the whole grid and collapse it to a single
    // one. Reserving the space unconditionally and only toggling opacity
    // (`add_flatpak_chip`, below) means every card in a grid is always
    // exactly the same width regardless of which ones actually match.
    // Short badge text ("Flatpak", matching `flatpak_card`'s own chip),
    // not the longer "Also on Flatpak" this used to say — reserving space
    // for the fuller phrase on every single card (needed either way, per
    // the comment above) was, on its own, enough extra width to push
    // every results grid down to one column permanently, matched or not.
    // `set_width_chars` locks this label's own width to that many
    // characters regardless of opacity state, so revealing it never
    // changes the card's width the way switching its *text* would.
    let flatpak_chip = gtk::Label::new(Some(portage_store::backend::SourceId::Flatpak.label()));
    flatpak_chip.add_css_class("source-chip");
    flatpak_chip.set_valign(gtk::Align::Center);
    flatpak_chip.set_width_chars(7);
    flatpak_chip.set_opacity(0.0);
    row.append(&flatpak_chip);

    let button = gtk::Button::builder().child(&row).build();
    button.add_css_class("card");
    button.add_css_class("package-card");
    button.set_tooltip_text(Some(&pkg.atom()));
    (button, icon)
}

/// Reveals the "Also on Flatpak" pill `package_card` already reserved
/// space for (always present at opacity 0 — see the comment there) —
/// called only when `backend::merge_search_results` found a confident
/// match, never unconditionally, so a card never claims a Flatpak build
/// exists when the merge wasn't sure enough to say so.
pub fn add_flatpak_chip(card: &gtk::Button) {
    let Some(row) = card.child().and_downcast::<gtk::Box>() else { return };
    let Some(chip) = row.last_child().and_downcast::<gtk::Label>() else { return };
    chip.set_opacity(1.0);
}

/// A card for a Flatpak-only search hit — the "Also available via
/// Flatpak" section's own cards, built to the same shape `package_card`
/// uses for Portage results (icon, title, 2-line description, in the same
/// `grid()` FlowBox) rather than a visually separate treatment, since a
/// Flatpak-only hit is just as much a real result as a Portage one.
///
/// The icon comes from Flatpak's own local AppStream icon cache
/// (`flatpak::icon_path` — the same cache GNOME Software reads, never a
/// network fetch) when one's been cached for this remote, falling back to
/// a generic placeholder otherwise — a Flatpak app has no Portage
/// category to pick a more specific fallback glyph from.
pub fn flatpak_card(app: &portage_store::flatpak::FlatpakApp, already_installed: bool) -> gtk::Button {
    const SIZE: i32 = 64;
    let icon = match portage_store::flatpak::icon_path(&app.remote, &app.app_id) {
        Some(path) => real_icon_image(path, SIZE),
        None => fallback_icon_image("package-x-generic-symbolic", SIZE),
    };
    icon.set_valign(gtk::Align::Center);

    let title = gtk::Label::new(Some(&truncate_chars(&app.name, 20)));
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    title.set_max_width_chars(20);
    title.add_css_class("package-card-title");

    let subtitle = gtk::Label::new(Some(&truncate_chars(&app.description, 90)));
    subtitle.set_xalign(0.0);
    subtitle.set_wrap(true);
    subtitle.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    subtitle.set_lines(2);
    subtitle.set_max_width_chars(22);
    subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
    subtitle.add_css_class("dim-label");
    subtitle.add_css_class("package-card-subtitle");

    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_valign(gtk::Align::Center);
    text.set_hexpand(true);
    text.append(&title);
    text.append(&subtitle);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    row.set_halign(gtk::Align::Fill);
    row.append(&icon);
    row.append(&text);

    let chip_text = if already_installed { "Installed".to_string() } else { portage_store::backend::SourceId::Flatpak.label().to_string() };
    let chip = gtk::Label::new(Some(&chip_text));
    chip.add_css_class("source-chip");
    chip.set_valign(gtk::Align::Center);
    row.append(&chip);

    let button = gtk::Button::builder().child(&row).build();
    button.add_css_class("card");
    button.add_css_class("package-card");
    button.set_tooltip_text(Some(&app.app_id));
    // Already-installed cards aren't a "tap to install" affordance — this
    // app has no Flatpak uninstall/manage flow yet, so there's nothing
    // useful for a tap to do here. Left in the grid (not hidden) so the
    // count in the section heading still matches what's actually shown.
    button.set_sensitive(!already_installed);
    button
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

/// Picks one of `group.icon_picks` at random and resolves it to a real
/// icon file plus a representative color sampled from it. `None` if the
/// pick isn't in any installed icon theme, or the file GTK found isn't
/// something `gdk_pixbuf` can decode (an SVG with no `librsvg` loader
/// installed, for instance) — either way the caller falls back to the
/// symbolic glyph and static gradient, so a miss here is never fatal.
fn resolve_tile_icon(group: &CategoryGroup) -> Option<(std::path::PathBuf, (u8, u8, u8))> {
    if group.icon_picks.is_empty() {
        return None;
    }
    let pick_index = random_range(0, group.icon_picks.len() as u32 - 1) as usize;
    let path = icons::resolve_by_name(group.icon_picks[pick_index])?;
    let color = dominant_color(&path)?;
    Some((path, color))
}

/// Averages the non-transparent, non-extreme pixels of a (downscaled)
/// icon into a single RGB color — not a real dominant-color/quantization
/// algorithm, just enough to give the tile background a tint that
/// actually resembles its icon (Steam's blue, GIMP's grey-and-orange,
/// ...) rather than a fixed palette that ignores which icon ended up
/// there. Near-white/near-black/near-transparent pixels are skipped since
/// most icons sit on a transparent or plain background that would
/// otherwise wash the average toward grey.
fn dominant_color(path: &std::path::Path) -> Option<(u8, u8, u8)> {
    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file_at_size(path, 24, 24).ok()?;
    let width = pixbuf.width();
    let height = pixbuf.height();
    if width <= 0 || height <= 0 {
        return None;
    }
    let n_channels = pixbuf.n_channels() as usize;
    let has_alpha = pixbuf.has_alpha();
    let rowstride = pixbuf.rowstride() as usize;
    // Safe here despite the `unsafe` signature: this `Pixbuf` was just
    // created above and never shared with anything else, so nothing else
    // can be racing this read-only scan of its pixel buffer.
    let pixels = unsafe { pixbuf.pixels() };

    let (mut r_sum, mut g_sum, mut b_sum, mut count) = (0u64, 0u64, 0u64, 0u64);
    for y in 0..height as usize {
        let Some(row) = pixels.get(y * rowstride..) else { continue };
        for x in 0..width as usize {
            let Some(px) = row.get(x * n_channels..x * n_channels + n_channels) else { continue };
            if has_alpha && px[3] < 32 {
                continue;
            }
            let (r, g, b) = (px[0] as u32, px[1] as u32, px[2] as u32);
            let is_extreme = (r > 235 && g > 235 && b > 235) || (r < 20 && g < 20 && b < 20);
            if is_extreme {
                continue;
            }
            r_sum += r as u64;
            g_sum += g as u64;
            b_sum += b as u64;
            count += 1;
        }
    }
    (count > 0).then(|| ((r_sum / count) as u8, (g_sum / count) as u8, (b_sum / count) as u8))
}

/// Turns a sampled icon color into the tile's two gradient stops. Icon
/// colors run lighter, on average, than this app's existing hand-picked
/// tile gradients — icons are drawn to read against either a light or a
/// dark desktop background, while these tiles are always dark with a
/// white label — so pale source colors get scaled down harder to keep
/// that label legible no matter which icon was picked.
fn tile_gradient_stops((r, g, b): (u8, u8, u8)) -> ((u8, u8, u8), (u8, u8, u8)) {
    let luminance = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    let scale = if luminance > 150.0 { 0.55 } else { 0.8 };
    let base = ((r as f32 * scale) as u8, (g as f32 * scale) as u8, (b as f32 * scale) as u8);
    let dark = ((base.0 as f32 * 0.6) as u8, (base.1 as f32 * 0.6) as u8, (base.2 as f32 * 0.6) as u8);
    (base, dark)
}

/// The CSS rule giving tile `index` its icon-derived gradient — a unique
/// class per tile (`category-tile-dyn-N`) rather than reusing the static
/// `category-tile-N` classes in `style.css`, so a tile whose icon lookup
/// failed keeps using the static, hand-picked gradient instead of losing
/// its background entirely.
fn tile_gradient_css(index: usize, color: (u8, u8, u8)) -> String {
    let (base, dark) = tile_gradient_stops(color);
    format!(
        ".category-tile-dyn-{index} {{ background-image: linear-gradient(160deg, #{:02x}{:02x}{:02x}, #{:02x}{:02x}{:02x}); }}\n",
        base.0, base.1, base.2, dark.0, dark.1, dark.2
    )
}

/// Registers generated CSS (the icon-derived tile gradients) with the
/// default display, same mechanism `main.rs` uses for the static
/// stylesheet — a second provider stacks fine at the same priority since
/// the two never define the same class.
fn apply_dynamic_css(css: &str) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    }
}

/// A category tile for the landing page: an icon-derived gradient behind a
/// real app icon (e.g. Steam's logo for Games) when one of the group's
/// `icon_picks` resolves, falling back to the static hand-picked gradient
/// and a symbolic glyph otherwise. Which pick wins is randomized once per
/// call, and this is only ever called once per group at startup (see
/// `App::build`), so the icon — and its tile color — varies from one
/// launch to the next but stays put for the rest of the session.
pub fn category_tile(index: usize, group: &CategoryGroup) -> gtk::Button {
    let resolved = resolve_tile_icon(group);

    let icon = match &resolved {
        Some((path, _)) => {
            let image = gtk::Image::from_file(path);
            image.set_pixel_size(32);
            image
        }
        None => papirus_symbolic_icon(group.icon).unwrap_or_else(|| gtk::Image::from_icon_name(group.icon)),
    };
    let label = gtk::Label::new(Some(group.name));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_halign(gtk::Align::Center);
    row.append(&icon);
    row.append(&label);

    let button = gtk::Button::builder().child(&row).build();
    button.add_css_class("category-tile");
    match resolved {
        Some((_, color)) => {
            apply_dynamic_css(&tile_gradient_css(index, color));
            button.add_css_class(&format!("category-tile-dyn-{index}"));
        }
        None => button.add_css_class(&format!("category-tile-{}", index % CATEGORY_GROUPS.len())),
    }
    button
}

/// A responsive grid that reflows from 2 columns down to 1 as the window
/// narrows — used for every card grid in the app (Explore tiles, search
/// results, the Flatpak-only section, Installed), so all of them share
/// exactly the same layout instead of drifting apart.
pub fn grid() -> gtk::FlowBox {
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_max_children_per_line(2);
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
    fn truncate_chars_leaves_short_strings_untouched() {
        assert_eq!(truncate_chars("firefox", 20), "firefox");
        assert_eq!(truncate_chars("exactly-ten", 11), "exactly-ten");
    }

    #[test]
    fn truncate_chars_cuts_long_strings_with_an_ellipsis() {
        // 20 real characters kept, then one more slot spent on the
        // ellipsis itself — the whole point is the *result* never exceeds
        // `max_chars`, not that exactly `max_chars` of original text
        // survives.
        let truncated = truncate_chars("gnome-shell-extension-desktop-icons-ng", 20);
        assert_eq!(truncated.chars().count(), 20);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated, "gnome-shell-extensi…");
    }

    #[test]
    fn truncate_chars_handles_one_unbroken_run_with_no_spaces() {
        // The exact real-world shape that used to defeat GtkLabel's own
        // wrap/ellipsize sizing and collapse a whole results grid to one
        // column — see `truncate_chars`'s own doc comment.
        let truncated = truncate_chars("cross-platform/cross-toolkit/cross-desktop", 22);
        assert_eq!(truncated.chars().count(), 22);
    }

    #[test]
    fn truncate_chars_is_a_no_op_exactly_at_the_boundary() {
        let s = "a".repeat(20);
        assert_eq!(truncate_chars(&s, 20), s);
    }

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
