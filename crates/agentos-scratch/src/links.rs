use regex::Regex;
use std::sync::LazyLock;

static WIKILINK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[\[\s*([^\]\|]+?)\s*(?:\|\s*([^\]]*?)\s*)?\]\]").expect("valid wikilink regex")
});

/// Parsed wikilink with target title and optional display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    /// The page title being linked to.
    pub target: String,
    /// Optional alias text after `|`.
    pub display: Option<String>,
    /// True when link target uses `@agent_id/title`.
    pub is_cross_agent: bool,
    /// If cross-agent, the destination agent id.
    pub agent_id: Option<String>,
}

/// Parse all `[[wikilinks]]` from markdown content.
pub fn parse_wikilinks(content: &str) -> Vec<WikiLink> {
    WIKILINK_RE
        .captures_iter(content)
        .map(|cap| {
            let raw_target = cap
                .get(1)
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let display = cap
                .get(2)
                .map(|m| m.as_str().trim().to_string())
                .filter(|s| !s.is_empty());

            if let Some(stripped) = raw_target.strip_prefix('@') {
                if let Some((agent_id, title)) = stripped.split_once('/') {
                    let agent_id = agent_id.trim();
                    let title = title.trim();
                    if !agent_id.is_empty() && !title.is_empty() {
                        return WikiLink {
                            target: title.to_string(),
                            display,
                            is_cross_agent: true,
                            agent_id: Some(agent_id.to_string()),
                        };
                    }
                }
                // Malformed cross-agent syntax (e.g. [[@agent]] without slash):
                // strip the @ and treat as a local link.
                WikiLink {
                    target: stripped.to_string(),
                    display,
                    is_cross_agent: false,
                    agent_id: None,
                }
            } else {
                WikiLink {
                    target: raw_target,
                    display,
                    is_cross_agent: false,
                    agent_id: None,
                }
            }
        })
        .filter(|link| !link.target.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_wikilinks, WikiLink};

    #[test]
    fn test_parse_simple_wikilink() {
        let links = parse_wikilinks("See [[Foo]] for details.");
        assert_eq!(
            links,
            vec![WikiLink {
                target: "Foo".to_string(),
                display: None,
                is_cross_agent: false,
                agent_id: None,
            }]
        );
    }

    #[test]
    fn test_parse_aliased_wikilink() {
        let links = parse_wikilinks("See [[Foo|bar]] for details.");
        assert_eq!(
            links,
            vec![WikiLink {
                target: "Foo".to_string(),
                display: Some("bar".to_string()),
                is_cross_agent: false,
                agent_id: None,
            }]
        );
    }

    #[test]
    fn test_parse_cross_agent() {
        let links = parse_wikilinks("See [[@agent123/Foo]] for details.");
        assert_eq!(
            links,
            vec![WikiLink {
                target: "Foo".to_string(),
                display: None,
                is_cross_agent: true,
                agent_id: Some("agent123".to_string()),
            }]
        );
    }

    #[test]
    fn test_parse_multiple() {
        let links = parse_wikilinks("[[A]] and [[B|beta]] and [[@agent-x/C]].");
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].target, "A");
        assert_eq!(links[1].target, "B");
        assert_eq!(links[1].display.as_deref(), Some("beta"));
        assert_eq!(links[2].target, "C");
        assert_eq!(links[2].agent_id.as_deref(), Some("agent-x"));
        assert!(links[2].is_cross_agent);
    }

    #[test]
    fn test_parse_no_links() {
        let links = parse_wikilinks("This content has no wiki links.");
        assert!(links.is_empty());
    }

    #[test]
    fn test_parse_malformed_cross_agent_strips_at() {
        // [[@agent]] without a slash — should strip @ and treat as local link
        let links = parse_wikilinks("See [[@agent]] here.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "agent");
        assert!(!links[0].is_cross_agent);
        assert!(links[0].agent_id.is_none());
    }

    #[test]
    fn test_parse_whitespace_only_filtered() {
        // [[  ]] should be filtered out (empty target after trim)
        let links = parse_wikilinks("See [[  ]] and [[Valid]].");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Valid");
    }
}
