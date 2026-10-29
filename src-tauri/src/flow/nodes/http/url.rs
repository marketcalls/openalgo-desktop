//! Python 3.12's `urllib.parse.urlsplit`, as the web's HTTP Request node
//! reads a URL: the destination check takes the scheme, `hostname` and
//! `port` from it, and `_redact_url` rebuilds the logged URL from it. A
//! WHATWG parser reads some URLs differently (`http://a.com\@b.com/`), so
//! the node checks with this one, as the web does, and refuses a URL the
//! two disagree on.

/// The five parts of a split URL.
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    pub scheme: String,
    pub netloc: String,
    pub path: String,
    pub query: String,
    pub fragment: String,
}

const SCHEME_CHARS: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+-.";

/// `urlsplit(url)`, or the `ValueError` text it raises.
pub fn urlsplit(url: &str) -> Result<Split, String> {
    let url = url.trim_start_matches(|c: char| c <= ' ');
    let url: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut rest = url.as_str();
    let mut scheme = String::new();
    if let Some(i) = rest.find(':') {
        let first = rest.chars().next();
        let candidate = &rest[..i];
        if i > 0
            && first.is_some_and(|c| c.is_ascii_alphabetic())
            && candidate.chars().all(|c| SCHEME_CHARS.contains(c))
        {
            scheme = candidate.to_ascii_lowercase();
            rest = &rest[i + 1..];
        }
    }
    let mut netloc = String::new();
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        netloc = after[..end].to_string();
        rest = &after[end..];
        let open = netloc.contains('[');
        let close = netloc.contains(']');
        if open != close {
            return Err("Invalid IPv6 URL".into());
        }
        if open {
            check_bracketed_netloc(&netloc)?;
        }
    }
    let (rest, fragment) = match rest.split_once('#') {
        Some((a, b)) => (a, b.to_string()),
        None => (rest, String::new()),
    };
    let (path, query) = match rest.split_once('?') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (rest.to_string(), String::new()),
    };
    Ok(Split {
        scheme,
        netloc,
        path,
        query,
        fragment,
    })
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    let host_port = netloc.rsplit_once('@').map(|(_, h)| h).unwrap_or(netloc);
    let hostname = match host_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err("Invalid IPv6 URL".into());
            }
            let (host, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err("Invalid IPv6 URL".into());
            }
            host
        }
        None => host_port
            .split_once(':')
            .map(|(h, _)| h)
            .unwrap_or(host_port),
    };
    if let Some(future) = hostname.strip_prefix('v') {
        let ok = future.split_once('.').is_some_and(|(hex, tail)| {
            !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) && !tail.is_empty()
        });
        if !ok {
            return Err("IPvFuture address is invalid".into());
        }
        return Ok(());
    }
    match hostname.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(_)) => Err("An IPv4 address cannot be in brackets".into()),
        Ok(std::net::IpAddr::V6(_)) => Ok(()),
        Err(_) => Err(format!(
            "{} does not appear to be an IPv4 or IPv6 address",
            crate::flow::contract::pyfmt::quote(hostname)
        )),
    }
}

impl Split {
    /// The `user:password@` part, if any.
    pub fn userinfo(&self) -> Option<&str> {
        self.netloc.rsplit_once('@').map(|(u, _)| u)
    }

    /// `_hostinfo`: the host text and the port text.
    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = self
            .netloc
            .rsplit_once('@')
            .map(|(_, h)| h)
            .unwrap_or(&self.netloc);
        let (host, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (h, p) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (h, p.split_once(':').map(|(_, p)| p).unwrap_or(""))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (host, Some(port).filter(|p| !p.is_empty()))
    }

    /// `hostname`: lower case, `None` when empty (a zone id keeps its case).
    pub fn hostname(&self) -> Option<String> {
        let (host, _) = self.hostinfo();
        if host.is_empty() {
            return None;
        }
        Some(match host.split_once('%') {
            Some((h, zone)) => format!("{}%{}", h.to_lowercase(), zone),
            None => host.to_lowercase(),
        })
    }

    /// `port`, or the `ValueError` text it raises.
    pub fn port(&self) -> Result<Option<u16>, String> {
        let Some(text) = self.hostinfo().1 else {
            return Ok(None);
        };
        if !text.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!(
                "Port could not be cast to integer value as {}",
                crate::flow::contract::pyfmt::quote(text)
            ));
        }
        let digits = text.trim_start_matches('0');
        if digits.len() > 5 {
            return Err("Port out of range 0-65535".into());
        }
        match digits.parse::<u32>() {
            Ok(n) if n > 65535 => Err("Port out of range 0-65535".into()),
            Ok(n) => Ok(Some(n as u16)),
            Err(_) => Ok(Some(0)),
        }
    }
}

/// `urlunsplit`.
fn urlunsplit(scheme: &str, netloc: &str, path: &str) -> String {
    const USES_NETLOC: &[&str] = &[
        "",
        "ftp",
        "http",
        "gopher",
        "nntp",
        "telnet",
        "imap",
        "wais",
        "file",
        "mms",
        "https",
        "shttp",
        "snews",
        "prospero",
        "rtsp",
        "rtsps",
        "rtspu",
        "rsync",
        "svn",
        "svn+ssh",
        "sftp",
        "nfs",
        "git",
        "git+ssh",
        "ws",
        "wss",
        "itms-services",
    ];
    let mut url = path.to_string();
    if !netloc.is_empty() {
        if !url.is_empty() && !url.starts_with('/') {
            url = format!("/{}", url);
        }
        url = format!("//{}{}", netloc, url);
    } else if url.starts_with("//") {
        url = format!("//{}", url);
    } else if !scheme.is_empty()
        && USES_NETLOC.contains(&scheme)
        && (url.is_empty() || url.starts_with('/'))
    {
        url = format!("//{}", url);
    }
    if !scheme.is_empty() {
        url = format!("{}:{}", scheme, url);
    }
    url
}

/// The URL as the run's log shows it (`_redact_url`): no credentials, no
/// query (`?<redacted>` when there was one), no fragment.
pub fn redact(url: &str) -> String {
    let Ok(parts) = urlsplit(url) else {
        return "<unparseable url>".into();
    };
    let mut host = parts.hostname().unwrap_or_default();
    if let Ok(Some(port)) = parts.port() {
        if port != 0 {
            host = format!("{}:{}", host, port);
        }
    }
    let redacted = urlunsplit(&parts.scheme, &host, &parts.path);
    if parts.query.is_empty() {
        redacted
    } else {
        format!("{}?<redacted>", redacted)
    }
}

/// Python's `urllib.parse.unquote` of userinfo text (UTF-8, bad bytes
/// replaced).
pub fn unquote(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
