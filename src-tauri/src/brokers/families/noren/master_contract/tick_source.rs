//! Tick sizes from another member's symbol files (web
//! `broker/flattrade/database/master_contract_db.py`
//! `download_shoonya_tick_sizes` and `apply_shoonya_tick_sizes`,
//! marketcalls/openalgo#2508, 702153110).
//!
//! Flattrade's scrip master files carry no tick size, so every row got a
//! fixed tick (0.05, 0.0025 on CDS), and the chart and order ticket offered
//! prices Flattrade refuses: NATGASMINI on MCX ticks in 0.10, and 334.55
//! was answered "Price is not a multiple of tick size 0.10". Shoonya runs
//! the same Noren OMS and publishes the exchange tick per token, so a
//! member with a `tick_source` takes each row's tick from that member's
//! files, matched on exchange and token. Rows the source does not list
//! keep their own tick, and the source failing never fails the download:
//! it is a correction.
//!
//! Desktop: the source's files are read as the source itself reads them
//! (its `TickRule`). Shoonya's NSE and BSE files give the tick in paise,
//! which the web's own Shoonya master divides by 100 (web issue #915,
//! 79b98b132); the web's Flattrade reads the same files as rupees, so a
//! stock ticking in 0.05 is stored there with a tick of 5. That is a web
//! defect and is not copied.

use super::{fetch_file, tick_size, NorenBroker, NorenConfig};
use crate::brokers::common::master_contract::{split_csv_line, CsvHeader};
use crate::brokers::common::symbols::SymToken;
use crate::error::Result;
use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;
use tokio_util::task::AbortOnDropHandle;

/// How long the download waits for the source's files once the member's
/// own files are in: the web's `_TICK_WAIT_SECONDS`, which it starts only
/// after downloading and processing Flattrade's files. Files not read by
/// then leave every row its own tick.
///
/// Each file of the source is fetched as the member's own are
/// (`fetch_file`): the web's `requests.get(url, timeout=30)` limits the
/// connect and each read, not the whole file, and the shared client
/// applies that to every request (30 s per read; 10 s to connect, where
/// the web gives 30). The whole file has `DOWNLOAD_TIMEOUT`, the budget
/// Shoonya's own master download gives these same files; the web sets no
/// such limit.
const WAIT: Duration = Duration::from_secs(120);

/// The exchange tick of each token, by exchange (`NSE`, `NFO`, ...), then
/// token, as the source's files give them.
pub(super) type Ticks = HashMap<String, HashMap<String, f64>>;

/// The member's own download (`own`) and its tick source's ticks, read
/// beside it: none when it has no source, or when the files were not read
/// in time (logged). A refused download takes no ticks.
pub(super) async fn beside<T>(
    b: &NorenBroker,
    own: impl Future<Output = Result<T>>,
) -> Result<(T, Ticks)> {
    let Some(source) = b.cfg.tick_source else {
        return Ok((own.await?, Ticks::new()));
    };
    let read = read_files(
        b.http.clone(),
        b.cfg.id,
        source,
        b.endpoints.tick_urls.clone(),
    );
    let (own, ticks) = then_wait(own, read).await?;
    let ticks = ticks.unwrap_or_else(|| {
        tracing::warn!(
            broker = b.cfg.id,
            "Tick sizes: {}'s files were not ready; keeping the default ticks",
            source.name
        );
        Ticks::new()
    });
    Ok((own, ticks))
}

/// `own`, with `read` running beside it in a task of its own (the web's one
/// tick worker), so parsing the member's files never holds the source's
/// download up; then `WAIT` more for `read` once `own` is done, which is
/// when the web starts its wait. `None` when `read` was not done by then
/// or ended without an answer. The task ends with this call: `own`
/// failing, the wait running out or the download being dropped aborts it.
async fn then_wait<T, R: Send + 'static>(
    own: impl Future<Output = Result<T>>,
    read: impl Future<Output = R> + Send + 'static,
) -> Result<(T, Option<R>)> {
    let mut read = AbortOnDropHandle::new(tokio::spawn(read));
    let own = own.await?;
    let read = tokio::time::timeout(WAIT, &mut read)
        .await
        .ok()
        .and_then(|joined| joined.ok());
    Ok((own, read))
}

