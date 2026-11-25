//! The fetch boundary (the web's `test_agent_webfetch.py`, "The fetch
//! boundary" and the company pages of "Preferred sources"), and the
//! desktop's stricter reading of an address.

use crate::agent::websearch::fetch_url::{
    check, host_matches, redirect_refusal, url_key, Check, Source, NOT_GIVEN,
};
use std::collections::HashMap;

const OPERATOR: &str = "get the fundamentals for reliance from the screener portal";
const SCREENER: &str = "https://www.screener.in/company/RELIANCE/consolidated/";
const PREFERRED: &[&str] = &["screener.in", "scanx.trade"];

fn seen(urls: &[&str]) -> HashMap<String, String> {
    urls.iter()
        .filter_map(|u| Some((url_key(u)?, u.to_string())))
        .collect()
}

fn checked(url: &str, operator: &str, returned: &[&str]) -> Check {
    check(url, operator, &seen(returned), &[])
}

fn preferred(url: &str, operator: &str) -> Check {
    let sources: Vec<String> = PREFERRED.iter().map(|s| s.to_string()).collect();
    check(url, operator, &HashMap::new(), &sources)
}

fn allowed(c: &Check) -> Option<(&str, Source)> {
    match c {
        Check::Allowed { url, source } => Some((url.as_str(), *source)),
        Check::Refused(_) => None,
    }
}

/// Web `test_a_url_nobody_wrote_and_no_search_returned_is_refused`.
#[test]
fn a_url_nobody_wrote_and_no_search_returned_is_refused() {
    let c = checked(
        "https://attacker.example/collect?balance=1000000",
        OPERATOR,
        &[],
    );
    assert_eq!(c, Check::Refused(NOT_GIVEN.into()));
    assert!(NOT_GIVEN.contains("web_search"));
}

/// Web `test_a_url_from_this_runs_search_is_accepted`.
#[test]
fn a_url_from_this_runs_search_is_accepted() {
    let c = checked(SCREENER, OPERATOR, &[SCREENER]);
    assert_eq!(allowed(&c), Some((SCREENER, Source::Search)));
}

/// Web `test_search_results_match_across_scheme_www_and_trailing_slash`:
/// what is sent is what the search returned, not the model's spelling.
#[test]
fn search_results_match_across_scheme_www_and_trailing_slash() {
    let returned = "http://screener.in/company/RELIANCE/consolidated";
    let c = checked(SCREENER, OPERATOR, &[returned]);
    assert_eq!(allowed(&c), Some((returned, Source::Search)));
}

/// Web `test_the_model_cannot_choose_among_spellings_of_an_accepted_url`:
/// every spelling is accepted and every one sends the same text; the
/// model's own spelling would carry bits in its casing.
#[test]
fn the_model_cannot_choose_among_spellings_of_an_accepted_url() {
    for spelling in [
        "https://www.screener.in/company/reliance/consolidated/",
        "HTTPS://WWW.SCREENER.IN/COMPANY/RELIANCE/CONSOLIDATED/",
        "http://screener.in/company/RELIANCE/consolidated",
        "https://www.screener.in/company/RELIANCE/consolidated/#x",
    ] {
        let c = checked(spelling, OPERATOR, &[SCREENER]);
        assert_eq!(
            allowed(&c),
            Some((SCREENER, Source::Search)),
            "{}",
            spelling
        );
    }
}

/// Web `test_an_operator_url_is_sent_as_the_operator_typed_it_not_as_the_model_spelled_it`.
#[test]
fn an_operator_url_is_sent_as_the_operator_typed_it_not_as_the_model_spelled_it() {
    let c = checked(
        "https://screener.in/company/tcs",
        "read Screener.IN/Company/TCS please",
        &[],
    );
    assert_eq!(
        allowed(&c),
        Some(("https://Screener.IN/Company/TCS", Source::Operator))
    );
}

