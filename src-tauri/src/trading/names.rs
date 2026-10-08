//! The name rules the /trading routes share, written once (web
//! `blueprints/custom_indicators.py`, `blueprints/openscript.py`,
//! `services/openscript_run_config.py`, `services/openscript_deployment.py`).
//!
//! Every name a route accepts off the wire is held to one of these before it
//! touches the disk or the database. None of them can hold a path separator or
//! a dot segment, which is what keeps every file route inside its folder; the
//! file stores check the entry itself as a second layer.

/// `^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}<suffix>$`: the shape of a stored file
/// name (an indicator module or an OpenScript source).
fn file_name(name: &str, suffix: &str) -> bool {
    let Some(stem) = name.strip_suffix(suffix) else {
        return false;
    };
    let bytes = stem.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// A custom indicator module: `name.js`.
pub fn is_indicator_name(name: &str) -> bool {
    file_name(name, ".js")
}

/// An OpenScript source: `name.oscript`.
pub fn is_script_name(name: &str) -> bool {
    file_name(name, ".oscript")
}

/// An instrument, an exchange or an interval a run can be started on
/// (web `_RUN_FIELD`: `^[A-Za-z0-9][A-Za-z0-9_.:-]{0,63}$`).
pub fn is_run_field(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'-'))
}

/// An exchange code for `/openscript/instrument` (`^[A-Z][A-Z0-9_]{0,19}$`).
pub fn is_exchange_code(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 20
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
}

/// The prefix every deployment id carries.
pub const DEPLOYMENT_PREFIX: &str = "openscript";

/// Whether this could be a deployment id (shape only; whether one by that
/// name exists is the settings store's answer).
pub fn is_deployment_id(value: &str) -> bool {
    let b = value.as_bytes();
    value.starts_with("openscript_")
        && b.len() <= 120
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// A script or a deployment of one: what the runner routes accept.
pub fn names_something(value: &str) -> bool {
    is_script_name(value) || is_deployment_id(value)
}

/// The name of a parameter a script can declare (`^[A-Za-z_][A-Za-z0-9_.]{0,63}$`).
pub fn is_input_key(value: &str) -> bool {
    let b = value.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_alphabetic() || b[0] == b'_')
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.'))
}

/// Python's `repr()` of a name, for the refusal sentence the web writes
/// (`Invalid script name 'x'`).
pub fn py_repr(text: &str) -> String {
    crate::services::schema::py_repr_str(text)
}

/// The one refusal every OpenScript route answers a bad name with.
pub fn script_name_refusal(given: &str) -> String {
    format!(
        "Invalid script name {}. A name is letters, digits, dot, dash or underscore, and ends in .oscript",
        py_repr(given)
    )
}

/// A name that is not a script name, in the settings store's words.
pub fn not_a_script_name(given: &str) -> String {
    format!(
        "{} is not a script name. A name is letters, digits, dot, dash or underscore, and ends in .oscript",
        py_repr(given)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_plain_and_bounded() {
        assert!(is_indicator_name("ema-ribbon.js"));
        assert!(is_indicator_name("a.b_c-d.js"));
        assert!(!is_indicator_name(".hidden.js"));
        assert!(!is_indicator_name("../x.js"));
        assert!(!is_indicator_name("a/b.js"));
        assert!(!is_indicator_name("a\\b.js"));
        assert!(!is_indicator_name("x.mjs"));
        assert!(!is_indicator_name(".js"));
        assert!(is_indicator_name(&format!("{}.js", "a".repeat(64))));
        assert!(!is_indicator_name(&format!("{}.js", "a".repeat(65))));
        assert!(is_script_name("trend.oscript"));
        assert!(!is_script_name("trend.oscript.bak"));
        assert!(!is_script_name("trend.oscript.program.json"));
        assert!(!is_script_name("runner"));
    }

    #[test]
    fn deployment_ids_are_told_from_scripts() {
        assert!(is_deployment_id("openscript_trend_SBIN_NSE_5m_ab12cd"));
        assert!(!is_deployment_id("openscript"));
        assert!(!is_deployment_id("openscript_a/b"));
        assert!(!is_deployment_id("trend.oscript"));
        assert!(names_something("trend.oscript"));
        assert!(names_something("openscript_trend"));
        assert!(!names_something("status"));
    }

    #[test]
    fn run_fields_and_exchanges() {
        assert!(is_run_field("NIFTY24DEC24000CE"));
        assert!(is_run_field("BTC-USD:PERP"));
        assert!(!is_run_field(""));
        assert!(!is_run_field("SBIN NSE"));
        assert!(is_exchange_code("NSE_INDEX"));
        assert!(!is_exchange_code("nse"));
        assert!(!is_exchange_code("1NSE"));
        assert!(is_input_key("fast.length"));
        assert!(!is_input_key("1fast"));
    }

    #[test]
    fn refusal_reads_like_the_web() {
        assert_eq!(
            script_name_refusal("../x"),
            "Invalid script name '../x'. A name is letters, digits, dot, dash or underscore, and ends in .oscript"
        );
    }
}
