//! Zotero SQLite reader.
//!
//! Pure synchronous functions over a `rusqlite::Connection` that turn
//! the Zotero schema into a flat [`RawItem`] list. Called from inside a
//! `spawn_blocking` so the async adapter can await on the result.
//!
//! ## Zotero schema (the useful subset)
//!
//! ```text
//! items(itemID, itemTypeID, dateModified, libraryID, key)
//! itemTypes(itemTypeID, typeName)                -- 'journalArticle', ...
//! itemData(itemID, fieldID, valueID)             -- per-item field map
//! fields(fieldID, fieldName)                     -- 'title', 'date', ...
//! itemDataValues(valueID, value)                 -- actual string values
//! creators(creatorID, firstName, lastName, fieldMode)
//! itemCreators(itemID, creatorID, creatorTypeID, orderIndex)
//! creatorTypes(creatorTypeID, creatorType)       -- 'author', 'editor', ...
//! tags(tagID, name)
//! itemTags(itemID, tagID)
//! deletedItems(itemID)                           -- items in the trash
//! itemAttachments(itemID, parentItemID, contentType, path)
//! ```
//!
//! We skip items in `deletedItems`, plus `attachment` and `note` types
//! (Zotero notes are not our Note entity — they're scratch inside a
//! reference, Obsidian owns user-authored notes).

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::ZoteroError;

/// A single Zotero item translated into the subset of fields we care
/// about. Populated by [`read_items`]. Attachments are folded into
/// `pdf_path` on the parent; notes are dropped entirely.
///
/// `item_type` is read for diagnostics (and to let the filter here live
/// next to the read). The Reference model doesn't yet carry a URL
/// column, so `url` is currently dead — kept on the raw row so adding
/// that column later is a one-line wire-up.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct RawItem {
    pub library_id: i64,
    /// 8-char uppercase item key. Used as `sync_metadata.external_id`
    /// and as the citekey fallback.
    pub key: String,
    /// RFC3339-ish string straight from Zotero. Opaque — we only use it
    /// as the sync_hash to detect changes, never parse it.
    pub date_modified: String,
    pub item_type: String,

    pub title: Option<String>,
    pub date: Option<String>,
    pub doi: Option<String>,
    pub url: Option<String>,
    pub publication_title: Option<String>,
    pub abstract_note: Option<String>,
    /// Raw `extra` field (Zotero's catch-all). BBT stores
    /// `Citation Key: xyz` here; we parse it in
    /// [`crate::zotero::mod_citekey_from_extra`].
    pub extra: Option<String>,

    pub creators: Vec<RawCreator>,
    pub tags: Vec<String>,
    /// Relative `attachments/...` path from the first PDF child
    /// attachment, if any. Storage-type attachments get a `storage:`
    /// prefix preserved verbatim.
    pub pdf_path: Option<String>,
    /// PDF highlight / note annotations (Zotero 6+ `itemAnnotations`),
    /// flattened to `highlight — comment` strings. Empty on older
    /// Zotero (no such table) or items without an annotated attachment.
    pub annotations: Vec<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct RawCreator {
    pub first_name: String,
    pub last_name: String,
    pub creator_type: String,
}

impl RawCreator {
    /// Human-readable rendering. Zotero stores single-name creators
    /// (institutions, Mononyms) with `fieldMode=1` and an empty
    /// first_name — we collapse those to just `last_name`.
    pub fn display(&self) -> String {
        if self.first_name.is_empty() {
            self.last_name.clone()
        } else {
            format!("{} {}", self.first_name, self.last_name)
        }
    }
}

/// Open `path` read-only. Uses `SQLITE_OPEN_READ_ONLY` so a running
/// Zotero instance can't see us as a writer and we can't accidentally
/// modify its database.
pub(crate) fn open_readonly(path: &Path) -> Result<Connection, ZoteroError> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| ZoteroError::Open {
        path: path.to_path_buf(),
        source,
    })
}

