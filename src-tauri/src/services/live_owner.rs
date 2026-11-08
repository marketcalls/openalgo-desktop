//! Which broker session a Live obligation belongs to (LOG-07, defence in
//! depth; docs/codex-evaluation 90-plus-roadmap, "Broker switching").
//!
//! Every Live obligation records the broker, and the account at that broker,
//! it was opened on: a strategy run (`sm_strategy_run.broker` and
//! `broker_account`), an OpenScript run and every order it placed
//! ([`Destination`], `openscript_incarnations` and `openscript_orders`), a
//! scalping stop
//! (`scalping_sl_state.broker` and `broker_account`) and an order a Live run
//! queued in the Action Center (`pending_orders.broker` and
//! `broker_account`). Before an order for it is sent, the dispatch layer
//! compares that with the session connected now, and refuses without
//! sending when they differ. Nothing was sent, so under LOG-08 it is a
//! definite refusal: the claim is released and the obligation stays managed
//! for when its own session is back.
//!
//! The broker switch guard (`services::broker_switch`) already refuses a
//! switch while obligations remain; this second layer holds when a session
//! changes some other way, and acknowledging a warning never changes it.
//! One key set per broker (several accounts of one broker are out of scope),
//! but the account is still compared, so a session that comes back on
//! another account is caught too.
//!
//! [`Destination`] is the side plus, for Live, that session: the one
//! identity a ledger row is bound to (NEW-TR-01). Its membership test is
//! strict where [`LiveOwner::refusal`] is lenient: a row whose broker was
//! never recorded belongs to no run, so a run started on another broker
//! never counts, cancels or closes it.

use crate::state::AppState;
use crate::strategy::dispatch::{OrderGateway, RunMode};

/// The broker session a Live obligation was opened on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LiveOwner {
    /// The broker id (`zerodha`); blank when it was not recorded.
    pub broker: String,
    /// The account at the broker (its client id); blank when unknown.
    pub account: String,
    /// For a live session told by its saved keys: what its login could have
    /// named instead, as digests (`session_account::Told`). Empty otherwise,
    /// and on a recorded owner.
    pub placeholders: Vec<String>,
}

impl LiveOwner {
    pub fn new(broker: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            broker: broker.into().trim().to_string(),
            account: account.into().trim().to_string(),
            placeholders: Vec::new(),
        }
    }

    /// The session connected now, if any, with the account it trades
    /// through (`services::session_account`).
    pub fn of_session(ctx: &AppState) -> Option<Self> {
        ctx.get_broker_session().map(|s| {
            let told = crate::services::session_account::told(ctx, &s);
            Self {
                placeholders: told.placeholders,
                ..Self::new(s.broker_id, told.account)
            }
        })
    }

    /// The session `gateway` sends `mode` orders through now: what a new
    /// obligation records (Sandbox: `sandbox`, no account).
    pub fn of_gateway(gateway: &dyn OrderGateway, mode: RunMode) -> Self {
        Self {
            placeholders: gateway.broker_placeholders(mode),
            ..Self::new(gateway.broker_name(mode), gateway.broker_account(mode))
        }
    }

    /// The live session `gateway` sends through now, if there is one.
    pub fn active(gateway: &dyn OrderGateway) -> Option<Self> {
        Some(Self::of_gateway(gateway, RunMode::Live)).filter(|o| !o.broker.is_empty())
    }

    /// The broker as a trader knows it, with the account when known:
    /// "Zerodha (AB1234)".
    pub fn describe(&self) -> String {
        let name = crate::brokers::setup_guide::display_name(&self.broker);
        if self.account.is_empty() {
            name
        } else if let Some(keys) = self
            .account
            .strip_prefix(crate::services::session_account::KEYS_ACCOUNT)
        {
            // The broker names no account: the saved keys stand for it.
            format!("{} (saved keys {})", name, &keys[..keys.len().min(6)])
        } else {
            format!("{} ({})", name, self.account)
        }
    }

    /// Whether the broker was recorded. A blank one, or the mode name a
    /// signal run writes when there was no session, says nothing.
    pub(crate) fn knows_broker(&self) -> bool {
        !self.broker.is_empty()
            && !self.broker.eq_ignore_ascii_case("live")
            && !self.broker.eq_ignore_ascii_case("sandbox")
    }

    /// Why an order for an obligation opened on `self` must not go through
    /// `active`, the session connected now, in a trader's words; `None`
    /// when it may. Only what both sides know is compared: an obligation
    /// recorded before owners were (no broker) or an account a broker does
    /// not report is not a mismatch. No session at all is refused by the
    /// caller's own session check.
    pub fn refusal(&self, active: Option<&LiveOwner>) -> Option<String> {
        let active = active?;
        let other_broker = self.knows_broker() && !self.broker.eq_ignore_ascii_case(&active.broker);
        // A session told by its saved keys: an obligation recorded before
        // accounts were derived so carries what the login named then, the
        // broker's name or a saved key value. Only such a placeholder is
        // this session's; a real account id recorded then is another
        // account's, whose exit must not go through this one.
        let keys = crate::services::session_account::KEYS_ACCOUNT;
        let recorded_placeholder = active.account.starts_with(keys)
            && !self.account.starts_with(keys)
            && crate::services::session_account::is_placeholder(
                &active.placeholders,
                &self.account,
            );
        if !other_broker && (self.same_account(active) || recorded_placeholder) {
            return None;
        }
        Some(self.not_sent_through(active))
    }

    /// The account the same wherever both sides know it.
    fn same_account(&self, other: &LiveOwner) -> bool {
        self.account.is_empty()
            || other.account.is_empty()
            || self.account.eq_ignore_ascii_case(&other.account)
    }

    /// Whether an obligation recorded on `self` was opened on the session
    /// `other` names: the broker and the account known on both sides and
    /// the same. Strict where [`Self::refusal`] is lenient: an owner never
    /// recorded matches nothing, and neither does a session whose account
    /// is not known (a failed account lookup must not own, cancel or exit
    /// another account's orders at the same broker).
    pub fn same_session(&self, other: &LiveOwner) -> bool {
        self.is_complete()
            && other.is_complete()
            && self.broker.eq_ignore_ascii_case(&other.broker)
            && self.account.eq_ignore_ascii_case(&other.account)
    }

    /// Both the broker and the account are known.
    fn is_complete(&self) -> bool {
        self.knows_broker() && !self.account.is_empty()
    }

    /// What a trader reads when an order of `self` would have gone through
    /// `active`, another session.
    fn not_sent_through(&self, active: &LiveOwner) -> String {
        let mine = self.describe();
        format!(
            "This was opened on {}, but OpenAlgo is signed in to {} now, so the order was not sent. Sign in to {} to manage it.",
            mine,
            active.describe(),
            mine
        )
    }
}

