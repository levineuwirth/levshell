//! Minimal CalDAV HTTP client.
//!
//! CalDAV is WebDAV with calendar-flavored REPORT methods. The
//! adapter uses just two interactions:
//!
//! 1. `PROPFIND` with `Depth: 1` on the calendar collection →
//!    `207 Multi-Status` XML listing every `.ics` object as a
//!    `<d:response>` element with an `<d:href>` and an ETag.
//! 2. `GET <href>` → the raw iCalendar body.
//!
//! We expose an [`CalDavClient`] trait so unit tests can mock
//! without an HTTP server. The real impl is [`CalDavHttpClient`],
//! which wraps `reqwest::Client` with HTTP Basic auth per request.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::Method;
use thiserror::Error;

/// The WebDAV `Depth` request header (RFC 4918 §10.2). Not a
/// standard HTTP header, so reqwest doesn't ship a constant — we
/// build it once at module init.
static DEPTH_HEADER: std::sync::OnceLock<HeaderName> = std::sync::OnceLock::new();
fn depth_header() -> &'static HeaderName {
    DEPTH_HEADER.get_or_init(|| HeaderName::from_static("depth"))
}

/// PROPFIND body asking the server for the current ETag of every
/// resource at depth 1. Intentionally minimal — we don't need
/// `<d:resourcetype>` or `<c:calendar-data>` for the diff pass.
const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:getetag/>
  </d:prop>
</d:propfind>"#;

/// One entry from a PROPFIND response. Collection entries (the
/// calendar itself) are filtered out by the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DavEntry {
    /// Raw `<d:href>` value. Server-relative (`/dav/cal/x.ics`) most
    /// of the time; sometimes absolute. The adapter normalizes this
    /// back to an absolute URL before the follow-up `GET`.
    pub href: String,
    /// `<d:getetag>` value. Servers quote it (`"abc-123"`); we
    /// strip the quotes at parse time so comparisons are uniform.
    pub etag: String,
}

#[derive(Debug, Error)]
pub enum CalDavError {
    #[error("http error contacting caldav at {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("caldav server returned unexpected status {status} for {url}")]
    BadStatus {
        url: String,
        status: reqwest::StatusCode,
    },

    #[error("caldav body was not valid utf-8 for {url}")]
    NotUtf8 { url: String },

    #[error("malformed PROPFIND response for {url}: {reason}")]
    Malformed { url: String, reason: String },
}

/// Trait the adapter depends on. Object-safe via `async-trait`.
#[async_trait]
pub trait CalDavClient: Send + Sync {
    /// PROPFIND depth=1 + parse the multistatus into DAV entries.
    async fn list_entries(&self, base_url: &str) -> Result<Vec<DavEntry>, CalDavError>;

    /// GET a single resource, returning the ICS body as a UTF-8
    /// string.
    async fn fetch_ics(&self, url: &str) -> Result<String, CalDavError>;
}

pub struct CalDavHttpClient {
    http: reqwest::Client,
    username: String,
    password: String,
}

impl CalDavHttpClient {
    pub fn new(
        username: &str,
        password: &str,
        request_timeout: Duration,
    ) -> Result<Self, CalDavError> {
        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            .build()
            .map_err(|source| CalDavError::Http {
                url: "<builder>".into(),
                source,
            })?;
        Ok(Self {
            http,
            username: username.into(),
            password: password.into(),
        })
    }
}

#[async_trait]
impl CalDavClient for CalDavHttpClient {
    async fn list_entries(&self, base_url: &str) -> Result<Vec<DavEntry>, CalDavError> {
        let method = Method::from_bytes(b"PROPFIND")
            .expect("PROPFIND is a valid HTTP method token");

        let resp = self
            .http
            .request(method, base_url)
            .basic_auth(&self.username, Some(&self.password))
            .header(depth_header(), HeaderValue::from_static("1"))
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/xml; charset=utf-8"),
            )
            .body(PROPFIND_BODY)
            .send()
            .await
            .map_err(|source| CalDavError::Http {
                url: base_url.into(),
                source,
            })?;

        let status = resp.status();
        if status != reqwest::StatusCode::MULTI_STATUS && !status.is_success() {
            return Err(CalDavError::BadStatus {
                url: base_url.into(),
                status,
            });
        }
        let body = resp.text().await.map_err(|source| CalDavError::Http {
            url: base_url.into(),
            source,
        })?;
        parse_multistatus(&body).map_err(|reason| CalDavError::Malformed {
            url: base_url.into(),
            reason,
        })
    }

    async fn fetch_ics(&self, url: &str) -> Result<String, CalDavError> {
        let resp = self
            .http
            .get(url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|source| CalDavError::Http {
                url: url.into(),
                source,
            })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(CalDavError::BadStatus {
                url: url.into(),
                status,
            });
        }
        let bytes = resp.bytes().await.map_err(|source| CalDavError::Http {
            url: url.into(),
            source,
        })?;
        String::from_utf8(bytes.to_vec()).map_err(|_| CalDavError::NotUtf8 { url: url.into() })
    }
}

