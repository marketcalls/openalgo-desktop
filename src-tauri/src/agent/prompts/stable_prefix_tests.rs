//! The prompt is the same text from one message to the next (web
//! d941bb3b1): a provider's prompt cache matches a prefix, so anything that
//! changes per message comes after everything that does not.

use super::*;
use chrono::TimeZone;

fn at(minute: u32) -> DateTime<Tz> {
    chrono_tz::Asia::Kolkata
        .with_ymd_and_hms(2026, 11, 19, 15, minute, 0)
        .single()
        .expect("a valid IST time")
}

fn chat_prompt(minute: u32) -> String {
    build_system_prompt(&SystemPrompt {
        surface: Some(Surface::Chat),
        now: Some(at(minute)),
        extra_runtime_lines: vec!["Chart symbol: NIFTY, interval 5m".into()],
        ..Default::default()
    })
}

/// Web `test/test_agent_prompt_cache.py::test_a_new_minute_changes_only_the_very_end_of_the_prompt`:
/// the clock sat at character 2,638 of 29,349, so 9 percent of the prompt
/// was stable.
#[test]
fn a_new_minute_changes_only_the_very_end_of_the_prompt() {
    let (first, second) = (chat_prompt(30), chat_prompt(31));
    let first: Vec<char> = first.chars().collect();
    let second: Vec<char> = second.chars().collect();
    let differs_at = first
        .iter()
        .zip(&second)
        .position(|(a, b)| a != b)
        .expect("the clock changed");

    assert!(
        differs_at as f64 / first.len() as f64 > 0.98,
        "differs at {} of {}",
        differs_at,
        first.len()
    );
    assert_eq!(first[..differs_at], second[..differs_at]);
}

/// Web `test/test_agent_prompt_cache.py::test_the_clock_and_the_chart_lines_are_still_in_the_prompt_and_come_last`.
#[test]
fn the_clock_and_the_chart_lines_are_still_in_the_prompt_and_come_last() {
    let prompt = chat_prompt(30);

    assert!(prompt.contains("Current date and time: Thursday, 2026-11-19 15:30 IST"));
    assert!(prompt.contains("Chart symbol: NIFTY, interval 5m"));
    let last = |s: &str| prompt.rfind(s).unwrap_or_else(|| panic!("{} missing", s));
    let first = |s: &str| prompt.find(s).unwrap_or_else(|| panic!("{} missing", s));
    assert!(last("RIGHT NOW") > last("THIS SESSION"));
    assert!(
        first("RIGHT NOW") > first("HOW TO ANSWER"),
        "after every static section"
    );
}

/// Web `test/test_agent_prompt_cache.py::test_what_changes_rarely_stays_in_the_static_part`.
#[test]
fn what_changes_rarely_stays_in_the_static_part() {
    let prompt = |trading_enabled| {
        build_system_prompt(&SystemPrompt {
            surface: Some(Surface::Chat),
            trading_enabled,
            ..Default::default()
        })
    };
    let (on, off) = (prompt(true), prompt(false));

    assert!(on.contains("Trading is enabled") && off.contains("Trading is disabled"));
    assert!(
        !on.contains("Current date and time"),
        "no clock was given, so none is invented"
    );
}

/// Web `test/test_agent_prompt_cache.py::test_a_prompt_trimmed_to_its_budget_keeps_the_clock`:
/// the highest-order sections are dropped first, and the clock is the last
/// one.
#[test]
fn a_prompt_trimmed_to_its_budget_keeps_the_clock() {
    let trimmed = build_system_prompt(&SystemPrompt {
        surface: Some(Surface::Chat),
        now: Some(at(30)),
        max_chars: Some(6000),
        ..Default::default()
    });

    assert!(trimmed.contains("Current date and time"));
}

/// Web `test/test_agent_prompt_cache.py::test_a_surface_with_no_clock_and_no_lines_adds_no_empty_section`.
#[test]
fn a_surface_with_no_clock_and_no_lines_adds_no_empty_section() {
    assert_eq!(volatile_section(None, &[]), None);
    let blank = ["".to_string(), "  ".to_string()];
    assert_eq!(volatile_section(None, &blank), None);
}
