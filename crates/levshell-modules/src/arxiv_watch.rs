//! arXiv new-paper watcher (spec §2.9.9 — "Configure keywords or
//! followed authors. A badge appears when new matching papers are
//! found. Click to see titles and abstracts; click through to open the
//! PDF").
//!
//! Polls the open arXiv Atom API (no key required). Dormant unless the
//! user configures keywords/authors in
//! `~/.config/levshell/modules/arxiv.toml`. Seen IDs persist to
//! `$XDG_STATE_HOME/levshell/arxiv_seen.json` so "new" survives a daemon
//! restart and the badge doesn't re-alarm on every poll.
//!
//! Network is treated like a sync adapter (spec §5.1): a fetch/parse
//! failure is logged and the last good state is kept — it never fails
//! the daemon. Semantic Scholar (which needs an API key + rate-limit
//! handling) is a deliberate later addition; arXiv alone covers the
//! core "new matching papers" need.
//!
//! State: `{ new_count, items: [{title, summary, url, published}] }`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use levshell_core::{Event, EventKind, Module, ModuleResult, WidgetDescriptor};
use levshell_ipc::{DaemonMessage, WidgetPublisher, WidgetStatus, WidgetUpdate};
use serde::Deserialize;

pub const ARXIV_WIDGET_ID: &str = "arxiv-watch";
pub const ARXIV_WIDGET_TYPE: &str = "arxiv_watch";
const MODULE_NAME: &str = "arxiv-watch";
const MAX_SEEN: usize = 2000;

fn d_poll() -> u64 {
    60
}
fn d_max() -> u32 {
    20
}

/// `~/.config/levshell/modules/arxiv.toml`. With no keywords *and* no
/// authors the module is dormant — it never hits the network.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ArxivConfig {
    pub keywords: Vec<String>,
    pub authors: Vec<String>,
    pub poll_minutes: Option<u64>,
    pub max_results: Option<u32>,
}

impl ArxivConfig {
    pub fn load_from_dir(dir: &std::path::Path) -> Self {
        let path = dir.join("arxiv.toml");
        match std::fs::read_to_string(&path) {
            Ok(t) => toml::from_str(&t).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "arxiv.toml malformed; watcher dormant");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    fn dormant(&self) -> bool {
        self.keywords.is_empty() && self.authors.is_empty()
    }

    /// `search_query` value: OR of `all:"kw"` and `au:"name"` terms.
    fn query(&self) -> String {
        let mut terms: Vec<String> = Vec::new();
        for k in &self.keywords {
            terms.push(format!("all:{}", quote(k)));
        }
        for a in &self.authors {
            terms.push(format!("au:{}", quote(a)));
        }
        terms.join("+OR+")
    }
}

fn quote(s: &str) -> String {
    // arXiv wants %22 around multi-word phrases; spaces → +.
    format!("%22{}%22", s.trim().replace(' ', "+"))
}

fn default_seen_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("levshell").join("arxiv_seen.json")
}

/// One parsed Atom `<entry>`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct Paper {
    id: String,
    title: String,
    summary: String,
    url: String,
    published: String,
}

pub struct ArxivWatchModule {
    cfg: ArxivConfig,
    http: reqwest::Client,
    publisher: WidgetPublisher,
    seen: HashSet<String>,
    seen_path: PathBuf,
    /// The new papers from the last poll, retained so the dropdown keeps
    /// showing them until the next poll.
    last_new: Vec<Paper>,
}