/// Web `test_a_built_screener_page_is_rebuilt_from_the_validated_symbol`.
#[test]
fn a_built_screener_page_is_rebuilt_from_the_validated_symbol() {
    let c = preferred(
        "HTTPS://SCREENER.IN/company/reliance/consolidated",
        "show reliance ratios",
    );
    assert_eq!(
        allowed(&c),
        Some((
            "https://www.screener.in/company/RELIANCE/consolidated/",
            Source::Preferred
        ))
    );
}

/// Web `test_a_url_the_operator_typed_is_accepted_as_written`.
#[test]
fn a_url_the_operator_typed_is_accepted_as_written() {
    let c = checked(
        "https://www.screener.in/company/TCS/",
        "read https://www.screener.in/company/TCS/ and summarise it",
        &[],
    );
    assert_eq!(allowed(&c).map(|(_, s)| s), Some(Source::Operator));
}

/// Web `test_a_prefix_of_a_pasted_link_is_accepted_but_not_an_extension_of_it`:
/// the model adding a query string is exactly the exfiltration shape.
#[test]
fn a_prefix_of_a_pasted_link_is_accepted_but_not_an_extension_of_it() {
    let message = "look at screener.in/company/TCS";
    assert!(allowed(&checked("https://screener.in/company/TCS", message, &[])).is_some());
    assert!(allowed(&checked(
        "https://screener.in/company/TCS?x=secret",
        message,
        &[]
    ))
    .is_none());
}

/// Web `test_private_credentialed_and_non_web_addresses_are_refused_even_if_typed`.
#[test]
fn private_credentialed_and_non_web_addresses_are_refused_even_if_typed() {
    for url in [
        "http://localhost/admin",
        "http://127.0.0.1:5000/api/v1/funds",
        "http://169.254.169.254/latest/meta-data",
        "http://10.0.0.5/",
        "https://printer.local/",
        "http://intranet/",
        "ftp://screener.in/company/TCS",
        "https://user:pass@screener.in/company/TCS",
        "https://screener.in:8443/company/TCS",
        "",
        "   ",
    ] {
        let message = format!("fetch {}", url);
        let c = checked(url, &message, &[url]);
        assert!(allowed(&c).is_none(), "{:?} was accepted", url);
    }
}

/// Web `test_a_fragment_is_dropped_before_the_url_is_sent`.
#[test]
fn a_fragment_is_dropped_before_the_url_is_sent() {
    let c = checked(&format!("{}#profit-loss", SCREENER), OPERATOR, &[SCREENER]);
    let (url, _) = allowed(&c).unwrap();
    assert!(!url.contains('#'));
}

/// Web `test_a_screener_page_for_a_symbol_the_operator_wrote_needs_no_search`.
#[test]
fn a_screener_page_for_a_symbol_the_operator_wrote_needs_no_search() {
    let c = preferred(SCREENER, "show reliance fundamentals");
    assert_eq!(allowed(&c).map(|(_, s)| s), Some(Source::Preferred));
}

/// Web `test_the_standalone_screener_page_is_accepted_too`.
#[test]
fn the_standalone_screener_page_is_accepted_too() {
    let c = preferred(
        "https://www.screener.in/company/RELIANCE/",
        "reliance ratios",
    );
    assert_eq!(
        allowed(&c),
        Some((
            "https://www.screener.in/company/RELIANCE/",
            Source::Preferred
        ))
    );
}

/// Web `test_a_symbol_the_operator_never_wrote_is_still_refused`.
#[test]
fn a_symbol_the_operator_never_wrote_is_still_refused() {
    let c = preferred(
        "https://www.screener.in/company/INFY/consolidated/",
        "reliance ratios",
    );
    assert!(allowed(&c).is_none());
}

/// Web `test_only_the_exact_company_page_shape_on_the_exact_domain_is_built`.
#[test]
fn only_the_exact_company_page_shape_on_the_exact_domain_is_built() {
    for url in [
        "https://www.screener.in/company/RELIANCE/consolidated/?next=secret",
        "https://www.screener.in/company/RELIANCE/consolidated/extra",
        "https://www.screener.in/user/login/",
        "https://screener.in.evil.example/company/RELIANCE/consolidated/",
        "https://notscreener.in/company/RELIANCE/consolidated/",
    ] {
        assert!(
            allowed(&preferred(url, "reliance ratios")).is_none(),
            "{}",
            url
        );
    }
}

