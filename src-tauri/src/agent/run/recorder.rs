//! One turn's frames folded into the stored answer (the web's
//! `_TurnRecorder`).
//!
//! The prose is the token deltas; each tool call is one timeline entry;
//! everything else beside the prose (notices, errors, a confirmation, up to
//! four charts, the OpenUI markup, the last usage) goes in `notices`, in the
//! order it happened, with the run id last so an edit can forget the run.

use crate::agent::contract::frames::Frame;
use serde_json::{json, Map, Value};

/// How many `viz` charts one stored turn keeps (a chart spec is a series,
/// not a sentence).
pub const MAX_STORED_VIZ: usize = 4;

#[derive(Debug, Default)]
pub struct TurnRecorder {
    text: String,
    tools: Vec<Map<String, Value>>,
    notices: Vec<Value>,
    viz: Vec<Value>,
    ui: String,
    usage: Option<Value>,
    pub run_id: String,
    pub session_id: String,
    pub paused: bool,
}

impl TurnRecorder {
    pub fn observe(&mut self, frame: &Frame) {
        match frame {
            Frame::Token { delta } => self.text.push_str(delta),
            Frame::Ui { delta } => self.ui.push_str(delta),
            Frame::Start {
                run_id, session_id, ..
            } => {
                self.run_id = run_id.clone();
                self.session_id = session_id.clone();
            }
            Frame::ToolStart { id, name, args } => {
                let mut e = Map::new();
                e.insert("id".into(), json!(id));
                e.insert("name".into(), json!(name));
                e.insert("args".into(), Value::Object(args.clone()));
                self.tools.push(e);
            }
            Frame::ToolEnd {
                id,
                name,
                ok,
                result,
                duration,
            } => {
                let open = self
                    .tools
                    .iter()
                    .rposition(|e| e.get("id") == Some(&json!(id)) && !e.contains_key("ok"));
                let index = match open {
                    Some(i) => i,
                    None => {
                        let mut e = Map::new();
                        e.insert("id".into(), json!(id));
                        e.insert("name".into(), json!(name));
                        self.tools.push(e);
                        self.tools.len() - 1
                    }
                };
                let entry = &mut self.tools[index];
                entry.insert("ok".into(), json!(ok));
                entry.insert("result".into(), result.clone());
                entry.insert("duration".into(), json!(duration));
            }
            Frame::Viz { .. } => {
                if self.viz.len() < MAX_STORED_VIZ {
                    self.viz.push(frame.to_value());
                }
            }
            Frame::Usage(_) => self.usage = Some(frame.to_value()),
            Frame::Notice { .. } | Frame::Error { .. } => self.notices.push(frame.to_value()),
            Frame::Confirm { .. } => {
                self.paused = true;
                self.notices.push(frame.to_value());
            }
            Frame::Reasoning { .. } | Frame::ChartCommand { .. } | Frame::Done { .. } => {}
        }
    }

    pub fn content(&self) -> &str {
        &self.text
    }

    pub fn tools(&self) -> Option<Value> {
        (!self.tools.is_empty())
            .then(|| Value::Array(self.tools.iter().cloned().map(Value::Object).collect()))
    }

    /// Everything beside the prose, in the order it happened.
    pub fn sidecar(&self) -> Option<Value> {
        let mut entries = self.notices.clone();
        entries.extend(self.viz.iter().cloned());
        if !self.ui.is_empty() {
            entries.push(json!({"type": "ui", "content": self.ui}));
        }
        if let Some(u) = &self.usage {
            entries.push(u.clone());
        }
        if !self.run_id.is_empty() {
            entries.push(json!({"type": "run", "run_id": self.run_id}));
        }
        (!entries.is_empty()).then_some(Value::Array(entries))
    }

    /// Whether the turn produced anything worth storing.
    pub fn has_content(&self) -> bool {
        !self.text.is_empty()
            || !self.tools.is_empty()
            || !self.notices.is_empty()
            || !self.viz.is_empty()
            || !self.ui.is_empty()
            || self.usage.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::contract::frames::{DoneReason, NoticeLevel, Usage, WireId};

    #[test]
    fn a_turn_is_folded_as_the_web_folds_it() {
        let mut r = TurnRecorder::default();
        let frames = vec![
            Frame::Start {
                run_id: "r1".into(),
                session_id: "s1".into(),
                conversation_id: WireId::Number(1),
                user_message_id: WireId::Number(2),
            },
            Frame::Token {
                delta: "Hel".into(),
            },
            Frame::ToolStart {
                id: "c1".into(),
                name: "get_quote".into(),
                args: Map::new(),
            },
            Frame::ToolEnd {
                id: "c1".into(),
                name: "get_quote".into(),
                ok: true,
                result: json!("{}"),
                duration: Some(0.5),
            },
            Frame::Notice {
                level: NoticeLevel::Info,
                message: "n".into(),
            },
            Frame::Ui { delta: "a".into() },
            Frame::Ui { delta: "b".into() },
            Frame::Usage(Usage {
                input_tokens: 1,
                ..Default::default()
            }),
            Frame::Usage(Usage {
                input_tokens: 2,
                ..Default::default()
            }),
            Frame::Token { delta: "lo".into() },
            Frame::Done {
                reason: DoneReason::Stop,
            },
        ];
        for _ in 0..6 {
            r.observe(&Frame::Viz {
                kind: "candles".into(),
                spec: Map::new(),
                title: String::new(),
                source: String::new(),
            });
        }
        for f in &frames {
            r.observe(f);
        }
        assert_eq!(r.content(), "Hello");
        assert_eq!(r.tools().unwrap()[0]["ok"], true);
        let side = r.sidecar().unwrap();
        let types: Vec<&str> = side
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            ["notice", "viz", "viz", "viz", "viz", "ui", "usage", "run"]
        );
        assert_eq!(side[5]["content"], "ab");
        assert_eq!(side[6]["input_tokens"], 2);
        assert_eq!(side[7]["run_id"], "r1");
    }
}
