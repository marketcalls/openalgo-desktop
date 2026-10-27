//! Spoken approval of a staged order, decided without a model (the web's
//! `services/agent/safety/voice_confirm.py`).
//!
//! Nothing here reads a prompt, holds state or touches I/O: a transcript
//! goes in and a yes or no comes out, so no phrasing in a conversation, a
//! symbol name or a web page can change a verdict. The risk guard still
//! runs afterwards, inside the order tool.
//!
//! What protects an order is the read-back: the order is spoken back in
//! full and the trader answers a question they have just heard the whole
//! of. There is no secret word (the web tried one and removed it). So this
//! is honest about its limit: anyone within earshot who says yes while the
//! window is open approves the staged order.

use std::time::{Duration, Instant};

/// Words that, on their own, mean yes to an order just read back. Short
/// and closed on purpose: anything longer is a sentence, and a sentence
/// near a pending order is conversation, not an answer to it.
pub const CONFIRMATIONS: [&str; 18] = [
    "yes",
    "yeah",
    "yep",
    "yup",
    "ok",
    "okay",
    "confirm",
    "confirmed",
    "place",
    "send",
    "go",
    "ahead",
    "do",
    "it",
    "please",
    "sure",
    "right",
    "correct",
];

/// A confirmation is at most this many words ("yes go ahead please").
pub const MAX_CONFIRMATION_WORDS: usize = 4;

/// The longest agent name, in characters.
pub const MAX_NAME_LENGTH: usize = 40;

/// Refusal of an agent name. D-043: the web's sentence still said "wake
/// phrase", a word from the removed approval phrase; this field is the
/// agent's name.
pub const NAME_REFUSED: &str =
    "The agent's name must be letters and spaces only, at most 40 characters.";

/// Whether one utterance is a plain yes to an order already read back: one
/// to four words, every one of them an affirmation. Anything but ASCII
/// letters separates words, so commas and full stops carry no meaning.
pub fn is_spoken_confirmation(transcript: &str) -> bool {
    let words: Vec<String> = transcript
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    !words.is_empty()
        && words.len() <= MAX_CONFIRMATION_WORDS
        && words.iter().all(|w| CONFIRMATIONS.contains(&w.as_str()))
}

/// Whether a spoken approval window is still open: never without a pause
/// time or a positive window, never when the clock reads earlier than the
/// pause, else while the time since the pause is at most the window.
pub fn window_is_open(opened_at: Option<Instant>, now: Instant, seconds: i64) -> bool {
    let (Some(opened), Ok(window)) = (opened_at, u64::try_from(seconds)) else {
        return false;
    };
    if window == 0 {
        return false;
    }
    now.checked_duration_since(opened)
        .is_some_and(|elapsed| elapsed <= Duration::from_secs(window))
}

/// The agent's name as stored: whitespace collapsed, case kept; letters
/// and spaces only, starting with a letter, at most 40 characters. It has
/// no security role; it is checked so it survives transcription and reads
/// well on screen.
pub fn normalise_agent_name(raw: &str) -> Result<String, &'static str> {
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let starts_with_letter = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    let letters_and_spaces = name.chars().all(|c| c.is_ascii_alphabetic() || c == ' ');
    if starts_with_letter && letters_and_spaces && name.chars().count() <= MAX_NAME_LENGTH {
        Ok(name)
    } else {
        Err(NAME_REFUSED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The web's examples, plus the edges of the four-word rule.
    #[test]
    fn only_a_plain_yes_approves() {
        for yes in [
            "yes",
            "Yes.",
            "go ahead",
            "yes place it",
            "OK, confirm",
            "yes go ahead please",
        ] {
            assert!(is_spoken_confirmation(yes), "{}", yes);
        }
        for no in [
            "",
            "   ",
            "no",
            "yes but wait",
            "what is nifty",
            "yes yes yes yes yes",
            "yes, buy 50 more",
            "si",
        ] {
            assert!(!is_spoken_confirmation(no), "{}", no);
        }
    }

    #[test]
    fn the_word_list_is_the_webs_eighteen() {
        assert_eq!(CONFIRMATIONS.len(), 18);
        assert_eq!(MAX_CONFIRMATION_WORDS, 4);
    }

    #[test]
    fn a_window_is_open_only_forwards_and_within_its_seconds() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(window_is_open(Some(t0), at(0), 30));
        assert!(window_is_open(Some(t0), at(30), 30));
        assert!(!window_is_open(Some(t0), at(31), 30));
        assert!(!window_is_open(None, at(1), 30));
        assert!(!window_is_open(Some(t0), at(1), 0));
        assert!(!window_is_open(Some(t0), at(1), -5));
        assert!(!window_is_open(Some(at(10)), at(5), 30), "clock went back");
    }

    /// D-043: the refusal names the agent's name, not a wake phrase.
    #[test]
    fn a_name_is_letters_and_spaces_and_says_so_in_those_words() {
        assert_eq!(normalise_agent_name("  Ava   Rose ").unwrap(), "Ava Rose");
        assert_eq!(normalise_agent_name(&"a".repeat(40)).unwrap().len(), 40);
        for bad in ["", "   ", "R2D2", "Ava!", " 7even", &"a".repeat(41)] {
            let e = normalise_agent_name(bad).unwrap_err();
            assert_eq!(e, NAME_REFUSED, "{}", bad);
            assert!(!e.contains("wake") && !e.contains("phrase"));
        }
    }
}