/// Web `test_scanx_pages_are_never_built_from_a_symbol`: ScanX names a
/// page after the company, so there is nothing to derive.
#[test]
fn scanx_pages_are_never_built_from_a_symbol() {
    let c = preferred("https://scanx.trade/company/reliance", "reliance ratios");
    assert!(allowed(&c).is_none());
}

/// Web `test_host_matching_is_exact_on_the_domain`.
#[test]
fn host_matching_is_exact_on_the_domain() {
    assert!(host_matches("https://www.screener.in/x", "screener.in"));
    assert!(host_matches("https://screener.in/x", "screener.in"));
    assert!(!host_matches(
        "https://screener.in.evil.example/",
        "screener.in"
    ));
    assert!(!host_matches("https://notscreener.in/", "screener.in"));
}

/// Desktop: an address a provider would read as this computer, though
/// Python's reading of it names no address, is refused too.
#[test]
fn an_address_a_provider_reads_as_this_computer_is_refused() {
    for url in [
        "http://127.1/admin",
        "http://0x7f.0.0.1/",
        "http://2130706433.example@127.0.0.1/",
        "http://127.0.0.1\\@screener.in/",
        "http://[::1]/",
        "http://[::ffff:10.0.0.1]/",
        "http://100.64.0.1/",
    ] {
        let message = format!("fetch {}", url);
        let c = checked(url, &message, &[url]);
        assert!(allowed(&c).is_none(), "{:?} was accepted", url);
    }
}

/// Desktop: what is sent passes the same checks as what was asked for. A
/// search that returned a page under a login or another port matches the
/// model's plain spelling of it, and is still refused.
#[test]
fn the_address_that_is_sent_passes_the_same_checks() {
    for returned in [
        "https://user@www.screener.in/company/RELIANCE/consolidated/",
        "https://www.screener.in:8443/company/RELIANCE/consolidated/",
    ] {
        let c = checked(SCREENER, OPERATOR, &[returned]);
        assert!(allowed(&c).is_none(), "{} was sent", returned);
    }
}

/// Desktop: a company page is built only from a symbol that is one of the
/// trader's words, whole, as the search boundary takes words; the web took
/// any part of a word, so the model could choose among fragments.
#[test]
fn a_company_page_needs_the_whole_word_the_trader_wrote() {
    let fragment = preferred("https://www.screener.in/company/REL/", "reliance ratios");
    assert!(allowed(&fragment).is_none());
    let whole = preferred(
        "https://www.screener.in/company/M&M/",
        "ratios of M&M please",
    );
    assert_eq!(
        allowed(&whole),
        Some(("https://www.screener.in/company/M&M/", Source::Preferred))
    );
}

/// Desktop: a page a provider ended up on after a redirect into a private
/// or local address is refused; another public page is not.
#[test]
fn a_redirect_into_a_private_address_is_refused() {
    for to in [
        "http://127.0.0.1/admin",
        "http://localhost:8080/",
        "http://192.168.1.1/",
        "http://127.1/",
        "http://metadata.internal/",
    ] {
        assert!(redirect_refusal(to).is_some(), "{}", to);
    }
    for to in [
        "https://www.screener.in/company/RELIANCE/",
        "https://example.com:8443/page",
        "",
    ] {
        assert_eq!(redirect_refusal(to), None, "{}", to);
    }
}

/// Two spellings of one page share a key; the key drops the scheme, `www.`,
/// the port, the fragment and a trailing slash.
#[test]
fn two_spellings_of_one_page_share_a_key() {
    assert_eq!(
        url_key("HTTPS://WWW.Screener.in:443/company/TCS/#x"),
        Some("screener.in/company/tcs".into())
    );
    assert_eq!(
        url_key("http://screener.in/company/TCS?a=1"),
        Some("screener.in/company/tcs?a=1".into())
    );
    assert_eq!(url_key("http://[::1/"), None);
}
