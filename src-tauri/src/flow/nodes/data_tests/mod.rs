//! The data family's tests (the web executor's goldens replay in
//! `flow::goldens::goldens_data`): the web's data-node tests ported (`nodes`,
//! `calendar`, `pnl`), the history cache (`cache`), the editor's lookups
//! (`lookups`) and the nodes over the real in-process `/api/v1` in both
//! analyzer and live mode (`app`).

mod app;
mod cache;
mod calendar;
mod lookups;
mod nodes;
mod pnl;
mod support;