/// The broker a Sandbox destination records.
pub const SANDBOX_BROKER: &str = "sandbox";

/// Where an obligation's orders go (NEW-TR-01): the side, and for Live the
/// broker session it was opened on. The OpenScript runner records it on
/// every run (`openscript_incarnations`) and every order row; a run's
/// ledger is the rows its destination [`owns`](Destination::owns), and its
/// orders, cancels, status reads and book reads go only through that
/// session ([`Destination::refusal`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub mode: RunMode,
    /// Live: the broker session. Sandbox: [`SANDBOX_BROKER`], no account.
    pub owner: LiveOwner,
}

impl Destination {
    pub fn sandbox() -> Self {
        Self {
            mode: RunMode::Sandbox,
            owner: LiveOwner::new(SANDBOX_BROKER, ""),
        }
    }

    pub fn live(owner: LiveOwner) -> Self {
        Self {
            mode: RunMode::Live,
            owner,
        }
    }

    /// A destination as a row records it: its side's word, broker and
    /// account. `None` for a word that names neither side.
    pub fn recorded(mode: &str, broker: &str, account: &str) -> Option<Self> {
        Some(match RunMode::parse(mode)? {
            RunMode::Sandbox => Self::sandbox(),
            RunMode::Live => Self::live(LiveOwner::new(broker, account)),
        })
    }

    /// Whether an order recorded at `row` belongs here: the same side and,
    /// for Live, the same session ([`LiveOwner::same_session`]). A Live row
    /// whose broker was never recorded belongs nowhere.
    pub fn owns(&self, row: &Destination) -> bool {
        self.mode == row.mode
            && (self.mode == RunMode::Sandbox || self.owner.same_session(&row.owner))
    }

    /// Whether it says where its orders went: Sandbox always, Live when its
    /// broker and account were recorded.
    pub fn is_known(&self) -> bool {
        self.mode == RunMode::Sandbox || self.owner.is_complete()
    }

    /// Why an order, cancel or read for this destination must not go
    /// through `active`, the live session now, in a trader's words; `None`
    /// when it may (Sandbox, or the same session). No session at all is
    /// refused by the caller's own session check.
    pub fn refusal(&self, active: Option<&LiveOwner>) -> Option<String> {
        if self.mode == RunMode::Sandbox {
            return None;
        }
        let active = active?;
        (!self.owner.same_session(active)).then(|| self.owner.not_sent_through(active))
    }

