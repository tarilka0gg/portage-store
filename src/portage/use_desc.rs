use std::collections::HashMap;
use std::sync::OnceLock;

const GLOBAL_DESC: &str = "/var/db/repos/gentoo/profiles/use.desc";
const LOCAL_DESC: &str = "/var/db/repos/gentoo/profiles/use.local.desc";

fn parse_global(content: &str) -> HashMap<String, String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (flag, desc) = line.split_once(" - ")?;
            Some((flag.trim().to_string(), desc.trim().to_string()))
        })
        .collect()
}

fn parse_local(content: &str) -> HashMap<(String, String), String> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (atom_flag, desc) = line.split_once(" - ")?;
            let (atom, flag) = atom_flag.split_once(':')?;
            Some(((atom.trim().to_string(), flag.trim().to_string()), desc.trim().to_string()))
        })
        .collect()
}

fn global() -> &'static HashMap<String, String> {
    static CELL: OnceLock<HashMap<String, String>> = OnceLock::new();
    CELL.get_or_init(|| {
        std::fs::read_to_string(GLOBAL_DESC)
            .map(|s| parse_global(&s))
            .unwrap_or_default()
    })
}

fn local() -> &'static HashMap<(String, String), String> {
    static CELL: OnceLock<HashMap<(String, String), String>> = OnceLock::new();
    CELL.get_or_init(|| {
        std::fs::read_to_string(LOCAL_DESC)
            .map(|s| parse_local(&s))
            .unwrap_or_default()
    })
}

/// Human-readable description for a USE flag on a given package, falling
/// back to the global (non-package-specific) description.
pub fn describe(atom: &str, flag: &str) -> Option<String> {
    local()
        .get(&(atom.to_string(), flag.to_string()))
        .or_else(|| global().get(flag))
        .cloned()
}

/// The maintainer-written long description from the ebuild's metadata.xml,
/// when one exists — a few sentences instead of eix's one-liner. Not every
/// package has one; this is best-effort.
pub fn long_description(atom: &str) -> Option<String> {
    // Every configured repository, not just ::gentoo — a package installed
    // from an overlay such as ::guru has its metadata.xml only there, and
    // looking in ::gentoo alone silently loses the description.
    let repos = std::fs::read_dir("/var/db/repos").ok()?;
    for repo in repos.flatten() {
        let path = repo.path().join(atom).join("metadata.xml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(text) = extract_long_description(&content) {
            return Some(text);
        }
    }
    None
}

/// The upstream GitHub repository (`owner/repo`), when the ebuild's
/// maintainer recorded one in metadata.xml's `<remote-id type="github">`.
///
/// This exists specifically to avoid guessing: a name-based GitHub search
/// for a package like `dev-ruby/git` — a small Ruby wrapper library — finds
/// `git/git`, the 60k-star version control system, because it's the far
/// more popular repo with the matching name. Gentoo maintainers already
/// resolved this exact ambiguity by hand for repology's benefit; reading
/// their answer beats re-deriving a wrong one from a name search.
pub fn github_remote_id(atom: &str) -> Option<String> {
    let repos = std::fs::read_dir("/var/db/repos").ok()?;
    for repo in repos.flatten() {
        let path = repo.path().join(atom).join("metadata.xml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(id) = extract_remote_id(&content, "github") {
            return Some(id);
        }
    }
    None
}

/// Pulls a `<remote-id type="TYPE">value</remote-id>` value out of a
/// metadata.xml. A package can list several remotes (github, pypi,
/// rubygems, ...); this returns the first of the requested type.
fn extract_remote_id(content: &str, remote_type: &str) -> Option<String> {
    let marker = format!(r#"type="{remote_type}">"#);
    let mut rest = content;
    loop {
        let start = rest.find("<remote-id")?;
        rest = &rest[start..];
        let tag_end = rest.find('>')? + 1;
        let opening_tag = &rest[..tag_end];
        if opening_tag.contains(&marker) {
            let close = rest[tag_end..].find("</remote-id>")? + tag_end;
            let value = rest[tag_end..close].trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
        rest = &rest[tag_end..];
    }
}

fn extract_long_description(content: &str) -> Option<String> {
    let start = content.find("<longdescription")?;
    let tag_end = content[start..].find('>')? + start + 1;
    let end = content[tag_end..].find("</longdescription>")? + tag_end;
    let normalized = content[tag_end..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_github_remote_among_several() {
        let xml = r#"
<pkgmetadata>
  <upstream>
    <remote-id type="github">ruby-git/ruby-git</remote-id>
    <remote-id type="rubygems">git</remote-id>
  </upstream>
</pkgmetadata>
"#;
        assert_eq!(extract_remote_id(xml, "github").as_deref(), Some("ruby-git/ruby-git"));
        assert_eq!(extract_remote_id(xml, "rubygems").as_deref(), Some("git"));
    }

    #[test]
    fn no_remote_of_the_requested_type_is_none() {
        let xml = r#"<remote-id type="pypi">requests</remote-id>"#;
        assert_eq!(extract_remote_id(xml, "github"), None);
    }

    #[test]
    fn missing_metadata_is_not_an_error() {
        assert_eq!(extract_remote_id("<pkgmetadata></pkgmetadata>", "github"), None);
    }
}
