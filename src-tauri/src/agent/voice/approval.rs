//! Spoken approval of a paused run, decided here and never in the page
//! (the web's `voice.note_pause`, `forget_pause` and `judge_approval`).
//!
//! A window opens when a run started on the voice surface pauses for
//! approval, and only then (D-017: the web opened one for every pause, so a
//! spoken "yes" could approve an order staged by a typed question). It is
//! closed when the run is answered on screen, ends, or its pause expires,
//! and consumed by the one utterance that approves it. The page reports
//! what it heard and which run it heard it against; whether trading is
//! reachable by voice, whether that run is really waiting, whether its
//! window is open and whether the words are a plain yes are read from
//! stored settings and from this registry, which the page cannot write.
//!
//! Approving here does not place anything: the page then resumes the run
//! through `/agent/api/chat/confirm`, the same route the card uses, which
//! claims the paused run once and records the decision; the risk guard
//! still runs inside the order tool.
//!
//! The registry is a window, not a record: at most [`MAX_WINDOWS`] runs,
//! the oldest dropped, in memory only.

use super::confirm::{is_spoken_confirmation, window_is_open};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::time::Instant;

/// How many waiting runs are remembered (the web's
/// `_PAUSE_REGISTRY_LIMIT`).
pub const MAX_WINDOWS: usize = 64;

pub const SWITCHED_OFF: &str = "Placing orders by voice is switched off.";
pub const PASSED: &str = "The time to approve that by voice has passed.";
pub const NOT_A_CONFIRMATION: &str = "That was not a confirmation.";

/// The answer to one spoken approval attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub approved: bool,
    /// Why not, in words the trader can act on; empty when approved.
    pub reason: &'static str,
}

impl Verdict {
    fn refused(reason: &'static str) -> Verdict {
        Verdict {
            approved: false,
            reason,
        }
    }
}

/// What the judgement reads from the settings, fresh for each attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// The master trading switch and the voice one, both on.
    pub trading_effective: bool,
    /// `voice_confirm_window_seconds`.
    pub window_seconds: i64,
}

/// The open windows: run id and when its pause was noted, oldest first.
#[derive(Default)]
pub struct ApprovalWindows {
    open: Mutex<VecDeque<(String, Instant)>>,
}

impl ApprovalWindows {
    /// Open the window of a run that started on the voice surface and has
    /// just paused. A blank id is ignored: a pause that cannot be named
    /// cannot be approved by voice either.
    pub fn note_pause(&self, run_id: &str, at: Instant) {
        let key = run_id.trim();
        if key.is_empty() {
            return;
        }
        let mut open = self.open.lock();
        open.retain(|(id, _)| id != key);
        while open.len() >= MAX_WINDOWS {
            open.pop_front();
        }
        open.push_back((key.to_string(), at));
    }

    /// Close a run's window: it was answered on screen, ended, or its
    /// pause expired. Unknown and blank ids are ignored.
    pub fn forget(&self, run_id: &str) {
        let key = run_id.trim();
        self.open.lock().retain(|(id, _)| id != key);
    }

