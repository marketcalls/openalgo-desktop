//! A long external data answer split into pages (the web's `_pages`,
//! `_json_pages` and `_paginate` in `tools/mcp.py`).
//!
//! A JSON answer is paged by the rows of its largest list, so every page is
//! still a whole, parsed document carrying a slice of that list (three
//! months of daily candles read as pages of rows, not as one escaped string
//! cut mid-row). Anything else is cut into pages of [`PAGE_CHARS`]
//! characters, on line breaks where possible. At most [`MAX_PAGES`]; past
//! that the tail is dropped and the answer says so.

use serde_json::Value;

/// Characters of a text page.
pub const PAGE_CHARS: usize = 7000;
/// Characters of a parsed JSON page, measured compact.
pub const JSON_PAGE_CHARS: usize = 9000;
/// How deep inside the answer's objects the list to page by is looked for.
const MAX_LIST_DEPTH: usize = 3;
/// Most pages an answer is split into.
pub const MAX_PAGES: usize = 20;

/// One page: parsed JSON or text, and for a JSON answer paged by rows,
/// which rows it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub content: Value,
    pub rows: Option<String>,
}

fn compact_len(v: &Value) -> usize {
    serde_json::to_string(v).map_or(0, |s| s.chars().count())
}

/// Text pages of at most [`PAGE_CHARS`], each ending after a line break in
/// its last quarter when there is one.
fn paginate(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= PAGE_CHARS {
        return vec![text.to_string()];
    }
    let mut pages = Vec::new();
    let mut start = 0;
    while start < chars.len() && pages.len() < MAX_PAGES {
        let mut end = (start + PAGE_CHARS).min(chars.len());
        if end < chars.len() {
            let from = start + PAGE_CHARS * 3 / 4;
            if let Some(cut) = (from..end).rev().find(|&i| chars[i] == '\n') {
                if cut > start {
                    end = cut + 1;
                }
            }
        }
        pages.push(chars[start..end].iter().collect());
        start = end;
    }
    pages
}

/// The path to, and the size of, the largest list with at least two items,
/// looked for through objects only, to [`MAX_LIST_DEPTH`] levels.
fn largest_list(value: &Value, path: &mut Vec<String>) -> Option<(Vec<String>, usize)> {
    match value {
        Value::Array(items) if items.len() > 1 => Some((path.clone(), compact_len(value))),
        Value::Object(map) if path.len() < MAX_LIST_DEPTH => {
            let mut best: Option<(Vec<String>, usize)> = None;
            for (key, item) in map {
                path.push(key.clone());
                let found = largest_list(item, path);
                path.pop();
                if let Some(f) = found {
                    if best.as_ref().is_none_or(|b| f.1 > b.1) {
                        best = Some(f);
                    }
                }
            }
            best
        }
        _ => None,
    }
}

/// A copy of `value` with the list at `path` replaced.
fn with_list(value: &Value, path: &[String], items: Vec<Value>) -> Value {
    match (path.split_first(), value) {
        (None, _) => Value::Array(items),
        (Some((head, rest)), Value::Object(map)) => {
            let mut copy = map.clone();
            let inner = map
                .get(head)
                .map_or(Value::Null, |v| with_list(v, rest, items));
            copy.insert(head.clone(), inner);
            Value::Object(copy)
        }
        _ => value.clone(),
    }
}

/// A JSON answer paged by the rows of its largest list, and whether the
/// page cap cut the tail; `None` when it has no list worth paging by, what
/// surrounds the list is itself too large, or one row alone is.
fn json_pages(parsed: &Value) -> Option<(Vec<Page>, bool)> {
    let (path, _) = largest_list(parsed, &mut Vec::new())?;
    let mut rows = parsed;
    for key in &path {
        rows = rows.get(key)?;
    }
    let rows = rows.as_array()?;
    let frame = compact_len(&with_list(parsed, &path, Vec::new()));
    if frame > JSON_PAGE_CHARS / 2 {
        return None;
    }
    let mut chunks: Vec<(usize, usize)> = Vec::new();
    let (mut start, mut used) = (0, frame);
    for (index, row) in rows.iter().enumerate() {
        let cost = compact_len(row) + 1;
        if cost + frame > JSON_PAGE_CHARS {
            return None;
        }
        if index > start && used + cost > JSON_PAGE_CHARS {
            chunks.push((start, index));
            start = index;
            used = frame;
        }
        used += cost;
    }
    chunks.push((start, rows.len()));
    let place = if path.is_empty() {
        "the answer".to_string()
    } else {
        path.join(".")
    };
    let pages = chunks
        .iter()
        .take(MAX_PAGES)
        .map(|&(a, b)| Page {
            content: with_list(parsed, &path, rows[a..b].to_vec()),
            rows: Some(format!(
                "rows {}-{} of {} in {}",
                a + 1,
                b,
                rows.len(),
                place
            )),
        })
        .collect();
    Some((pages, chunks.len() > MAX_PAGES))
}

/// An answer split into pages, by rows when it is JSON and by characters
/// otherwise, and whether [`MAX_PAGES`] cut the tail. A JSON answer that
/// fits one page comes back parsed.
pub fn pages(text: &str) -> (Vec<Page>, bool) {
    let stripped = text.trim();
    if stripped.starts_with(['{', '[']) {
        if let Ok(parsed) = serde_json::from_str::<Value>(stripped) {
            if compact_len(&parsed) <= JSON_PAGE_CHARS {
                return (
                    vec![Page {
                        content: parsed,
                        rows: None,
                    }],
                    false,
                );
            }
            if let Some(paged) = json_pages(&parsed) {
                return paged;
            }
        }
    }
    let chunks = paginate(text);
    let kept: usize = chunks.iter().map(|c| c.chars().count()).sum();
    let truncated = kept < text.chars().count();
    (
        chunks
            .into_iter()
            .map(|c| Page {
                content: Value::String(c),
                rows: None,
            })
            .collect(),
        truncated,
    )
}