/// Pull every non-trashed, non-attachment, non-note item out of the
/// Zotero database in four bulk queries, then stitch them together in
/// memory. Cheap even for 50k-item libraries.
pub(crate) fn read_items(conn: &Connection) -> Result<Vec<RawItem>, ZoteroError> {
    let mut items_by_id: HashMap<i64, RawItem> = HashMap::new();

    // Query 1: candidate items. Exclude trash, attachments, and notes.
    {
        let mut stmt = conn.prepare(
            "SELECT items.itemID, items.libraryID, items.key, items.dateModified, \
                    itemTypes.typeName \
             FROM items \
             JOIN itemTypes ON itemTypes.itemTypeID = items.itemTypeID \
             LEFT JOIN deletedItems ON deletedItems.itemID = items.itemID \
             WHERE deletedItems.itemID IS NULL \
               AND itemTypes.typeName NOT IN ('attachment', 'note')",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (id, library_id, key, date_modified, item_type) = row?;
            items_by_id.insert(
                id,
                RawItem {
                    library_id,
                    key,
                    date_modified,
                    item_type,
                    title: None,
                    date: None,
                    doi: None,
                    url: None,
                    publication_title: None,
                    abstract_note: None,
                    extra: None,
                    creators: Vec::new(),
                    tags: Vec::new(),
                    pdf_path: None,
                    annotations: Vec::new(),
                },
            );
        }
    }

    // Query 2: all field values for those items. We filter by the
    // fieldNames we actually consume — other Zotero fields (language,
    // ISBN, etc.) are dropped for v1.
    {
        let mut stmt = conn.prepare(
            "SELECT itemData.itemID, fields.fieldName, itemDataValues.value \
             FROM itemData \
             JOIN fields ON fields.fieldID = itemData.fieldID \
             JOIN itemDataValues ON itemDataValues.valueID = itemData.valueID \
             WHERE fields.fieldName IN \
                 ('title', 'date', 'DOI', 'url', 'publicationTitle', \
                  'conferenceName', 'proceedingsTitle', 'bookTitle', \
                  'abstractNote', 'extra')",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (id, field, value) = row?;
            let Some(item) = items_by_id.get_mut(&id) else {
                continue;
            };
            match field.as_str() {
                "title" => item.title = Some(value),
                "date" => item.date = Some(value),
                "DOI" => item.doi = Some(value),
                "url" => item.url = Some(value),
                // Zotero splits venue by item type; we collapse the
                // three common ones into publication_title, first
                // non-empty wins. publicationTitle (journal articles)
                // is most common so we let it overwrite only if empty.
                "publicationTitle" | "conferenceName" | "proceedingsTitle" | "bookTitle"
                    if item.publication_title.is_none() => {
                        item.publication_title = Some(value);
                    }
                "abstractNote" => item.abstract_note = Some(value),
                "extra" => item.extra = Some(value),
                _ => {}
            }
        }
    }

    // Query 3: creators in order. We include every creatorType so
    // translators / editors still populate `authors` — downstream
    // consumers can filter by type if they care.
    {
        let mut stmt = conn.prepare(
            "SELECT itemCreators.itemID, creators.firstName, creators.lastName, \
                    creatorTypes.creatorType \
             FROM itemCreators \
             JOIN creators ON creators.creatorID = itemCreators.creatorID \
             JOIN creatorTypes ON creatorTypes.creatorTypeID = itemCreators.creatorTypeID \
             ORDER BY itemCreators.itemID, itemCreators.orderIndex",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, first_name, last_name, creator_type) = row?;
            if let Some(item) = items_by_id.get_mut(&id) {
                item.creators.push(RawCreator {
                    first_name,
                    last_name,
                    creator_type,
                });
            }
        }
    }

    // Query 4: tags.
    {
        let mut stmt = conn.prepare(
            "SELECT itemTags.itemID, tags.name \
             FROM itemTags \
             JOIN tags ON tags.tagID = itemTags.tagID",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, name) = row?;
            if let Some(item) = items_by_id.get_mut(&id) {
                item.tags.push(name);
            }
        }
    }

    // Query 5: first PDF attachment per parent. Zotero can have multiple
    // attachments per item; we pick the first PDF deterministically by
    // itemID order. Non-PDF attachments are ignored.
    {
        let mut stmt = conn.prepare(
            "SELECT itemAttachments.parentItemID, itemAttachments.path \
             FROM itemAttachments \
             LEFT JOIN deletedItems \
                 ON deletedItems.itemID = itemAttachments.itemID \
             WHERE itemAttachments.parentItemID IS NOT NULL \
               AND itemAttachments.contentType = 'application/pdf' \
               AND deletedItems.itemID IS NULL \
             ORDER BY itemAttachments.itemID",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })?;
        for row in rows {
            let (parent_id, path) = row?;
            if let Some(item) = items_by_id.get_mut(&parent_id) {
                if item.pdf_path.is_none() {
                    item.pdf_path = path;
                }
            }
        }
    }

    // Query 6: PDF annotations (Zotero 6+). An annotation's
    // `parentItemID` is the *attachment* item; that attachment's
    // `parentItemID` is the reference. Best-effort: the table is absent
    // on Zotero < 6, and per spec §5.1 a sync adapter must never fail
    // the whole sync over a tool-schema gap — so a missing table /
    // malformed row is logged and skipped, not propagated.
    match conn.prepare(
        "SELECT att.parentItemID, ia.text, ia.comment \
         FROM itemAnnotations ia \
         JOIN itemAttachments att ON att.itemID = ia.parentItemID \
         LEFT JOIN deletedItems d ON d.itemID = ia.itemID \
         WHERE att.parentItemID IS NOT NULL AND d.itemID IS NULL \
         ORDER BY att.parentItemID, ia.sortIndex",
    ) {
        Ok(mut stmt) => {
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            });
            match rows {
                Ok(rows) => {
                    for row in rows.flatten() {
                        let (ref_id, text, comment) = row;
                        let Some(item) = items_by_id.get_mut(&ref_id) else {
                            continue;
                        };
                        let text = text.unwrap_or_default();
                        let comment = comment.unwrap_or_default();
                        let entry = match (text.trim(), comment.trim()) {
                            ("", "") => continue,
                            (t, "") => t.to_string(),
                            ("", c) => c.to_string(),
                            (t, c) => format!("{t} — {c}"),
                        };
                        item.annotations.push(entry);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "zotero: annotation rows unreadable; skipping"),
            }
        }
        Err(e) => tracing::debug!(
            error = %e,
            "zotero: itemAnnotations unavailable (pre-6 schema?); no annotations imported"
        ),
    }

    // Stable output order by item key so sync-metadata churn is
    // predictable between runs (the upsert pass only writes on change,
    // but deterministic iteration helps test assertions).
    let mut items: Vec<RawItem> = items_by_id.into_values().collect();
    items.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(items)
}

