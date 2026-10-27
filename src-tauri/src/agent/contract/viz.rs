//! The side channel a rendering tool's payload travels on (the web's
//! `services/agent/viz_sink.py`).
//!
//! A chart tool has two audiences: the model wants one line saying the
//! chart was drawn, the browser wants every candle. So the tool returns the
//! line and leaves the frame here, and the run drains the sink after each
//! successful tool call, sending what it holds as frames. Charting five
//! hundred candles costs the conversation one sentence.
//!
//! Every rendering tier travels this way: `viz` charts and cards, `ui`
//! markup from `render_ui`, `chart_command` for the `/trading` chart.
//!
//! The sink belongs to one run and is dropped with it; there is no
//! registry keyed by run id to leak. It holds at most
//! [`MAX_SINK_ENTRIES`]: with the run draining it after every tool it holds
//! one entry at a time, and the cap only matters if something forgets to
//! drain, when the oldest is dropped rather than memory growing.

use super::frames::Frame;
use parking_lot::Mutex;
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::sync::Arc;

/// Most renderings one run may leave undrained.
pub const MAX_SINK_ENTRIES: usize = 8;

/// One rendering waiting to go out.
#[derive(Debug, Clone)]
struct Entry {
    /// The tool that produced it, for logs only.
    tool: String,
    frame: Frame,
}

/// A run's sink, shared by every tool the run builds. A surface without one
/// ([`VizSink::none`]) renders nothing, and the tool says so to the model.
#[derive(Debug, Clone, Default)]
pub struct VizSink(Option<Arc<Mutex<VecDeque<Entry>>>>);

impl VizSink {
    /// A fresh sink for one run.
    pub fn new() -> VizSink {
        VizSink(Some(Arc::new(Mutex::new(VecDeque::new()))))
    }

    /// No sink: a rendering tool on this surface draws nothing.
    pub fn none() -> VizSink {
        VizSink(None)
    }

    pub fn is_present(&self) -> bool {
        self.0.is_some()
    }

    /// Queue a chart for the client (a [`Frame::Viz`]). False when there is
    /// no sink, which the tool reports with [`no_sink_message`] instead of
    /// claiming a chart is on screen.
    pub fn emit(
        &self,
        tool: &str,
        kind: &str,
        spec: Map<String, Value>,
        title: &str,
        source: &str,
    ) -> bool {
        self.emit_frame(
            tool,
            Frame::Viz {
                kind: kind.into(),
                spec,
                title: title.into(),
                source: source.into(),
            },
        )
    }

    /// Queue an already built frame (`ui`, `chart_command`, `viz`).
    pub fn emit_frame(&self, tool: &str, frame: Frame) -> bool {
        let Some(sink) = &self.0 else {
            tracing::warn!(
                "Agent tool {} produced a rendering but the run carries no sink",
                tool
            );
            return false;
        };
        let mut q = sink.lock();
        while q.len() >= MAX_SINK_ENTRIES {
            if let Some(dropped) = q.pop_front() {
                tracing::warn!(
                    "Agent viz sink is full; dropping the undrained rendering from {}",
                    dropped.tool
                );
            }
        }
        q.push_back(Entry {
            tool: tool.into(),
            frame,
        });
        true
    }

    /// Take every queued frame, oldest first, emptying the sink.
    pub fn drain(&self) -> Vec<Frame> {
        match &self.0 {
            Some(sink) => sink.lock().drain(..).map(|e| e.frame).collect(),
            None => Vec::new(),
        }
    }
}

/// What a rendering tool answers when its payload could not be delivered,
/// so the model does not tell the trader to look at a chart that is not on
/// screen.
pub fn no_sink_message(what: &str) -> String {
    format!(
        "The {what} could not be delivered to this surface, so nothing was drawn. Answer in prose and do not tell the operator that a {what} is on screen."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_drain_in_order_and_the_sink_empties() {
        let sink = VizSink::new();
        let shared = sink.clone();
        assert!(shared.emit("viz", "candles", Map::new(), "INFY", "history_service"));
        assert!(sink.emit_frame("chart", Frame::ChartCommand { commands: vec![] }));
        let frames = sink.drain();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].wire_type(), "viz");
        assert_eq!(frames[1].wire_type(), "chart_command");
        assert!(sink.drain().is_empty());
    }

    #[test]
    fn an_undrained_sink_keeps_only_the_newest() {
        let sink = VizSink::new();
        for i in 0..(MAX_SINK_ENTRIES + 3) {
            sink.emit_frame(
                "ui",
                Frame::Ui {
                    delta: i.to_string(),
                },
            );
        }
        let frames = sink.drain();
        assert_eq!(frames.len(), MAX_SINK_ENTRIES);
        assert_eq!(frames[0], Frame::Ui { delta: "3".into() });
    }

    #[test]
    fn no_sink_draws_nothing_and_says_so() {
        let sink = VizSink::none();
        assert!(!sink.is_present() && VizSink::new().is_present());
        assert!(!sink.emit_frame("ui", Frame::Ui { delta: "x".into() }));
        assert!(sink.drain().is_empty());
        assert_eq!(
            no_sink_message("chart"),
            "The chart could not be delivered to this surface, so nothing was drawn. Answer in prose and do not tell the operator that a chart is on screen."
        );
    }
}
