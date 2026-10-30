//! The data family's tests (the web executor's goldens replay in
//! `flow::goldens::goldens_data`): the web's data-node tests ported (`nodes`,
//! `calendar`, `pnl`), the history cache (`cache`), the editor's lookups
//! (`lookups`), the nodes over the real in-process `/api/v1` in both
//! analyzer and live mode (`app`), and the details the goldens leave open
//! (decision 35): defaults and stored failures (`defaults`), the Margin
//! node's choices (`margin`), the history nodes' bars and wording (`bars`)
//! and the web's server-log lines (`server_log`).

mod app;
mod cache;
mod calendar;
mod lookups;
mod nodes;
mod pnl;
mod support;
