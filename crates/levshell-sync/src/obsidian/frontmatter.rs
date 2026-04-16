//! Minimal Obsidian frontmatter parser.
//!
//! Obsidian notes optionally start with a YAML block delimited by `---` on
//! its own line. The useful fields for v1 are `title` (string) and `tags`
//! (string or list of strings). We parse only those two and ignore the
//! rest — this sidesteps pulling in `serde_yaml` for a format where, in
//! practice, all we ever read are two keys.
//!
//! If the file has no frontmatter, [`parse`] returns a [`Frontmatter`]
//! with empty fields and passes the full content through unchanged.

/// Result of running [`parse`] on a file's bytes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Frontmatter {
    /// `title:` field from the YAML block. Overrides the filename-derived
    /// title when present.
    pub title: Option<String>,
    /// `tags:` field. Supports the two common Obsidian forms:
    ///   tags: rust, shell         → ["rust", "shell"]
    ///   tags: [rust, shell]       → ["rust", "shell"]
    ///   tags:
    ///     - rust
    ///     - shell                 → ["rust", "shell"]
    pub tags: Vec<String>,
}

/// Parse frontmatter from a Markdown file. Returns the extracted fields
/// and the body of the file (content with the frontmatter block stripped).
pub fn parse(content: &str) -> (Frontmatter, &str) {
    let Some(rest) = content.strip_prefix("---\n") else {
        // No frontmatter; also try windows line endings defensively.
        if let Some(rest) = content.strip_prefix("---\r\n") {
            return parse_block(rest);
        }
        return (Frontmatter::default(), content);
    };
    parse_block(rest)
}

fn parse_block(rest: &str) -> (Frontmatter, &str) {
    // Find the closing "---" line. Split on lines so we don't match inside
    // a multi-line value; frontmatter-closing delimiter is always at the
    // start of a line.
    let mut end_offset = None;
    let mut cumulative = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            end_offset = Some(cumulative);
            break;
        }
        cumulative += line.len();
    }
    let Some(end) = end_offset else {
        // Malformed frontmatter (no closer) — treat the whole thing as body.
        return (Frontmatter::default(), rest);
    };
    let block = &rest[..end];
    let after_closer = rest[end..]
        .strip_prefix("---\n")
        .or_else(|| rest[end..].strip_prefix("---\r\n"))
        .or_else(|| rest[end..].strip_prefix("---"))
        .unwrap_or(&rest[end..]);
    let fm = parse_fields(block);
    (fm, after_closer)
}

fn parse_fields(block: &str) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut collecting_tags_list = false;

    for raw in block.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            collecting_tags_list = false;
            continue;
        }
        // Bullet under `tags:` — collect indented entries until a
        // non-bullet line.
        if collecting_tags_list {
            if let Some(tag) = line.trim_start().strip_prefix("- ") {
                let tag = tag.trim().trim_matches(['"', '\'']).to_string();
                if !tag.is_empty() {
                    fm.tags.push(tag);
                }
                continue;
            } else {
                collecting_tags_list = false;
            }
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "title" => {
                if !value.is_empty() {
                    fm.title = Some(value.trim_matches(['"', '\'']).to_string());
                }
            }
            "tags" => {
                if value.is_empty() {
                    // Tags are on following lines as "- tag" entries.
                    collecting_tags_list = true;
                } else if let Some(inner) = value.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
                {
                    fm.tags.extend(
                        inner
                            .split(',')
                            .map(|t| t.trim().trim_matches(['"', '\'']).to_string())
                            .filter(|t| !t.is_empty()),
                    );
                } else {
                    fm.tags.extend(
                        value
                            .split(',')
                            .map(|t| t.trim().trim_matches(['"', '\'']).to_string())
                            .filter(|t| !t.is_empty()),
                    );
                }
            }
            _ => {}
        }
    }
    fm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_frontmatter_passes_content_through() {
        let (fm, body) = parse("# Just a note\n\nHello.");
        assert!(fm.title.is_none());
        assert!(fm.tags.is_empty());
        assert_eq!(body, "# Just a note\n\nHello.");
    }

    #[test]
    fn parses_title_and_inline_tags() {
        let content = "---\ntitle: My Note\ntags: rust, shell\n---\n# Heading\n";
        let (fm, body) = parse(content);
        assert_eq!(fm.title.as_deref(), Some("My Note"));
        assert_eq!(fm.tags, vec!["rust".to_string(), "shell".to_string()]);
        assert_eq!(body, "# Heading\n");
    }

    #[test]
    fn parses_bracketed_tag_list() {
        let content = "---\ntags: [rust, \"shell\"]\n---\nbody";
        let (fm, body) = parse(content);
        assert_eq!(fm.tags, vec!["rust".to_string(), "shell".to_string()]);
        assert_eq!(body, "body");
    }

    #[test]
    fn parses_bullet_tag_list() {
        let content = "---\ntitle: Project Notes\ntags:\n  - rust\n  - shell\n---\ncontent\n";
        let (fm, body) = parse(content);
        assert_eq!(fm.title.as_deref(), Some("Project Notes"));
        assert_eq!(fm.tags, vec!["rust".to_string(), "shell".to_string()]);
        assert_eq!(body, "content\n");
    }

    #[test]
    fn tolerates_missing_closer() {
        let content = "---\ntitle: unclosed\n";
        let (fm, _body) = parse(content);
        // Malformed: frontmatter has no closing ---. Parser treats the
        // content as body; no panic.
        assert!(fm.title.is_none());
    }

    #[test]
    fn ignores_unknown_keys() {
        let content = "---\ntitle: T\naliases: [X, Y]\nsource: me\n---\nbody";
        let (fm, body) = parse(content);
        assert_eq!(fm.title.as_deref(), Some("T"));
        assert!(fm.tags.is_empty());
        assert_eq!(body, "body");
    }
}
