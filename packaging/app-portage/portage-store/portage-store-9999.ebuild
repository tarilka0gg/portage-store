# Copyright 1999-2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

CRATES=""
# ^ Generate this with `cargo-ebuild` (dev-util/cargo-ebuild) run against
#   this repo's Cargo.lock — 116 crates as of the version this ebuild was
#   drafted against, too many to transcribe by hand and too easy to get a
#   checksum wrong doing it manually. See packaging/README.md.

inherit cargo git-r3 xdg

DESCRIPTION="GTK4/libadwaita GUI app store for Gentoo Portage, with an optional Flatpak lane"
HOMEPAGE="https://github.com/REPLACE-ME/portage-store"
EGIT_REPO_URI="https://github.com/REPLACE-ME/portage-store.git"

LICENSE="MIT"
# ^ REPLACE-ME: set to whatever license the repo actually ships (no
#   LICENSE file existed in the tree this ebuild was drafted from).
SLOT="0"
KEYWORDS=""
IUSE="webview"

RDEPEND="
	gui-libs/libadwaita:1
	gui-libs/gtk:4
	app-portage/eix
	sys-apps/doas
	webview? ( net-libs/webkit-gtk:6 )
"
DEPEND="${RDEPEND}"
BDEPEND=""

QA_FLAGS_IGNORED="usr/bin/${PN}"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_compile() {
	cargo_src_compile $(usev webview '--features webview')
}

src_install() {
	cargo_src_install $(usev webview '--features webview')

	# The privileged helper and its passwordless doas rule are the same
	# design documented in resources/priv-helper.sh: nothing sensitive
	# ever crosses the doas call as argv/script text, only a validated
	# subcommand name and paths. Installed to /usr/libexec (not the
	# /usr/local/libexec this repo's own HELPER_PATH constant currently
	# hardcodes — see packaging/README.md's caveat before relying on this
	# ebuild as-is).
	exeinto /usr/libexec/${PN}
	doexe resources/priv-helper.sh
	newexe resources/priv-helper.sh priv-helper
	doexe resources/sandbox-build.sh

	domenu packaging/org.gentoo.PortageStore.desktop
	insinto /usr/share/metainfo
	doins packaging/org.gentoo.PortageStore.metainfo.xml
	insinto /usr/share/icons/hicolor/scalable/apps
	newins packaging/icons/org.gentoo.PortageStore.svg org.gentoo.PortageStore.svg
}

pkg_postinst() {
	xdg_pkg_postinst

	elog "portage-store needs a passwordless doas rule for its privileged"
	elog "helper before installs/USE-flag changes/etc. will work. Add a line"
	elog "like the following to /etc/doas.conf (adjust the username):"
	elog
	elog "  permit nopass YOUR_USER cmd /usr/libexec/${PN}/priv-helper"
	elog
	elog "This is deliberately not done automatically by this ebuild —"
	elog "package installation should never silently edit a system auth"
	elog "file. See resources/priv-helper.sh's own header comment for what"
	elog "the passwordless rule does and doesn't trust."
	elog
	elog "Also create the helper's log directory once, as root:"
	elog "  mkdir -p /var/log/${PN} && chown root:portage /var/log/${PN}"
	elog "  chmod 0750 /var/log/${PN}"
	elog "  : > /var/log/${PN}/priv-helper.log"
	elog "  chown root:portage /var/log/${PN}/priv-helper.log"
	elog "  chmod 0640 /var/log/${PN}/priv-helper.log"
}
