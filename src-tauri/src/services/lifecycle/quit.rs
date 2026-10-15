//! Quitting with Live work running (ARCH-03): the trader is told what
//! stops watching, with the inventory an update's restart reads
//! (`services::update`), and confirms first. Nothing is flattened: the
//! positions and broker orders stay where they are.

use crate::services::update::{activity, broker_name, Warning};
use crate::state::AppState;
use std::sync::atomic::{AtomicBool, Ordering};

/// What quitting now would leave unwatched, or `None` when nothing runs.
pub async fn quit_warning(ctx: &AppState) -> Option<Warning> {
    let broker = broker_name(ctx);
    activity(ctx).await.quit_warning(broker.as_deref())
}

/// What to do with a request to close the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseRequest {
    /// Close: the trader confirmed, or nothing needed confirming.
    Close,
    /// Keep the window and ask the trader (start the check).
    Ask,
    /// Keep the window: the trader is being asked already.
    Asking,
}

/// One quit check at a time, and the trader's answer.
#[derive(Debug, Default)]
pub struct QuitGate {
    asking: AtomicBool,
    confirmed: AtomicBool,
}

impl QuitGate {
    /// Decide on a request to close the main window.
    pub fn on_close(&self) -> CloseRequest {
        if self.confirmed.load(Ordering::SeqCst) {
            CloseRequest::Close
        } else if self.asking.swap(true, Ordering::SeqCst) {
            CloseRequest::Asking
        } else {
            CloseRequest::Ask
        }
    }

    /// The trader answered (or nothing needed asking): `quit` lets the next
    /// close go through; otherwise the next close asks again.
    pub fn answered(&self, quit: bool) {
        self.confirmed.store(quit, Ordering::SeqCst);
        self.asking.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_check_at_a_time_and_a_kept_window_asks_again() {
        let gate = QuitGate::default();
        assert_eq!(gate.on_close(), CloseRequest::Ask);
        // A second close while the dialog is open does not open another.
        assert_eq!(gate.on_close(), CloseRequest::Asking);
        // Kept open: the next close asks again.
        gate.answered(false);
        assert_eq!(gate.on_close(), CloseRequest::Ask);
        // Confirmed: the close goes through, every time.
        gate.answered(true);
        assert_eq!(gate.on_close(), CloseRequest::Close);
        assert_eq!(gate.on_close(), CloseRequest::Close);
    }
}
