//! The claims of a broker's JWT, read and never verified: a token names its
//! own account or user, and an adapter reads that name from it where the
//! broker sends nothing else (the token itself is checked by the broker on
//! every call).

use base64::Engine;
use serde_json::{Map, Value};

/// The payload of `token` (`header.payload.signature`, base64url with or
/// without padding) as a JSON object; `None` when it is not one.
pub(crate) fn claims(token: &str) -> Option<Map<String, Value>> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    match serde_json::from_slice::<Value>(&bytes).ok()? {
        Value::Object(claims) => Some(claims),
        _ => None,
    }
}

/// A claim's text: a string as it is, a number as its digits; `None` for
/// anything else or an empty value.
pub(crate) fn text_claim(claims: &Map<String, Value>, key: &str) -> Option<String> {
    match claims.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(s: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
    }

    #[test]
    fn reads_a_claim_of_a_token_and_nothing_of_a_non_token() {
        let t = format!(
            "{}.{}.sig",
            part("{\"alg\":\"HS256\"}"),
            part("{\"userID\":\"1048131_APP\",\"n\":7}")
        );
        let c = claims(&t).unwrap();
        assert_eq!(text_claim(&c, "userID").as_deref(), Some("1048131_APP"));
        assert_eq!(text_claim(&c, "n").as_deref(), Some("7"));
        assert_eq!(text_claim(&c, "missing"), None);
        // A padded payload reads the same ("{}" is "e30" unpadded).
        assert!(claims("h.e30=.sig").is_some());
        // Not a token, a payload that is not base64url JSON, or not an object.
        for t in ["not-a-jwt", "a.b.c", "", "x.W10.sig"] {
            assert!(claims(t).is_none(), "{t}");
        }
    }
}