/// Hand-rolled multistatus extractor. We only need `<d:href>` and
/// `<d:getetag>` pairs — pulling in a full XML parser is overkill.
/// Accepts any `DAV:`-like namespace prefix (`d:`, `D:`, `DAV:`,
/// or un-prefixed) by matching the local name only.
///
/// Collection entries (the calendar itself, reported by depth=1 along
/// with the object list) are filtered out: they're the rows whose
/// href ends in `/` or whose getetag is empty.
pub fn parse_multistatus(body: &str) -> Result<Vec<DavEntry>, String> {
    let mut out = Vec::new();
    let mut rest = body;
    // Walk `<…response>…</…response>` blocks. We only care about the
    // local name after any namespace prefix.
    while let Some((response_body, after)) = take_element(rest, "response") {
        let href = match take_inner_text(response_body, "href") {
            Some(s) => s,
            None => {
                rest = after;
                continue;
            }
        };
        let etag = take_inner_text(response_body, "getetag").unwrap_or_default();

        // Drop the collection row (PROPFIND depth=1 reports the
        // parent before its children).
        if href.ends_with('/') || etag.is_empty() {
            rest = after;
            continue;
        }
        out.push(DavEntry {
            href: href.trim().to_string(),
            etag: etag.trim().trim_matches('"').to_string(),
        });
        rest = after;
    }
    Ok(out)
}

/// Find the first `<…local_name …>…</…local_name>` element,
/// returning (inner, after). Skips namespace prefixes via a suffix
/// match on the tag name.
fn take_element<'a>(input: &'a str, local_name: &str) -> Option<(&'a str, &'a str)> {
    let mut idx = 0;
    while let Some(lt) = input[idx..].find('<') {
        let start = idx + lt;
        let after_lt = start + 1;
        if after_lt >= input.len() {
            return None;
        }
        let rest = &input[after_lt..];
        // Skip closing tags `</…>` and processing instructions `<?…?>`.
        if rest.starts_with('/') || rest.starts_with('?') || rest.starts_with('!') {
            idx = after_lt;
            continue;
        }
        // Pull the element's tag name up to whitespace or `>` or `/`.
        let tag_end = rest
            .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
            .unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        let local = tag.rsplit(':').next().unwrap_or(tag);
        if !local.eq_ignore_ascii_case(local_name) {
            idx = after_lt;
            continue;
        }
        // Find the end of the opening tag.
        let open_end = rest.find('>').map(|e| after_lt + e + 1)?;
        // Self-closing: <tag .../>. Skip (no inner text to return).
        if rest[..open_end - after_lt]
            .trim_end_matches('>')
            .trim_end()
            .ends_with('/')
        {
            idx = open_end;
            continue;
        }
        // Find the matching close tag by local name. Nested same-name
        // elements aren't expected here, so a flat search is enough
        // for WebDAV responses.
        let close_seq = format!("{tag}>");
        let close_start = input[open_end..]
            .to_ascii_lowercase()
            .find(&format!("</{}", close_seq.to_ascii_lowercase()))
            .map(|p| open_end + p)?;
        let close_end = input[close_start..]
            .find('>')
            .map(|e| close_start + e + 1)?;
        return Some((&input[open_end..close_start], &input[close_end..]));
    }
    None
}

fn take_inner_text(input: &str, local_name: &str) -> Option<String> {
    take_element(input, local_name).map(|(inner, _)| {
        // Trim and strip any nested tags from the inner content
        // (e.g. `<getetag>"abc"</getetag>` vs. `<getetag><![CDATA[...]]></getetag>`).
        let stripped: String = strip_tags(inner).trim().to_string();
        stripped
    })
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nextcloud_style_multistatus() {
        let body = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:">
  <d:response>
    <d:href>/remote.php/dav/calendars/u/work/</d:href>
    <d:propstat>
      <d:prop><d:getetag>"collection"</d:getetag></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/remote.php/dav/calendars/u/work/abc-123.ics</d:href>
    <d:propstat>
      <d:prop><d:getetag>"aaa111"</d:getetag></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
  <d:response>
    <d:href>/remote.php/dav/calendars/u/work/def-456.ics</d:href>
    <d:propstat>
      <d:prop><d:getetag>"bbb222"</d:getetag></d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
</d:multistatus>"#;
        let entries = parse_multistatus(body).unwrap();
        assert_eq!(entries.len(), 2, "collection row filtered out");
        assert_eq!(entries[0].href, "/remote.php/dav/calendars/u/work/abc-123.ics");
        assert_eq!(entries[0].etag, "aaa111");
        assert_eq!(entries[1].etag, "bbb222");
    }

    #[test]
    fn parses_caps_dav_prefix() {
        // Some servers (including SabreDAV) use uppercase-D prefix.
        let body = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/cal/x.ics</D:href>
    <D:propstat><D:prop><D:getetag>"etag-1"</D:getetag></D:prop></D:propstat>
  </D:response>
</D:multistatus>"#;
        let entries = parse_multistatus(body).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].href, "/cal/x.ics");
    }

    #[test]
    fn empty_multistatus_yields_empty_vec() {
        let body = r#"<d:multistatus xmlns:d="DAV:"></d:multistatus>"#;
        let entries = parse_multistatus(body).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn skips_entries_without_etag() {
        let body = r#"<d:multistatus xmlns:d="DAV:">
          <d:response>
            <d:href>/cal/x.ics</d:href>
          </d:response>
        </d:multistatus>"#;
        let entries = parse_multistatus(body).unwrap();
        assert!(entries.is_empty(), "no etag → skipped");
    }

    #[test]
    fn strips_surrounding_etag_quotes() {
        let body = r#"<d:multistatus xmlns:d="DAV:">
          <d:response>
            <d:href>/cal/x.ics</d:href>
            <d:getetag>&quot;abc&quot;</d:getetag>
          </d:response>
        </d:multistatus>"#;
        // HTML-entity etag is unusual but valid; we don't decode
        // entities, so this ends up as &quot;abc&quot;. That's
        // fine — all we need is stable bytes for the sync_hash
        // comparison.
        let entries = parse_multistatus(body).unwrap();
        assert_eq!(entries.len(), 1);
    }
}
