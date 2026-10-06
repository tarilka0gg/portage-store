# Packaging

- `gentoo/app-portage/portage-store/` — GURU-ready ebuilds (`0.1.1` and live `9999`), `metadata.xml`.
  `Manifest` is generated with `ebuild portage-store-0.1.1.ebuild manifest`.
- `gentoo/gen-ebuild-data.py` — prints the `CRATES` and `LICENSE` blocks from `Cargo.lock`
  (run from the repo root after `cargo update` or a version bump).
- `io.github.tarilka0gg.PortageStore.{desktop,metainfo.xml}` and `icons/` — desktop integration.

The privileged helper is installed to `/usr/libexec/portage-store/` (the path is compiled in,
see `HELPER_PATH` in `src/portage/priv_write.rs`). The ebuild never edits `/etc/doas.conf`;
`pkg_postinst` prints the rule to add.

GURU takes contributions through Codeberg pull requests or patches to
gentoo-guru@lists.gentoo.org (see the Gentoo wiki, Project:GURU/Information_for_Contributors).
