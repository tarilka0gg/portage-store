//! A terminal companion to the GUI, sharing `portage_store`'s domain
//! layer wholesale rather than reimplementing any of it — every
//! subcommand below is a thin wrapper over a function the GUI already
//! calls (`portage::eix::search`, `portage::emerge::install_job`, ...),
//! and every mutating command streams through the exact same
//! `portage::emerge::run` the GUI's job queue does, including the same
//! passwordless-`doas`-to-`priv-helper` path — there is no second
//! privilege mechanism here.

use clap::{Parser, Subcommand};
use portage_store::portage::{emerge, eix, installed, preset, sync};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "portage-store-cli", about = "Terminal companion to Portage Store", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Search the portage tree by name/description substring.
    Search { query: String },
    /// Show details for a single package.
    Show { atom: String },
    /// List every installed package.
    ListInstalled,
    /// Show pending @world updates.
    Updates,
    /// Install (or update) a package.
    Install {
        atom: String,
        /// Prefer a prebuilt binary package over compiling from source.
        #[arg(long, default_value_t = true)]
        binpkg: bool,
        /// Show what would happen without changing anything.
        #[arg(long)]
        pretend: bool,
    },
    /// Uninstall a package.
    Uninstall { atom: String },
    /// Update the whole @world set.
    UpdateWorld {
        #[arg(long, default_value_t = true)]
        binpkg: bool,
        #[arg(long)]
        pretend: bool,
    },
    /// Sync the package tree (emerge --sync).
    Sync,
    /// Shareable USE-flag + starter-package presets.
    Preset {
        #[command(subcommand)]
        command: PresetCommand,
    },
}

#[derive(Subcommand)]
enum PresetCommand {
    /// List built-in presets.
    List,
    /// Apply a built-in preset by name: sets its USE flags, then queues
    /// its packages for install.
    Apply {
        name: String,
        #[arg(long, default_value_t = true)]
        binpkg: bool,
    },
    /// Apply a preset from an exported JSON file.
    ApplyFile {
        path: PathBuf,
        #[arg(long, default_value_t = true)]
        binpkg: bool,
    },
}

fn print_summary(pkg: &eix::PackageSummary) {
    println!("{}  {}", pkg.atom(), pkg.latest_version);
    if !pkg.description.is_empty() {
        println!("    {}", pkg.description);
    }
}

/// Runs `job` to completion, printing every line as it arrives and
/// returning whether it succeeded — the CLI's equivalent of
/// `ui::queue::start_next`'s job-completion handling, minus the widgets.
async fn run_job(label: &str, job: emerge::Job) -> bool {
    println!("=== {label} ===");
    let (tx, rx) = async_channel::unbounded();
    let handle = tokio::spawn(emerge::run(job, tx));
    let mut success = false;
    while let Ok(event) = rx.recv().await {
        match event {
            emerge::EmergeEvent::Line(line) => println!("{line}"),
            emerge::EmergeEvent::Finished { success: s } => success = s,
            emerge::EmergeEvent::FailedToStart(err) => {
                eprintln!("failed to start: {err}");
                success = false;
            }
        }
    }
    let _ = handle.await;
    success
}

fn find_builtin_preset(name: &str) -> Option<&'static preset::Preset> {
    preset::BUILTIN_PRESETS.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// Sets every USE flag in `owned`, then runs its install job — shared by
