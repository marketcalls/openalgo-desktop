//! web: services/strategy_module/state.py claims (test_strategy_module_engine.py,
//! test_strategy_module_qa_edges.py)

use openalgo_desktop_lib::strategy::state::{
    new_leg_state, ClaimId, EntryDecision, LegSpec, RunState, StateRegistry,
};

fn registry_with_open_leg(entry_status: &str) -> StateRegistry {
    let reg = StateRegistry::new();
    let mut l = new_leg_state(&LegSpec {
        leg_id: 1,
        position: "S".into(),
        symbol: "X".into(),
        exchange: "NFO".into(),
        quantity: 75,
        position_ref: Some("ref1".into()),
        ..Default::default()
    })
    .unwrap();
    l.status = "open".into();
    l.entry_status = entry_status.into();
    reg.install(RunState::new(7, 1, vec![l]));
    reg
}

#[test]
fn a_second_claim_on_one_leg_is_refused() {
    let reg = registry_with_open_leg("complete");
    assert!(reg.claim_leg_exit(7, 1, "exit_sl").is_some());
    assert!(reg.claim_leg_exit(7, 1, "exit_target").is_none());
}

#[test]
fn the_claim_marker_is_written_before_any_order_id() {
    let reg = registry_with_open_leg("complete");
    reg.claim_leg_exit(7, 1, "exit_sl").unwrap();
    let leg = reg.snapshot(7).unwrap().leg(1).unwrap().clone();
    assert_eq!(leg.exit_kind.as_deref(), Some("exit_sl"));
    assert!(leg.exit_order_id.is_none());
}

#[test]
fn an_accepted_but_unfilled_entry_is_never_claimed_for_exit() {
    let reg = registry_with_open_leg("open");
    assert!(reg.claim_leg_exit(7, 1, "exit_sl").is_none());
    let (claimed, unfilled) = reg.claim_legs_for_exit(7, &[1], "exit_close_all");
    assert!(claimed.is_empty());
    assert_eq!(unfilled.len(), 1);
}

#[test]
fn a_released_claim_makes_the_leg_exitable_again() {
    let reg = registry_with_open_leg("complete");
    let c = reg.claim_leg_exit(7, 1, "exit_sl").unwrap();
    assert!(!reg.release_leg_exit(7, 1, &ClaimId::Token("someone-else".into())));
    assert!(reg.release_leg_exit(7, 1, &ClaimId::Token(c.claim_token)));
    assert!(reg.claim_leg_exit(7, 1, "exit_target").is_some());
}

#[test]
fn a_claim_on_a_cleared_run_registers_nothing() {
    let reg = StateRegistry::new();
    assert!(reg.claim_leg_exit(99, 1, "exit_sl").is_none());
    assert!(
        reg.is_empty(),
        "probing an unknown run must not create state"
    );
}

#[test]
fn clearing_a_run_drops_its_state_and_lock() {
    let reg = registry_with_open_leg("complete");
    reg.clear(7);
    assert!(reg.snapshot(7).is_none());
    assert_eq!(reg.len(), 0);
}

#[test]
fn a_stopping_run_refuses_new_signal_entries() {
    let reg = registry_with_open_leg("complete");
    reg.mark_stopping(7);
    assert_eq!(
        reg.claim_signal_entry(7, 2, "B"),
        Some(EntryDecision::Note("run_stopping"))
    );
}

#[test]
fn a_repeated_entry_on_the_held_side_is_a_noop() {
    let reg = registry_with_open_leg("complete");
    assert_eq!(
        reg.claim_signal_entry(7, 1, "S"),
        Some(EntryDecision::Note("already_short"))
    );
}

#[test]
fn one_signal_entry_decision_per_leg_at_a_time() {
    let reg = registry_with_open_leg("complete");
    assert!(matches!(
        reg.claim_signal_entry(7, 3, "B"),
        Some(EntryDecision::Claimed(_))
    ));
    assert_eq!(
        reg.claim_signal_entry(7, 3, "B"),
        Some(EntryDecision::Note("flip_pending"))
    );
}

#[test]
fn the_checkpoint_snapshot_round_trips() {
    let reg = registry_with_open_leg("complete");
    let s = reg.snapshot(7).unwrap();
    let snap = s.snapshot_for_checkpoint();
    let legs: std::collections::BTreeMap<String, openalgo_desktop_lib::strategy::state::LegState> =
        serde_json::from_value(snap["leg_state"].clone()).unwrap();
    assert_eq!(legs, s.legs);
    assert_eq!(snap["leg_state"]["1"]["position"], "S");
}