/// Each of the source's files in turn, as the web's one worker fetches
/// them. A file that fails or cannot be read is logged and skipped: its
/// segment keeps the member's own tick.
async fn read_files(
    http: reqwest::Client,
    broker: &'static str,
    source: &'static NorenConfig,
    urls: Vec<String>,
) -> Ticks {
    let mut ticks = Ticks::new();
    for (file, url) in source.master_files.iter().zip(&urls) {
        let Some(text) = fetch_file(&http, broker, file, url, "Tick file").await else {
            continue;
        };
        match parse_into(source, &text, &mut ticks) {
            Some(read) => tracing::info!(
                broker,
                "Tick sizes: {} {} ticks read from {}",
                read,
                file.exchange,
                source.name
            ),
            None => tracing::warn!(
                broker,
                "Tick sizes: {}'s {} file has no Exchange, Token and TickSize columns",
                source.name,
                file.exchange
            ),
        }
    }
    ticks
}

/// Add one of the source's files to `ticks`, from its `Exchange`, `Token`
/// and `TickSize` columns, and say how many rows gave a tick; `None` when
/// the file lacks one of those columns. A tick of zero or less (Shoonya
/// gives indices 0) or one that cannot be read is not a tick and is left
/// out.
pub(super) fn parse_into(source: &NorenConfig, text: &str, ticks: &mut Ticks) -> Option<usize> {
    let mut lines = text.lines();
    let head = CsvHeader::parse(lines.next()?);
    let exchange_col = head.index("Exchange")?;
    let token_col = head.index("Token")?;
    let tick_col = head.index("TickSize")?;
    let mut read = 0;
    for line in lines {
        let cells = split_csv_line(line);
        let cell = |i: usize| cells.get(i).map_or("", |s| s.trim());
        let (exchange, token) = (cell(exchange_col), cell(token_col));
        let tick = tick_size(source.tick_rule, exchange, cell(tick_col));
        if exchange.is_empty() || token.is_empty() || !(tick > 0.0 && tick.is_finite()) {
            continue;
        }
        ticks
            .entry(exchange.to_string())
            .or_default()
            .insert(token.to_string(), tick);
        read += 1;
    }
    Some(read)
}

/// Rows of one segment: how many took the source's tick.
struct Matched {
    segment: String,
    matched: usize,
    rows: usize,
}

/// Set each row's tick from `ticks` where its exchange and token match,
/// an index row by its exchange less `_INDEX`; the rest keep the tick
/// their file gave. One line per segment says how many matched. Nothing
/// changes for a member without a tick source.
pub(super) fn apply(cfg: &NorenConfig, rows: &mut [SymToken], ticks: &Ticks) {
    let Some(source) = cfg.tick_source else {
        return;
    };
    let mut segments: Vec<Matched> = Vec::new();
    for row in rows.iter_mut() {
        let segment = row
            .brexchange
            .strip_suffix("_INDEX")
            .unwrap_or(&row.brexchange);
        let tick = ticks
            .get(segment)
            .and_then(|by_token| by_token.get(row.token.as_str()));
        let at = match segments.iter().position(|s| s.segment == segment) {
            Some(at) => at,
            None => {
                segments.push(Matched {
                    segment: segment.to_string(),
                    matched: 0,
                    rows: 0,
                });
                segments.len() - 1
            }
        };
        segments[at].rows += 1;
        if let Some(&tick) = tick {
            row.tick_size = tick;
            segments[at].matched += 1;
        }
    }
    for s in &segments {
        tracing::info!(
            broker = cfg.id,
            "Tick sizes: {} {} of {} rows matched {}; the rest keep the default tick",
            s.segment,
            s.matched,
            s.rows,
            source.name
        );
    }
}

#[cfg(test)]
mod tests;
