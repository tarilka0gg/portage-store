/// A search box query, split into recognized operator tokens and whatever
/// free text is left over. Pure and synchronous — parsing never touches
/// `eix`/the filesystem, only the raw string the user typed; the caller
/// (`ui::mod`'s `run_search`/`render_filtered_results`) decides what each
/// field actually does against real package data.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    /// Whatever's left after every recognized operator token is stripped
    /// out, re-joined with single spaces — this is what actually goes to
    /// `eix::search`/`eix::list_categories`.
    pub text: String,
    /// From `cat:<name>` — a bare category name, not a regex (the caller
    /// builds whatever `eix` call it needs from it).
    pub category: Option<String>,
    /// From `use:<flag>` — a plain IUSE flag name.
    pub use_flag: Option<String>,
    /// From the literal token `@world`.
    pub world_only: bool,
    /// From `installed:` (bare, or `installed:true`/`installed:false`
    /// spelled out) — `None` means the operator wasn't present at all,
    /// distinct from an explicit `installed:false`.
    pub installed_only: Option<bool>,
}

/// Case-insensitively strips `prefix` off `token`, returning the rest —
/// `None` if `token` doesn't start with it. Operators are matched
/// case-insensitively (`Cat:`, `USE:` etc. all work) since a search box
/// isn't a place anyone expects to have to get letter-casing exactly
/// right.
fn strip_prefix_ci<'a>(token: &'a str, prefix: &str) -> Option<&'a str> {
    if token.len() < prefix.len() || !token.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes()) {
        return None;
    }
    Some(&token[prefix.len()..])
}

pub fn parse(raw: &str) -> ParsedQuery {
    let mut parsed = ParsedQuery::default();
    let mut leftover: Vec<&str> = Vec::new();

    for token in raw.split_whitespace() {
        if token.eq_ignore_ascii_case("@world") {
            parsed.world_only = true;
        } else if let Some(rest) = strip_prefix_ci(token, "cat:") {
            if rest.is_empty() {
                leftover.push(token);
            } else {
                parsed.category = Some(rest.to_string());
            }
        } else if let Some(rest) = strip_prefix_ci(token, "use:") {
            if rest.is_empty() {
                leftover.push(token);
            } else {
                parsed.use_flag = Some(rest.to_string());
            }
        } else if let Some(rest) = strip_prefix_ci(token, "installed:") {
            match rest {
                "" | "true" => parsed.installed_only = Some(true),
                "false" => parsed.installed_only = Some(false),
                // Doesn't look like a real value for this operator —
                // treat the whole token as ordinary search text rather
                // than silently discarding whatever the user actually
                // typed.
                _ => leftover.push(token),
            }
        } else {
            leftover.push(token);
        }
    }

    parsed.text = leftover.join(" ");
    parsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_query_has_no_operators() {
        let parsed = parse("firefox browser");
        assert_eq!(parsed, ParsedQuery { text: "firefox browser".to_string(), ..Default::default() });
    }

    #[test]
    fn strips_a_single_operator_leaving_the_rest_as_text() {
        let parsed = parse("firefox use:wayland");
        assert_eq!(parsed.text, "firefox");
        assert_eq!(parsed.use_flag.as_deref(), Some("wayland"));
    }

    #[test]
    fn combines_several_operators_at_once() {
        let parsed = parse("cat:www-client use:wayland installed: firefox");
        assert_eq!(parsed.text, "firefox");
        assert_eq!(parsed.category.as_deref(), Some("www-client"));
        assert_eq!(parsed.use_flag.as_deref(), Some("wayland"));
        assert_eq!(parsed.installed_only, Some(true));
    }

    #[test]
    fn world_operator_is_case_insensitive_and_needs_no_value() {
        let parsed = parse("@World");
        assert!(parsed.world_only);
        assert_eq!(parsed.text, "");
    }

    #[test]
    fn installed_true_and_false_are_both_recognized() {
        assert_eq!(parse("installed:true").installed_only, Some(true));
        assert_eq!(parse("installed:false").installed_only, Some(false));
    }

    #[test]
    fn an_operator_with_no_value_is_left_as_plain_text() {
        let parsed = parse("cat: firefox");
        assert_eq!(parsed.category, None);
        assert_eq!(parsed.text, "cat: firefox");
    }

    #[test]
    fn an_unrecognized_colon_token_is_not_swallowed() {
        let parsed = parse("foo:bar firefox");
        assert_eq!(parsed.text, "foo:bar firefox");
        assert_eq!(parsed.category, None);
        assert_eq!(parsed.use_flag, None);
    }

    #[test]
    fn operator_only_query_leaves_empty_text() {
        let parsed = parse("cat:net-misc");
        assert_eq!(parsed.text, "");
        assert_eq!(parsed.category.as_deref(), Some("net-misc"));
    }
}