impl ArxivWatchModule {
    pub fn new(cfg: ArxivConfig, publisher: WidgetPublisher) -> Self {
        let seen_path = default_seen_path();
        let seen = std::fs::read_to_string(&seen_path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default();
        let http = reqwest::Client::builder()
            .timeout(StdDuration::from_secs(20))
            .user_agent("levshell/0.1 (+https://github.com/levineuwirth/levshell)")
            .build()
            .unwrap_or_default();
        Self {
            cfg,
            http,
            publisher,
            seen,
            seen_path,
            last_new: Vec::new(),
        }
    }

    fn persist_seen(&self) {
        // Keep the set bounded — oldest membership is irrelevant once a
        // paper has been acknowledged once.
        let mut v: Vec<&String> = self.seen.iter().collect();
        if v.len() > MAX_SEEN {
            v.drain(0..v.len() - MAX_SEEN);
        }
        if let Some(parent) = self.seen_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(&v) {
            let _ = std::fs::write(&self.seen_path, json);
        }
    }

    async fn fetch(&self) -> Option<String> {
        let url = format!(
            "http://export.arxiv.org/api/query?search_query={}\
             &sortBy=submittedDate&sortOrder=descending&max_results={}",
            self.cfg.query(),
            self.cfg.max_results.unwrap_or_else(d_max),
        );
        match self.http.get(&url).send().await {
            Ok(r) => r.text().await.ok(),
            Err(e) => {
                tracing::warn!(error = %e, "arxiv: fetch failed; keeping last state");
                None
            }
        }
    }

    fn publish(&self, items: &[Paper]) {
        let update = WidgetUpdate {
            widget_id: ARXIV_WIDGET_ID.into(),
            widget_type: ARXIV_WIDGET_TYPE.into(),
            state: serde_json::json!({
                "new_count": items.len(),
                "items": items,
            }),
            status: WidgetStatus::Normal,
            escalation: Default::default(),
        };
        if let Err(e) = self.publisher.try_send(DaemonMessage::WidgetUpdate(update)) {
            tracing::warn!(error = %e, "arxiv: publish drop");
        }
    }

    async fn poll(&mut self) {
        if self.cfg.dormant() {
            return;
        }
        let Some(xml) = self.fetch().await else {
            return;
        };
        let papers = parse_atom(&xml);
        let fresh: Vec<Paper> = papers
            .into_iter()
            .filter(|p| !self.seen.contains(&p.id))
            .collect();
        for p in &fresh {
            self.seen.insert(p.id.clone());
        }
        if !fresh.is_empty() {
            self.persist_seen();
            self.last_new = fresh;
        }
        // Always publish the current "new since last ack" set so the
        // badge clears correctly after the user opens the dropdown.
        let snapshot = self.last_new.clone();
        self.publish(&snapshot);
    }
}

/// Strip the handful of XML entities arXiv emits and collapse the
/// whitespace it wraps titles/summaries with.
fn unescape(s: &str) -> String {
    let s = s
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tag<'a>(block: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let s = block.find(&open)? + open.len();
    let e = block[s..].find(&close)? + s;
    Some(&block[s..e])
}

/// Hand parse the arXiv Atom feed — we need five fields per entry, not a
/// general XML tree, so no xml crate is pulled in (same call as the
/// recent-docs XBEL reader).
fn parse_atom(xml: &str) -> Vec<Paper> {
    let mut out = Vec::new();
    for chunk in xml.split("<entry>").skip(1) {
        let Some(end) = chunk.find("</entry>") else {
            continue;
        };
        let block = &chunk[..end];
        let Some(id) = tag(block, "id") else { continue };
        let id = id.trim().to_string();
        // arXiv ids look like http://arxiv.org/abs/2401.01234v1 →
        // the PDF is the same with /abs/ swapped for /pdf/.
        let url = id.replace("/abs/", "/pdf/");
        out.push(Paper {
            id,
            title: tag(block, "title").map(unescape).unwrap_or_default(),
            summary: tag(block, "summary")
                .map(unescape)
                .map(|s| s.chars().take(280).collect())
                .unwrap_or_default(),
            url,
            published: tag(block, "published")
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
        });
    }
    out
}

#[async_trait]
impl Module for ArxivWatchModule {
    fn name(&self) -> &str {
        MODULE_NAME
    }

    fn widgets(&self) -> Vec<WidgetDescriptor> {
        vec![WidgetDescriptor {
            id: ARXIV_WIDGET_ID.into(),
            widget_type: ARXIV_WIDGET_TYPE.into(),
        }]
    }

    fn tick_interval(&self) -> Option<StdDuration> {
        Some(StdDuration::from_secs(
            self.cfg.poll_minutes.unwrap_or_else(d_poll) * 60,
        ))
    }

    fn subscribed_events(&self) -> Vec<EventKind> {
        vec![EventKind::WidgetActionReceived]
    }

    async fn start(&mut self) -> ModuleResult<()> {
        self.poll().await;
        Ok(())
    }

    /// Dropdown rows send `arxiv-watch open url=<pdf>`; open it. An
    /// `ack` clears the badge once the user has looked.
    async fn on_event(&mut self, event: &Event) -> ModuleResult<()> {
        if let Event::WidgetActionReceived {
            widget_id,
            action,
            data,
        } = event
        {
            if widget_id != ARXIV_WIDGET_ID {
                return Ok(());
            }
            match action.as_str() {
                "open" => {
                    if let Some(url) = serde_json::from_str::<serde_json::Value>(data)
                        .ok()
                        .and_then(|v| v.get("url").and_then(|u| u.as_str()).map(str::to_owned))
                    {
                        if let Err(e) = crate::palette::spawn_detached("xdg-open", &[&url]) {
                            tracing::warn!(error = %e, "arxiv: open failed");
                        }
                    }
                }
                "ack" => {
                    self.last_new.clear();
                    self.publish(&[]);
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn tick(&mut self) -> ModuleResult<()> {
        self.poll().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dormant_without_keywords_or_authors() {
        assert!(ArxivConfig::default().dormant());
        let c = ArxivConfig {
            keywords: vec!["transformers".into()],
            ..Default::default()
        };
        assert!(!c.dormant());
    }

    #[test]
    fn query_builds_or_of_quoted_terms() {
        let c = ArxivConfig {
            keywords: vec!["sparse attention".into()],
            authors: vec!["Vaswani".into()],
            ..Default::default()
        };
        assert_eq!(c.query(), "all:%22sparse+attention%22+OR+au:%22Vaswani%22");
    }

    #[test]
    fn parse_atom_extracts_entries_and_pdf_url() {
        let xml = r#"<feed><title>arXiv</title>
<entry>
  <id>http://arxiv.org/abs/2401.00001v1</id>
  <title>A   Great
  Paper</title>
  <summary>We show &amp; prove things.</summary>
  <published>2024-01-02T00:00:00Z</published>
</entry>
<entry>
  <id>http://arxiv.org/abs/2401.00002v2</id>
  <title>Second</title>
  <summary>x</summary>
  <published>2024-01-03T00:00:00Z</published>
</entry>
</feed>"#;
        let p = parse_atom(xml);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].title, "A Great Paper");
        assert_eq!(p[0].summary, "We show & prove things.");
        assert_eq!(p[0].url, "http://arxiv.org/pdf/2401.00001v1");
        assert_eq!(p[1].id, "http://arxiv.org/abs/2401.00002v2");
    }
}
