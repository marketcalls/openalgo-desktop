//! What one run of the toolkit remembers, as the web's toolkit does per
//! run: the URLs its searches returned (the only ones `web_fetch` accepts
//! besides the trader's own), the pages it read (so reading on with
//! `offset` costs nothing), and the preferred sources whose page could not
//! be read (skipped from then on, so one blocked site costs one failed
//! call and the next source carries on).
//!
//! Bounded by the run: every URL comes from a search the turn's budget
//! paid for, at most [`MAX_PAGES_PER_RUN`] pages are kept, each cut to
//! [`MAX_PAGE_CHARS`], and all of it goes with the run's tools.

use crate::agent::websearch::fetch_url::url_key;
use parking_lot::Mutex;
use std::collections::HashMap;

/// The most of one page's text kept (`MAX_PAGE_CHARS`).
pub(super) const MAX_PAGE_CHARS: usize = 400_000;
/// The most pages kept for one run (`MAX_PAGES_PER_RUN`).
pub(super) const MAX_PAGES_PER_RUN: usize = 8;

/// A page a provider read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Page {
    pub provider: &'static str,
    pub text: String,
    pub title: String,
    pub final_url: String,
}

#[derive(Default)]
struct Inner {
    /// `url_key` of a returned URL, to the URL exactly as returned.
    seen: HashMap<String, String>,
    pages: HashMap<String, Page>,
    failed_sources: Vec<String>,
}

#[derive(Default)]
pub(super) struct RunState(Mutex<Inner>);

impl RunState {
    /// Note a URL a search returned, exactly as returned, without its
    /// fragment (`_remember_url`).
    pub(super) fn remember(&self, url: &str) {
        let exact = url.trim().split('#').next().unwrap_or_default();
        if exact.is_empty() {
            return;
        }
        if let Some(key) = url_key(exact) {
            self.0.lock().seen.insert(key, exact.to_string());
        }
    }

    /// The URLs remembered so far.
    pub(super) fn seen(&self) -> HashMap<String, String> {
        self.0.lock().seen.clone()
    }

    /// A page already read this run.
    pub(super) fn page(&self, url: &str) -> Option<Page> {
        let key = url_key(url)?;
        self.0.lock().pages.get(&key).cloned()
    }

    /// Keep a page for reading on, while there is room.
    pub(super) fn keep(&self, url: &str, page: &Page) {
        let Some(key) = url_key(url) else {
            return;
        };
        let mut inner = self.0.lock();
        if inner.pages.len() < MAX_PAGES_PER_RUN {
            inner.pages.insert(key, page.clone());
        }
    }

    /// A preferred source whose page could not be read.
    pub(super) fn fail_source(&self, domain: &str) {
        let mut inner = self.0.lock();
        if !inner.failed_sources.iter().any(|d| d == domain) {
            inner.failed_sources.push(domain.to_string());
        }
    }

    /// The preferred sources that have not failed this run, best first.
    pub(super) fn live_sources(&self, preferred: &[String]) -> Vec<String> {
        let inner = self.0.lock();
        preferred
            .iter()
            .filter(|d| !inner.failed_sources.contains(d))
            .cloned()
            .collect()
    }
}

/// The first `MAX_PAGE_CHARS` characters of a page's text.
pub(super) fn bounded_text(text: &str) -> String {
    match text.char_indices().nth(MAX_PAGE_CHARS) {
        Some((cut, _)) => text[..cut].to_string(),
        None => text.to_string(),
    }
}
