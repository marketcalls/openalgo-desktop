//! A smart order never sizes itself against a position it could not read
//! (web `utils/position_read.py`, #2116 / #2117).
//!
//! A failed position-book read used to read as an empty book on several
//! brokers, so a smart order placed its full quantity on top of a position
//! it could not see. A broker's `get_open_position` reads the book
//! strictly: its own success check, or, for a broker that answers an empty
//! book with an error envelope, an empty-book phrase in that broker's
//! message fields; anything else refuses the order with `unread`.

use crate::error::AppError;
use serde_json::Value;

/// Phrases brokers use in the message of an answer that means "no
/// positions" (web `_EMPTY_BOOK_MARKERS`).
pub const EMPTY_BOOK_MARKERS: &[&str] = &[
    "no data",
    "nodata",
    "no_data",
    "no-data",
    "no position",
    "no open position",
    "no record",
    "have any position",
    "have any open position",
    "data not found",
    "record not found",
    "positions not found",
];

/// A one-line "no data" message is short; a long error page that happens to
/// contain the words cannot pass (web `_EMPTY_BOOK_MAX_CHARS`).
const EMPTY_BOOK_MAX_CHARS: usize = 2000;

fn field<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, part| cur.get(part))
}

/// web `says_no_positions`: one of `fields` (dotted paths into the answer,
/// and only those) is a short string carrying an empty-book phrase.
pub fn says_no_positions(v: &Value, fields: &[&str]) -> bool {
    fields.iter().any(|path| {
        field(v, path)
            .and_then(Value::as_str)
            .filter(|s| s.chars().count() <= EMPTY_BOOK_MAX_CHARS)
            .is_some_and(|s| {
                let lower = s.to_lowercase();
                EMPTY_BOOK_MARKERS.iter().any(|m| lower.contains(m))
            })
    })
}

/// The refusal a trader reads (web `position_read_failed_message`), with
/// the broker's display name.
pub fn unread(display_name: &str) -> AppError {
    AppError::Broker(format!(
        "OpenAlgo could not read your open position from {}, so no order was sent. Check your positions and try again.",
        display_name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_the_named_short_fields_count() {
        let v = json!({"head": {"statusDescription": "No Data Found"}, "body": {"Message": "x"}});
        assert!(says_no_positions(&v, &["head.statusDescription"]));
        assert!(!says_no_positions(&v, &["body.Message"]));
        assert!(!says_no_positions(&v, &["missing.path"]));
        // A phrase in a field that is not named does not pass.
        let other = json!({"statusMessage": "Failure", "detail": "no data"});
        assert!(!says_no_positions(&other, &["statusMessage"]));
        // Nor does a long page that happens to carry it.
        let long = json!({"message": format!("{} no data", "x".repeat(2001))});
        assert!(!says_no_positions(&long, &["message"]));
        assert!(says_no_positions(
            &json!({"message": "Positions Not Found"}),
            &["message"]
        ));
        assert!(!says_no_positions(
            &json!({"message": "No session"}),
            &["message"]
        ));
        assert_eq!(
            unread("Samco").client_message(),
            "OpenAlgo could not read your open position from Samco, so no order was sent. Check your positions and try again."
        );
    }
}
