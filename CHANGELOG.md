# Changelog

All notable changes to Portage Store, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/), with
`## [version] - date` headings.

## [Unreleased]

### Changed
- README: new screenshots taken in the Simple Linux live session, each with a title and a description; a CI workflow (build and tests).

## [0.1.1] - 2026-10-06

### Added
- A dedicated Flatpak detail page with licence, developer and homepage.

### Changed
- Prepared for GURU: the privilege helper is installed to `/usr/libexec`, the app has its own app-id, the ebuilds carry the crate list and
  `RUST_MIN_VER` is 1.92.

## [0.1.0] - 2026-09-11

### Added
- A GTK4/libadwaita app store for Portage: browse by category, search (eix), package pages with upstream screenshots, USE flag editor with
  impact counts, installs and updates through a job queue with a live log and an errors-only filter.
- Flatpak as a second backend, merged by AppStream id; Update All can resume from the failed packages (`emerge --keep-going`).
- Automatic fixes for circular dependencies by relaxing USE flags; a blast-radius warning for toolchain updates.
- A config-update reviewer (with Accept All), an Env Files page, profile export and import with a diff and cherry-pick view.
- A first-run wizard with a quick scan and starter bundles; health checks with a trend history.
- A passwordless `doas`-backed privilege helper in place of `pkexec`.
