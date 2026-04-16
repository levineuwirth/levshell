//! Minimal Obsidian wiki-link parser.
//!
//! Scans a Markdown body for `[[wiki-link]]` references and returns each
//! target's *resolved target path* — the part of the link that
//! identifies the note, stripped of alias / heading / block-id
//! suffixes. Handles:
//!
//! - `[[Target]]` → `Target`
//! - `[[Target|Alias]]` → `Target`
//! - `[[Target#Heading]]` → `Target`
//! - `[[Target#Heading|Alias]]` → `Target`
//! - `[[Target^block]]` → `Target`
//! - `[[folder/Target]]` → `folder/Target` (path prefixes are kept)
//!
//! Skips content inside inline code spans (`` `...` ``) and fenced code
//! blocks (` ``` `) so notes that *discuss* wiki-link syntax don't
//! produce phantom edges. The parser is deliberately small and
//! allocation-light; Obsidian itself accepts a slightly larger superset
//! (e.g. embed syntax `![[Target]]`), but for the graph-population
//! purpose the extra variants reduce to the same target path.

/// Extract every wiki-link target found in `body`, deduplicated while
/// preserving first-occurrence order. Empty targets and pure-heading
/// references (`[[#section]]`) are dropped.
pub fn extract(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in raw_links(body) {
        let target = normalize(&raw);
        if target.is_empty() {
            continue;
        }
        if seen.insert(target.clone()) {
            out.push(target);
        }
    }
    out
}

/// Walk `body`, yielding the raw content between each `[[` and `]]`
/// pair that lies outside a code span or fenced block. Returns owned
/// strings — the caller doesn't need to track lifetimes.
fn raw_links(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = body.char_indices().peekable();
    let mut in_fence = false;

    // Line-level scan so fenced-block toggling is per-line.
    while let Some((line_start, _)) = chars.peek().copied() {
        // Slice off the current line.
        let line_end = body[line_start..]
            .find('\n')
            .map(|i| line_start + i)
            .unwrap_or(body.len());
        let line = &body[line_start..line_end];

        // Advance `chars` past the line + optional newline so the next
        // loop iteration starts fresh.
        while let Some(&(i, _)) = chars.peek() {
            if i >= line_end {
                break;
            }
            chars.next();
        }
        if matches!(chars.peek(), Some(&(_, '\n'))) {
            chars.next();
        }

        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        scan_line(line, &mut out);
    }
    out
}

/// Find every `[[...]]` on a single line, skipping content inside
/// inline-code backtick runs.
fn scan_line(line: &str, out: &mut Vec<String>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_code = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'`' {
            in_code = !in_code;
            i += 1;
            continue;
        }
        if in_code {
            i += 1;
            continue;
        }
        if b == b'[' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Find the matching ]] — search byte-wise since brackets
            // can only appear in ASCII contexts for wiki links.
            let mut j = i + 2;
            while j + 1 < bytes.len() {
                if bytes[j] == b']' && bytes[j + 1] == b']' {
                    break;
                }
                if bytes[j] == b'\n' {
                    // Wiki-links don't span lines.
                    break;
                }
                j += 1;
            }
            if j + 1 < bytes.len() && bytes[j] == b']' && bytes[j + 1] == b']' {
                // Safe: all characters in between must be valid UTF-8
                // since `line` is &str.
                let inner = &line[i + 2..j];
                if !inner.is_empty() {
                    out.push(inner.to_string());
                }
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
}

/// Strip `|alias`, `#heading`, and `^block-id` suffixes from a raw
/// wiki-link body, returning the target-path portion trimmed of
/// surrounding whitespace.
fn normalize(raw: &str) -> String {
    let mut target = raw;
    if let Some(idx) = target.find('|') {
        target = &target[..idx];
    }
    if let Some(idx) = target.find('#') {
        target = &target[..idx];
    }
    if let Some(idx) = target.find('^') {
        target = &target[..idx];
    }
    target.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bare_link() {
        assert_eq!(extract("See [[Target]] for details."), vec!["Target"]);
    }

    #[test]
    fn extracts_link_with_alias() {
        assert_eq!(extract("[[Canonical Name|shown]]"), vec!["Canonical Name"]);
    }

    #[test]
    fn extracts_link_with_heading() {
        assert_eq!(extract("[[Page#Section]]"), vec!["Page"]);
    }

    #[test]
    fn extracts_link_with_heading_and_alias() {
        assert_eq!(extract("[[Page#Section|display]]"), vec!["Page"]);
    }

    #[test]
    fn keeps_folder_prefix() {
        assert_eq!(extract("[[research/paper]]"), vec!["research/paper"]);
    }

    #[test]
    fn block_reference_is_stripped() {
        assert_eq!(extract("[[Page^abc123]]"), vec!["Page"]);
    }

    #[test]
    fn dedup_preserves_first_order() {
        let body = "[[a]] and [[b]] and [[a]] again";
        assert_eq!(extract(body), vec!["a", "b"]);
    }

    #[test]
    fn pure_heading_reference_is_dropped() {
        assert_eq!(extract("See [[#Section]]"), Vec::<String>::new());
    }

    #[test]
    fn empty_link_body_is_dropped() {
        assert_eq!(extract("weird [[]] syntax"), Vec::<String>::new());
    }

    #[test]
    fn multiple_links_on_one_line() {
        assert_eq!(extract("[[a]] [[b]] [[c]]"), vec!["a", "b", "c"]);
    }

    #[test]
    fn inline_code_is_skipped() {
        let body = "prose [[real]] `code [[fake]] more code` more [[also_real]]";
        assert_eq!(extract(body), vec!["real", "also_real"]);
    }

    #[test]
    fn fenced_code_block_is_skipped() {
        let body = "
before [[keep]]
```
[[ignored]]
```
after [[also_keep]]
";
        assert_eq!(extract(body), vec!["keep", "also_keep"]);
    }

    #[test]
    fn tilde_fenced_block_also_skipped() {
        let body = "
before [[keep]]
~~~
[[ignored]]
~~~
after [[also_keep]]
";
        assert_eq!(extract(body), vec!["keep", "also_keep"]);
    }

    #[test]
    fn links_cannot_span_newlines() {
        // The "[[" opens but no "]]" comes before the newline — this
        // is not a wiki-link and must not be captured.
        let body = "[[not a link\nreally]]";
        assert_eq!(extract(body), Vec::<String>::new());
    }

    #[test]
    fn handles_unicode_in_target() {
        assert_eq!(extract("[[café]]"), vec!["café"]);
        assert_eq!(extract("[[研究/論文]]"), vec!["研究/論文"]);
    }

    #[test]
    fn ignores_single_brackets() {
        // Markdown links like [text](url) must not be picked up.
        assert_eq!(
            extract("[text](https://example.com)"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn embed_syntax_also_extracts() {
        // Obsidian's embed syntax ![[Target]] — we lose the leading !
        // but otherwise treat it as a regular link for graph purposes.
        assert_eq!(extract("![[Embedded]]"), vec!["Embedded"]);
    }
}
