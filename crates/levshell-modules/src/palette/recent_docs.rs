//! Recent-documents palette provider (spec §2.1.2 / §2.5.4).
//!
//! Reads the freedesktop recent-files store
//! (`$XDG_DATA_HOME/recently-used.xbel`, written by GTK/Qt file dialogs),
//! newest first. An empty query lists the most recent files; a non-empty
//! query filters on the file name. Selecting opens the file with
//! `xdg-open`. Missing files are skipped so a stale entry never offers a
//! dead link.
//!
//! The XBEL is parsed with a deliberately small hand-rolled scan rather
//! than pulling an XML crate into the workspace — we only need two
//! attributes per `<bookmark>`.

use std::path::PathBuf;

use async_trait::async_trait;

use super::provider::{PaletteItem, PaletteProvider, ProviderError, ProviderResult};

pub const RECENT_DOCS_PROVIDER: &str = "recent-docs";
const MAX_RESULTS: usize = 12;

pub struct RecentDocsProvider;

impl RecentDocsProvider {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RecentDocsProvider {
    fn default() -> Self {
        Self::new()
    }
}

fn xbel_path() -> Option<PathBuf> {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("recently-used.xbel"))
}

/// Minimal percent-decoder for `file://` paths (spaces → `%20`, etc.).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// `(modified-timestamp-string, absolute-path)` newest first. The XBEL
/// `modified` attribute is ISO-8601, so lexical sort = chronological.
fn parse_xbel(xml: &str) -> Vec<(String, PathBuf)> {
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for chunk in xml.split("<bookmark").skip(1) {
        let tag = match chunk.find('>') {
            Some(i) => &chunk[..i],
            None => continue,
        };
        let Some(href) = attr(tag, "href") else { continue };
        let Some(path) = href.strip_prefix("file://") else { continue };
        let path = PathBuf::from(percent_decode(path));
        let modified = attr(tag, "modified")
            .or_else(|| attr(tag, "visited"))
            .unwrap_or("")
            .to_string();
        entries.push((modified, path));
    }
    entries.sort_by(|a, b| b.0.cmp(&a.0));
    entries
}

#[async_trait]
impl PaletteProvider for RecentDocsProvider {
    fn name(&self) -> &'static str {
        RECENT_DOCS_PROVIDER
    }

    async fn search(&self, query: &str) -> Vec<PaletteItem> {
        let Some(path) = xbel_path() else {
            return Vec::new();
        };
        let Ok(xml) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let q = query.trim().to_lowercase();
        let mut out = Vec::new();
        for (rank, (_, file)) in parse_xbel(&xml).into_iter().enumerate() {
            if !file.exists() {
                continue;
            }
            let name = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if !q.is_empty() && !name.to_lowercase().contains(&q) {
                continue;
            }
            // Recency-ranked; below exact app/calc hits, comparable to
            // note/ref search so a remembered file competes fairly.
            let score = 0.68 - (rank as f64 * 0.02).min(0.3);
            out.push(
                PaletteItem::new(RECENT_DOCS_PROVIDER, file.display().to_string(), name)
                    .with_subtitle(
                        file.parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                    )
                    .with_icon("file")
                    .with_score(score),
            );
            if out.len() >= MAX_RESULTS {
                break;
            }
        }
        out
    }

    async fn execute(&self, item_id: &str) -> ProviderResult<()> {
        // item_id is the absolute path (see `search`).
        if !std::path::Path::new(item_id).exists() {
            tracing::info!(path = item_id, "recent-docs: file vanished");
            return Ok(());
        }
        super::spawn_detached("xdg-open", &[item_id])
            .map_err(|e| ProviderError::ExecuteFailed(format!("xdg-open: {e}")))?;
        tracing::info!(path = item_id, "recent-docs: opened");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_spaces_and_utf8() {
        assert_eq!(percent_decode("/home/u/My%20Paper.pdf"), "/home/u/My Paper.pdf");
        assert_eq!(percent_decode("/a/plain/path"), "/a/plain/path");
    }

    #[test]
    fn parse_xbel_extracts_newest_first() {
        let xml = r#"<?xml version="1.0"?>
<xbel>
 <bookmark href="file:///tmp/old.txt" modified="2024-01-01T00:00:00Z"/>
 <bookmark href="file:///tmp/new%20file.pdf" modified="2025-06-01T12:00:00Z"/>
</xbel>"#;
        let e = parse_xbel(xml);
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].1, PathBuf::from("/tmp/new file.pdf"));
        assert_eq!(e[1].1, PathBuf::from("/tmp/old.txt"));
    }

    #[test]
    fn attr_missing_is_none() {
        assert_eq!(attr("href=\"x\"", "modified"), None);
        assert_eq!(attr("href=\"x\" modified=\"y\"", "modified"), Some("y"));
    }
}