/// Parse `Citation Key: xyz` out of Zotero's `extra` field (the Better
/// BibTeX plugin stores citekeys there). Returns `None` if the header
/// isn't present; upstream then falls back to the Zotero item key.
pub(crate) fn citekey_from_extra(extra: &str) -> Option<String> {
    for line in extra.lines() {
        let trimmed = line.trim();
        // BBT writes "Citation Key:" but older configs used
        // "Citekey:". Accept both, case-insensitive on the header.
        for prefix in ["Citation Key:", "Citekey:", "citation key:", "citekey:"] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let key = rest.trim();
                if !key.is_empty() {
                    return Some(key.to_string());
                }
            }
        }
    }
    None
}

/// Parse a four-digit year out of Zotero's free-form `date` field.
/// Zotero stores dates as the user typed them ("2023", "2023-04",
/// "April 2023", "2023/4/1", ...) so we just scrape the first
/// four-digit run that looks like a plausible year.
pub(crate) fn year_from_date(date: &str) -> Option<i32> {
    let mut chars = date.chars().peekable();
    while chars.peek().is_some() {
        let run: String = chars
            .by_ref()
            .take_while(|c| c.is_ascii_digit())
            .take(4)
            .collect();
        if run.len() == 4 {
            if let Ok(y) = run.parse::<i32>() {
                if (1000..=9999).contains(&y) {
                    return Some(y);
                }
            }
        }
        // Skip any non-digit run to the next number.
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                break;
            }
            chars.next();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citekey_from_extra_handles_common_forms() {
        assert_eq!(
            citekey_from_extra("Citation Key: smith2023").as_deref(),
            Some("smith2023")
        );
        assert_eq!(
            citekey_from_extra("DOI: 10.1\nCitation Key: foo\n").as_deref(),
            Some("foo")
        );
        assert_eq!(
            citekey_from_extra("citekey: bar2024").as_deref(),
            Some("bar2024")
        );
        assert_eq!(citekey_from_extra("no keys here").as_deref(), None);
        assert_eq!(citekey_from_extra("Citation Key:").as_deref(), None);
    }

    #[test]
    fn year_from_date_extracts_plausible_year() {
        assert_eq!(year_from_date("2023"), Some(2023));
        assert_eq!(year_from_date("2023-04-01"), Some(2023));
        assert_eq!(year_from_date("April 2023"), Some(2023));
        assert_eq!(year_from_date("2023/4/1"), Some(2023));
        assert_eq!(year_from_date("circa 1985"), Some(1985));
        assert_eq!(year_from_date(""), None);
        assert_eq!(year_from_date("volume 23"), None);
        assert_eq!(year_from_date("99"), None);
    }
}