    /// Where it is, as a trader reads it.
    pub fn describe(&self) -> String {
        match self.mode {
            RunMode::Sandbox => "sandbox mode".into(),
            RunMode::Live if self.owner.is_complete() => self.owner.describe(),
            RunMode::Live => "your broker, on an account this version did not record".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_another_broker_or_account_is_refused() {
        let zerodha = LiveOwner::new("zerodha", "AB1234");
        assert_eq!(zerodha.refusal(Some(&zerodha)), None);
        assert_eq!(zerodha.refusal(None), None, "no session: the caller says");
        let why = zerodha
            .refusal(Some(&LiveOwner::new("angel", "XY9876")))
            .unwrap();
        assert_eq!(
            why,
            "This was opened on Zerodha (AB1234), but OpenAlgo is signed in to Angel One (XY9876) now, so the order was not sent. Sign in to Zerodha (AB1234) to manage it."
        );
        assert!(zerodha
            .refusal(Some(&LiveOwner::new("zerodha", "ZZ0001")))
            .is_some());
        // What was not recorded is not compared.
        let legacy = LiveOwner::new("zerodha", "");
        assert_eq!(legacy.refusal(Some(&zerodha)), None);
        assert!(legacy.refusal(Some(&LiveOwner::new("angel", ""))).is_some());
        for unknown in ["", "live", "sandbox"] {
            let o = LiveOwner::new(unknown, "");
            assert_eq!(o.refusal(Some(&zerodha)), None, "{:?}", unknown);
        }
        assert_eq!(
            zerodha.refusal(Some(&LiveOwner::new("ZERODHA", "ab1234"))),
            None
        );
    }

    /// NEW-TR-01: a ledger row belongs to a run only on the same side and,
    /// for Live, the same broker session. Unlike `refusal`, a row whose
    /// broker was never recorded belongs to no run.
    #[test]
    fn a_destination_owns_only_its_own_sessions_rows() {
        let a = Destination::live(LiveOwner::new("zerodha", "AB1234"));
        let row = |m: &str, b: &str, acc: &str| Destination::recorded(m, b, acc).unwrap();
        assert!(a.owns(&row("live", "zerodha", "AB1234")));
        assert!(a.owns(&row("live", "ZERODHA", "ab1234")));
        assert!(
            !a.owns(&row("live", "zerodha", "")),
            "account not recorded: nobody's"
        );
        assert!(!a.owns(&row("live", "angel", "AB1234")));
        assert!(!a.owns(&row("live", "zerodha", "ZZ0001")));
        assert!(!a.owns(&row("live", "", "")), "never recorded: nobody's");
        assert!(!a.owns(&row("sandbox", "", "")));
        let sandbox = Destination::sandbox();
        assert!(sandbox.owns(&row("sandbox", "sandbox", "")));
        assert!(sandbox.owns(&row("sandbox", "", "")));
        assert!(!sandbox.owns(&row("live", "zerodha", "AB1234")));
        assert!(Destination::recorded("paper", "zerodha", "").is_none());
        assert!(!row("live", "", "").is_known());
        assert!(!row("live", "zerodha", "").is_known());

        let angel = LiveOwner::new("angel", "XY9876");
        assert_eq!(a.refusal(Some(&LiveOwner::new("zerodha", "AB1234"))), None);
        assert_eq!(a.refusal(None), None, "no session: the caller says");
        assert!(
            a.refusal(Some(&LiveOwner::new("zerodha", ""))).is_some(),
            "a session whose account is not known owns nothing"
        );
        assert_eq!(
            a.refusal(Some(&angel)).unwrap(),
            "This was opened on Zerodha (AB1234), but OpenAlgo is signed in to Angel One (XY9876) now, so the order was not sent. Sign in to Zerodha (AB1234) to manage it."
        );
        assert!(
            row("live", "", "").refusal(Some(&angel)).is_some(),
            "strict"
        );
        assert_eq!(sandbox.refusal(Some(&angel)), None);
    }

    /// Lead's third review, P2-2, and its final review: an obligation
    /// recorded before accounts were derived from the saved keys carries
    /// the login's placeholder ("samco"). The same broker's session told by
    /// its keys still manages it; a real account id recorded then (AB1234)
    /// is another account's, and so are other keys or another broker.
    #[test]
    fn only_a_recorded_placeholder_matches_its_brokers_keys_identity() {
        use crate::services::session_account::placeholder_digest;
        let session = |broker: &str, keys: &str, said: &[&str]| LiveOwner {
            placeholders: said.iter().map(|v| placeholder_digest(v)).collect(),
            ..LiveOwner::new(broker, keys)
        };
        let samco = session("samco", "keys:new-set", &["samco", "samco-api-key"]);
        assert_eq!(LiveOwner::new("samco", "samco").refusal(Some(&samco)), None);
        assert_eq!(
            LiveOwner::new("samco", "samco-api-key").refusal(Some(&samco)),
            None
        );
        let zerodha = session("zerodha", "keys:new-set", &["zerodha", "kitekey"]);
        assert!(
            LiveOwner::new("zerodha", "AB1234")
                .refusal(Some(&zerodha))
                .is_some(),
            "a real account recorded then is another account's"
        );
        assert!(LiveOwner::new("samco", "keys:old-set")
            .refusal(Some(&samco))
            .is_some());
        assert!(LiveOwner::new("samco", "samco")
            .refusal(Some(&session("angel", "keys:new-set", &["samco"])))
            .is_some());
    }
}
