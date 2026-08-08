use anyhow::{Context, Result};
use std::collections::BTreeMap;

pub const MAKE_CONF: &str = "/etc/portage/make.conf";

/// Variables the GUI exposes with dedicated widgets. Everything else in
/// make.conf is still preserved verbatim and editable as raw text.
pub const KNOWN_VARS: &[&str] = &[
    "USE",
    "MAKEOPTS",
    "ACCEPT_KEYWORDS",
    "ACCEPT_LICENSE",
    "VIDEO_CARDS",
    "L10N",
    "CPU_FLAGS_X86",
];

pub fn read_raw() -> Result<String> {
    std::fs::read_to_string(MAKE_CONF).with_context(|| format!("failed to read {}", MAKE_CONF))
}

/// Extracts simple `KEY="value"` / `KEY=value` assignments. Lines that
/// aren't a plain top-level assignment (comments, multi-line continuations,
/// variable references like `${COMMON_FLAGS}`) are left untouched and just
/// don't show up here.
pub fn parse_vars(raw: &str) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !key.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()) {
            continue;
        }
        let value = value.trim();
        let value = value.strip_prefix('"').unwrap_or(value);
        let value = value.strip_suffix('"').unwrap_or(value);
        vars.insert(key.to_string(), value.to_string());
    }
    vars
}

/// Replaces (or appends) a single `KEY="value"` assignment in the raw
/// make.conf text, returning the new full file contents.
pub fn set_var(raw: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let mut out_lines: Vec<String> = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some((k, _)) = trimmed.split_once('=')
            && k.trim() == key && !found {
                out_lines.push(format!("{key}=\"{value}\""));
                found = true;
                continue;
            }
        out_lines.push(line.to_string());
    }

    if !found {
        if !out_lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            out_lines.push(String::new());
        }
        out_lines.push(format!("{key}=\"{value}\""));
    }

    let mut result = out_lines.join("\n");
    result.push('\n');
    result
}