    /// Whether one utterance approves one run, in the web's order of
    /// checks: spoken trading on, the window open, the words a plain yes.
    /// The read and the claim happen under one lock, so two utterances
    /// arriving together approve once; anything that is not a yes leaves
    /// the window open (the trader is still deciding about an order they
    /// have just heard).
    pub fn judge(&self, run_id: &str, transcript: &str, rules: Rules, now: Instant) -> Verdict {
        if !rules.trading_effective {
            return Verdict::refused(SWITCHED_OFF);
        }
        let key = run_id.trim();
        let mut open = self.open.lock();
        let position = open.iter().position(|(id, _)| id == key);
        let opened_at = position.map(|i| open[i].1);
        let Some(index) = position.filter(|_| window_is_open(opened_at, now, rules.window_seconds))
        else {
            return Verdict::refused(PASSED);
        };
        if !is_spoken_confirmation(transcript) {
            return Verdict::refused(NOT_A_CONFIRMATION);
        }
        open.remove(index);
        drop(open);
        tracing::info!("Voice approval accepted for run {}", key);
        Verdict {
            approved: true,
            reason: "",
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.open.lock().len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    const ARMED: Rules = Rules {
        trading_effective: true,
        window_seconds: 30,
    };

    fn later(t: Instant, s: u64) -> Instant {
        t + Duration::from_secs(s)
    }

    /// The web's TestSpokenApprovalIsDecidedServerSide, case for case.
    #[test]
    fn a_waiting_run_is_approved_once_by_a_plain_yes() {
        let w = ApprovalWindows::default();
        let t = Instant::now();
        assert_eq!(w.judge("run-1", "yes", ARMED, t), Verdict::refused(PASSED));
        w.note_pause("run-1", t);
        assert!(w.judge("run-1", "yes", ARMED, later(t, 1)).approved);
        let again = w.judge("run-1", "yes", ARMED, later(t, 2));
        assert_eq!(again, Verdict::refused(PASSED), "approving consumes it");
    }

    #[test]
    fn a_question_is_not_an_approval_and_keeps_the_window() {
        let w = ApprovalWindows::default();
        let t = Instant::now();
        w.note_pause("run-1", t);
        let v = w.judge("run-1", "what is bank nifty doing", ARMED, t);
        assert_eq!(v, Verdict::refused(NOT_A_CONFIRMATION));
        assert_eq!(w.len(), 1);
        assert!(w.judge("run-1", "yes", ARMED, t).approved);
    }

    #[test]
    fn spoken_trading_off_refuses_before_anything_else() {
        let w = ApprovalWindows::default();
        let t = Instant::now();
        w.note_pause("run-1", t);
        let off = Rules {
            trading_effective: false,
            ..ARMED
        };
        assert_eq!(
            w.judge("run-1", "yes", off, t),
            Verdict::refused(SWITCHED_OFF)
        );
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn an_expired_window_refuses_a_valid_yes() {
        let w = ApprovalWindows::default();
        let t = Instant::now();
        w.note_pause("run-1", t);
        assert_eq!(
            w.judge("run-1", "yes", ARMED, later(t, 120)),
            Verdict::refused(PASSED)
        );
        assert!(
            w.judge("run-1", "yes", ARMED, later(t, 30)).approved,
            "30 s is inside"
        );
    }

    #[test]
    fn a_run_decided_elsewhere_and_unknown_ids_are_handled() {
        let w = ApprovalWindows::default();
        let t = Instant::now();
        w.note_pause("run-1", t);
        w.forget("run-never-paused");
        w.forget("");
        w.note_pause("  ", t);
        assert_eq!(w.len(), 1);
        w.forget("run-1");
        assert_eq!(w.judge("run-1", "yes", ARMED, t), Verdict::refused(PASSED));
    }

    /// The cap: the 65th pause drops the oldest, never grows the registry.
    #[test]
    fn at_most_sixty_four_windows_and_the_oldest_goes() {
        let w = ApprovalWindows::default();
        let t = Instant::now();
        for i in 0..=MAX_WINDOWS {
            w.note_pause(&format!("run-{}", i), t);
        }
        assert_eq!(w.len(), MAX_WINDOWS);
        assert_eq!(w.judge("run-0", "yes", ARMED, t), Verdict::refused(PASSED));
        assert!(w.judge("run-64", "yes", ARMED, t).approved);
        assert!(w.judge("run-1", "yes", ARMED, t).approved);
    }

    /// The web's "claimed by the pop, not by the read", as a race: many
    /// utterances at once approve the run exactly once.
    #[test]
    fn utterances_arriving_together_approve_once() {
        let w = Arc::new(ApprovalWindows::default());
        let t = Instant::now();
        w.note_pause("run-1", t);
        let barrier = Arc::new(std::sync::Barrier::new(16));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let (w, b) = (w.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    w.judge("run-1", "yes", ARMED, t).approved
                })
            })
            .collect();
        let approvals = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|a| *a)
            .count();
        assert_eq!(approvals, 1);
        assert!(w.is_empty());
    }
}