/// both `preset apply` and `preset apply-file`.
async fn apply_owned_preset(owned: &preset::OwnedPreset, getbinpkg: bool) -> bool {
    println!("Applying {} USE flag(s) for '{}'…", owned.use_flags.len(), owned.name);
    if let Err(err) = preset::apply_use_flags(owned) {
        eprintln!("failed to apply USE flags: {err}");
        return false;
    }
    if owned.packages.is_empty() {
        return true;
    }
    run_job(&format!("Installing '{}' packages", owned.name), preset::install_job(owned, getbinpkg)).await
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match cli.command {
        Command::Search { query } => match eix::search(&query) {
            Ok(results) if results.is_empty() => {
                println!("No matches for '{query}'.");
            }
            Ok(results) => {
                for pkg in &results {
                    print_summary(pkg);
                }
            }
            Err(err) => {
                eprintln!("search failed: {err}");
                return ExitCode::FAILURE;
            }
        },

        Command::Show { atom } => match eix::lookup(&atom) {
            Ok(Some(pkg)) => {
                println!("{}", pkg.atom());
                println!("  Version:     {}", pkg.latest_version);
                println!("  Description: {}", pkg.description);
                println!("  Homepage:    {}", pkg.homepage);
                println!("  License:     {}", pkg.license);
                if let Some(slot) = pkg.slot_label() {
                    println!("  Slot:        {slot}");
                }
                println!("  Masked:      {}", pkg.masked);
                if let Some(overlay) = &pkg.overlay {
                    println!("  Overlay:     {overlay}");
                }
                if !pkg.iuse.is_empty() {
                    let flags: Vec<String> =
                        pkg.iuse.iter().map(|f| if f.default_enabled { format!("+{}", f.name) } else { f.name.clone() }).collect();
                    println!("  IUSE:        {}", flags.join(" "));
                }
            }
            Ok(None) => {
                println!("'{atom}' not found.");
                return ExitCode::FAILURE;
            }
            Err(err) => {
                eprintln!("lookup failed: {err}");
                return ExitCode::FAILURE;
            }
        },

        Command::ListInstalled => match installed::scan() {
            Ok(mut packages) => {
                packages.sort_by(|a, b| (&a.category, &a.name).cmp(&(&b.category, &b.name)));
                for pkg in &packages {
                    println!("{}/{}-{}", pkg.category, pkg.name, pkg.version);
                }
            }
            Err(err) => {
                eprintln!("failed to scan installed packages: {err}");
                return ExitCode::FAILURE;
            }
        },

        Command::Updates => {
            if !run_job("Checking for updates", emerge::pretend_world_job(true)).await {
                // A pretend run's own failure (a resolver conflict, etc.)
                // still leaves useful partial output above — not a reason
                // to hide it, but still worth a non-zero exit.
                return ExitCode::FAILURE;
            }
        }

        Command::Install { atom, binpkg, pretend } => {
            let job = if pretend { emerge::pretend_install_job(&atom, binpkg) } else { emerge::install_job(&atom, binpkg, false) };
            if !run_job(&format!("Installing {atom}"), job).await {
                return ExitCode::FAILURE;
            }
        }

        Command::Uninstall { atom } => {
            if !run_job(&format!("Removing {atom}"), emerge::uninstall_job(&atom)).await {
                return ExitCode::FAILURE;
            }
        }

        Command::UpdateWorld { binpkg, pretend } => {
            let job = if pretend { emerge::pretend_world_job(binpkg) } else { emerge::update_world_job(binpkg) };
            if !run_job("System update (@world)", job).await {
                return ExitCode::FAILURE;
            }
        }

        Command::Sync => {
            if !run_job("Syncing package tree", sync::sync_job()).await {
                return ExitCode::FAILURE;
            }
        }

        Command::Preset { command } => match command {
            PresetCommand::List => {
                for p in preset::BUILTIN_PRESETS {
                    println!("{}", p.name);
                    println!("  {}", p.description);
                    println!("  {} package(s), {} USE flag(s)", p.packages.len(), p.use_flags.len());
                }
            }
            PresetCommand::Apply { name, binpkg } => {
                let Some(p) = find_builtin_preset(&name) else {
                    eprintln!("no built-in preset named '{name}' (see `preset list`)");
                    return ExitCode::FAILURE;
                };
                let owned = preset::OwnedPreset::from(p);
                if !apply_owned_preset(&owned, binpkg).await {
                    return ExitCode::FAILURE;
                }
            }
            PresetCommand::ApplyFile { path, binpkg } => {
                let owned = match preset::import(&path) {
                    Ok(owned) => owned,
                    Err(err) => {
                        eprintln!("failed to import preset: {err}");
                        return ExitCode::FAILURE;
                    }
                };
                if !apply_owned_preset(&owned, binpkg).await {
                    return ExitCode::FAILURE;
                }
            }
        },
    }

    ExitCode::SUCCESS
}
