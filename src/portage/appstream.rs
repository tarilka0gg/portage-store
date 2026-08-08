use quick_xml::events::Event;
use quick_xml::Reader;
use std::fs;
use std::path::PathBuf;

/// Upstream-authored presentation data for an app, in the AppStream format
/// that desktop stores use: several paragraphs of real prose and a set of
/// screenshot URLs.
///
/// Portage itself has none of this — an ebuild carries a one-line DESCRIPTION
/// and nothing else — so it comes from the `.metainfo.xml`/`.appdata.xml`
/// the package installs. That file only exists once the package is on disk,
/// which is why this is empty for anything not yet installed.
#[derive(Debug, Clone, Default)]
pub struct AppStreamInfo {
    pub paragraphs: Vec<String>,
    pub screenshots: Vec<String>,
}

impl AppStreamInfo {
    pub fn is_empty(&self) -> bool {
        self.paragraphs.is_empty() && self.screenshots.is_empty()
    }
}

/// Locates the AppStream file an installed package shipped, by scanning the
/// file list portage recorded for it.
fn find_metainfo_file(category: &str, name: &str, version: &str) -> Option<PathBuf> {
    let contents_path = format!("/var/db/pkg/{category}/{name}-{version}/CONTENTS");
    let contents = fs::read_to_string(contents_path).ok()?;
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("obj") {
            continue;
        }
        let Some(path) = parts.next() else { continue };
        let in_appstream_dir = path.contains("/metainfo/") || path.contains("/appdata/");
        if in_appstream_dir && path.ends_with(".xml") {
            let path = PathBuf::from(path);
            if path.exists() {
                return Some(path);
            }
        }
    }
    None
}

/// Pulls the description paragraphs and screenshot URLs out of an AppStream
/// document.
///
/// Only untranslated elements are taken: AppStream marks localised copies
/// with an `xml:lang` attribute, and without that filter every paragraph
/// would repeat once per language shipped in the file.
fn parse(xml: &str) -> AppStreamInfo {
    let mut info = AppStreamInfo::default();
    let mut reader = Reader::from_str(xml);
    // Whitespace is kept rather than trimmed per event, because an entity
    // splits its paragraph into several text events and the spaces on
    // either side of it live at those seams. "messaging &amp; more" would
    // otherwise come back as "messaging& more".
    reader.config_mut().trim_text(false);

    let mut in_description = false;
    let mut in_paragraph = false;
    let mut in_image = false;
    let mut localised = false;
    let mut current = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let has_lang = e
                    .attributes()
                    .flatten()
                    .any(|a| a.key.as_ref().ends_with(b"lang"));
                match e.name().as_ref() {
                    b"description" => in_description = true,
                    b"p" if in_description => {
                        in_paragraph = true;
                        localised = has_lang;
                        current.clear();
                    }
                    b"image" => {
                        in_image = true;
                        localised = has_lang;
                        current.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"description" => in_description = false,
                b"p" => {
                    if in_paragraph && !localised {
                        let text = normalize(&current);
                        if !text.is_empty() {
                            info.paragraphs.push(text);
                        }
                    }
                    in_paragraph = false;
                    current.clear();
                }
                b"image" => {
                    if in_image && !localised {
                        let url = normalize(&current);
                        if url.starts_with("http") {
                            info.screenshots.push(url);
                        }
                    }
                    in_image = false;
                    current.clear();
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if in_paragraph || in_image {
                    current.push_str(&e.decode().unwrap_or_default());
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if in_paragraph || in_image {
                    current.push_str(&resolve_entity(&e));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    info.screenshots.dedup();
    info
}

/// Collapses the line wrapping and indentation that AppStream files use for
/// readability into the single spaces a rendered paragraph wants.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Resolves the five predefined XML entities plus numeric character
/// references. Anything else is dropped: AppStream files don't declare
/// custom entities, and emitting a raw `&name;` would be worse than a gap.
fn resolve_entity(name: &[u8]) -> String {
    match name {
        b"amp" => "&".to_string(),
        b"lt" => "<".to_string(),
        b"gt" => ">".to_string(),
        b"quot" => "\"".to_string(),
        b"apos" => "'".to_string(),
        other => {
            let text = String::from_utf8_lossy(other);
            let Some(digits) = text.strip_prefix('#') else {
                return String::new();
            };
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => digits.parse::<u32>().ok(),
            };
            code.and_then(char::from_u32).map(String::from).unwrap_or_default()
        }
    }
}

/// Best-effort AppStream lookup for an installed package.
pub fn lookup(category: &str, name: &str, version: &str) -> Option<AppStreamInfo> {
    let path = find_metainfo_file(category, name, version)?;
    let xml = fs::read_to_string(path).ok()?;
    let info = parse(&xml);
    (!info.is_empty()).then_some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
<component type="desktop-application">
  <name>Telegram Desktop</name>
  <description>
    <p>Pure instant messaging &amp; more.</p>
    <p xml:lang="uk">Просто месенджер.</p>
    <p>SECURE: everything is encrypted.</p>
  </description>
  <screenshots>
    <screenshot type="default">
      <image>https://example.org/preview.png</image>
    </screenshot>
    <screenshot>
      <image>https://example.org/slide.01.jpg</image>
      <caption>A lightning-fast native app</caption>
    </screenshot>
  </screenshots>
</component>
"#;

    #[test]
    fn takes_untranslated_paragraphs_only() {
        let info = parse(SAMPLE);
        assert_eq!(
            info.paragraphs,
            vec!["Pure instant messaging & more.", "SECURE: everything is encrypted."]
        );
    }

    #[test]
    fn collects_screenshot_urls_in_order() {
        let info = parse(SAMPLE);
        assert_eq!(
            info.screenshots,
            vec!["https://example.org/preview.png", "https://example.org/slide.01.jpg"]
        );
    }

    #[test]
    fn numeric_and_named_entities_resolve() {
        assert_eq!(resolve_entity(b"amp"), "&");
        assert_eq!(resolve_entity(b"#233"), "\u{e9}");
        assert_eq!(resolve_entity(b"#x2014"), "\u{2014}");
        assert_eq!(resolve_entity(b"nbsp"), "");
    }

    #[test]
    fn wrapped_paragraphs_collapse_to_single_spaces() {
        let xml = "<description><p>one\n   two\n\tthree</p></description>";
        assert_eq!(parse(xml).paragraphs, vec!["one two three"]);
    }

    #[test]
    fn captions_are_not_mistaken_for_images() {
        assert!(!parse(SAMPLE).screenshots.iter().any(|s| s.contains("lightning")));
    }
}
