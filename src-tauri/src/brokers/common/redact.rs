//! Error text that is safe to log.
//!
//! Several brokers carry credentials in a URL: Kite's ticker takes
//! `?api_key=..&access_token=..`, HDFC Securities and others put a key and a
//! token on the query, and some feeds accept `user:token@host`. `reqwest`
//! and `tungstenite` errors can repeat the URL they failed on in their
//! `Display`, so an error logged with `{}` can write a live token to the
//! log. Every connect, read or request error from a broker socket or call is
//! logged through [`url_safe_error`], which keeps the scheme, host and path
//! of any URL in the text and drops its userinfo, query and fragment.

use std::fmt::Display;

/// What replaces a dropped query string or userinfo.
pub const REDACTED: &str = "<redacted>";

/// `err` as text with every URL in it reduced to scheme, host and path.
pub fn url_safe_error(err: &dyn Display) -> String {
    url_safe(&err.to_string())
}

/// `text` with every `scheme://` URL reduced to scheme, host and path:
/// userinfo, query and fragment are replaced by [`REDACTED`].
pub fn url_safe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(sep) = rest.find("://") {
        // The scheme runs back from `://` over [A-Za-z0-9+.-].
        let start = rest[..sep]
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            .last()
            .map(|(i, _)| i)
            .unwrap_or(sep);
        out.push_str(&rest[..start]);
        let after = &rest[sep + 3..];
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(after.len());
        out.push_str(&rest[start..sep + 3]);
        out.push_str(&clean_after_scheme(&after[..end]));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// `user:pass@host/path?q#f` -> `<redacted>@host/path?<redacted>`.
fn clean_after_scheme(u: &str) -> String {
    let authority_end = u.find(['/', '?', '#']).unwrap_or(u.len());
    let (authority, tail) = u.split_at(authority_end);
    let mut s = match authority.rfind('@') {
        Some(at) => format!("{}@{}", REDACTED, &authority[at + 1..]),
        None => authority.to_string(),
    };
    match tail.find(['?', '#']) {
        Some(q) => {
            s.push_str(&tail[..q]);
            s.push_str(&tail[q..q + 1]);
            s.push_str(REDACTED);
        }
        None => s.push_str(tail),
    }
    s
}

/// What went wrong with a broker socket, without any of the error's text
/// (which can repeat the URL or a server's reply): what the feed logs.
pub fn ws_error_kind(err: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    use tokio_tungstenite::tungstenite::Error as E;
    match err {
        E::ConnectionClosed => "connection closed",
        E::AlreadyClosed => "already closed",
        E::Io(e) => match e.kind() {
            std::io::ErrorKind::ConnectionRefused => "connection refused",
            std::io::ErrorKind::ConnectionReset => "connection reset",
            std::io::ErrorKind::ConnectionAborted => "connection aborted",
            std::io::ErrorKind::TimedOut => "timed out",
            std::io::ErrorKind::UnexpectedEof => "unexpected end of stream",
            _ => "network error",
        },
        E::Tls(_) => "TLS error",
        E::Capacity(_) => "message too large",
        E::Protocol(_) => "protocol error",
        E::WriteBufferFull(_) => "write buffer full",
        E::Utf8 => "invalid text frame",
        E::AttackAttempt => "rejected by the client",
        E::Url(_) => "invalid address",
        E::Http(_) => "handshake refused",
        E::HttpFormat(_) => "invalid handshake",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_and_userinfo_are_dropped() {
        assert_eq!(
            url_safe("error sending request for url (https://api.kite.trade/x?api_key=K&access_token=SENTINEL)"),
            "error sending request for url (https://api.kite.trade/x?<redacted>)"
        );
        assert_eq!(
            url_safe("connect wss://ws.kite.trade?api_key=k&access_token=SENTINEL failed"),
            "connect wss://ws.kite.trade?<redacted> failed"
        );
        assert_eq!(
            url_safe("wss://user:SENTINEL@feed.example.com/ws#frag"),
            "wss://<redacted>@feed.example.com/ws#<redacted>"
        );
        assert_eq!(url_safe("no url here"), "no url here");
        assert_eq!(
            url_safe("a http://h/p and b ws://h2:9/q?t=1"),
            "a http://h/p and b ws://h2:9/q?<redacted>"
        );
        assert_eq!(url_safe("://?x"), "://?<redacted>");
    }
}
