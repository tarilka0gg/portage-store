use anyhow::{Context, Result};

const WORLD_FILE: &str = "/var/lib/portage/world";

/// Reads the `@world` set: every package atom explicitly installed by the
/// user (as opposed to pulled in as a dependency).
pub fn read() -> Result<Vec<String>> {
    let content = std::fs::read_to_string(WORLD_FILE)
        .with_context(|| format!("failed to read {}", WORLD_FILE))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect())
}
