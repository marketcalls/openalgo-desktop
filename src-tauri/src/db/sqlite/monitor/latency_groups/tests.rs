use super::*;
use std::collections::{BTreeSet, HashMap};

/// Every `/api/v1` route the desktop registers, by the path the latency
/// recorder names it by (`services::monitor::api_type`): read from the
/// route table itself, so a route added there is seen here.
fn registered_paths() -> BTreeSet<String> {
    let source = include_str!("../../../../server/api_v1/mod.rs");
    let mut paths = BTreeSet::new();
    for piece in source.split("\"/api/v1/").skip(1) {
        let Some(end) = piece.find('"') else {
            continue;
        };
        let path = piece[..end].trim_end_matches('/');
        // `ticker/{symbol}` is recorded by its first segment.
        let path = path.split("/{").next().unwrap_or(path);
        paths.insert(path.to_string());
    }
    paths
}

/// Web test/test_latency_groups.py::test_every_registered_endpoint_is_in_a_group:
/// a new endpoint has to be placed, or its calls fall into "Other" unseen.
#[test]
fn every_api_v1_route_is_in_a_group() {
    let paths = registered_paths();
    assert!(paths.len() > 40, "the route table was not read: {paths:?}");
    let unplaced: Vec<_> = paths.iter().filter(|p| tag_for_path(p).is_none()).collect();
    assert!(
        unplaced.is_empty(),
        "{unplaced:?} is registered in server/api_v1 but is in no latency group. \
         Add an Endpoint for it to the right Group in db/sqlite/monitor/latency_groups.rs."
    );
}

/// Web test/test_latency_groups.py::test_no_group_names_an_endpoint_that_does_not_exist,
/// less the web's Portfolio and SIP endpoints, which the desktop does not
/// carry (out of scope) and keeps only so the groups read as the web's.
#[test]
fn no_group_names_a_path_that_does_not_exist() {
    let registered: BTreeSet<String> = registered_paths()
        .iter()
        .map(|p| p.split('/').next().unwrap_or(p).to_string())
        .chain(registered_paths())
        .collect();
    let unknown: Vec<_> = GROUPS
        .iter()
        .flat_map(|g| g.endpoints.iter())
        .flat_map(|e| e.paths.iter())
        .filter(|p| !registered.contains(**p) && !["portfolio", "sip"].contains(p))
        .collect();
    assert!(unknown.is_empty(), "no such /api/v1 route: {unknown:?}");
}

/// Web test/test_latency_groups.py::test_a_tag_belongs_to_one_group_and_fits_its_column
#[test]
fn a_tag_belongs_to_one_group_and_fits_its_column() {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for g in GROUPS {
        for e in g.endpoints {
            assert!(seen.insert(e.tag, g.key).is_none(), "{} twice", e.tag);
            // `insert_latency` caps the column at 20 characters.
            assert!(e.tag.len() <= 20, "{}", e.tag);
            assert!(!e.label.is_empty() && !e.paths.is_empty(), "{}", e.tag);
        }
    }
    for (old, new) in LEGACY_TAGS {
        assert!(!seen.contains_key(old), "{old} is also a current tag");
        assert!(seen.contains_key(new), "{new} is no tag");
    }
}

/// Web test/test_latency_groups.py::test_every_order_endpoint_is_kept_as_an_order
#[test]
fn every_order_endpoint_is_kept_as_an_order() {
    let kept = order_tags();
    for path in [
        "placeorder",
        "placesmartorder",
        "optionsorder",
        "optionsmultiorder",
        "basketorder",
        "splitorder",
        "modifyorder",
        "cancelorder",
        "cancelallorder",
        "closeposition",
        "placegttorder",
        "modifygttorder",
        "cancelgttorder",
    ] {
        let Some(tag) = tag_for_path(path) else {
            panic!("{path} has no tag");
        };
        assert_eq!(group_key_of(Some(tag)), ORDERS, "{path}");
        assert!(
            kept.contains(&tag),
            "{path} is recorded as {tag}, which is not kept"
        );
    }
    // Rows already written under the old spellings are still orders.
    assert!(kept.contains(&"OPTIONSORDER") && kept.contains(&"OPTIONSMULTIORDER"));
    assert_eq!(canonical("OPTIONSORDER"), "OPTIONS");
}

/// Web test/test_latency_groups.py::test_a_read_is_not_an_order
#[test]
fn a_read_is_not_an_order() {
    for path in [
        "gttorderbook",
        "orderbook",
        "orderstatus",
        "history",
        "quotes",
        "funds",
    ] {
        let tag = tag_for_path(path);
        assert!(tag.is_some(), "{path}");
        assert!(!order_tags().contains(&tag.unwrap_or_default()), "{path}");
    }
}

/// The desktop's own earlier spellings of the options tools are read as
/// the endpoints they are, in the Options group.
#[test]
fn the_desktops_old_options_tags_are_options_calls() {
    for (old, label) in [
        ("GREEKS", "Option Greeks"),
        ("MULTI_GREEKS", "Batch option Greeks"),
        ("OPTION_SYMBOL", "Option symbol"),
        ("SYNTHETIC", "Synthetic future"),
    ] {
        assert_eq!(group_key_of(Some(old)), "options", "{old}");
        assert_eq!(label_of(Some(old)), label);
    }
    assert_eq!(label_of(Some("SOMETHING_NEW")), "SOMETHING_NEW");
    assert_eq!(label_of(None), "Unknown");
    assert_eq!(group_key_of(None), OTHER);
}

#[test]
fn paths_with_more_segments_take_their_first_unless_listed_whole() {
    assert_eq!(tag_for_path("market/holidays"), Some("MARKET_HOLIDAYS"));
    assert_eq!(tag_for_path("strategy/list"), Some("STRATEGY"));
    assert_eq!(tag_for_path("ticker/NSE:SBIN"), Some("TICKER"));
    assert_eq!(tag_for_path("analyzer/toggle"), Some("ANALYZER"));
    assert_eq!(tag_for_path("pnl/symbols"), Some("PNL"));
    assert_eq!(tag_for_path("newthing"), None);
    assert_eq!(
        group_keys().collect::<Vec<_>>(),
        [
            "orders",
            "account",
            "market_data",
            "options",
            "reference",
            "tools",
            "other",
            "all"
        ]
    );
}

/// Web blueprints/latency.py `requested_group`: the default when none is
/// named, any case, and a name that is no group refused in plain words.
#[test]
fn a_requested_group_is_read_or_refused_in_plain_words() {
    assert_eq!(requested(None, ORDERS), Ok("orders"));
    assert_eq!(requested(Some("  "), ALL), Ok("all"));
    assert_eq!(requested(Some(" Market_Data "), ORDERS), Ok("market_data"));
    let Err(message) = requested(Some("nonsense"), ORDERS) else {
        panic!("nonsense is no group");
    };
    assert_eq!(
        message,
        "That is not a latency group. The groups are: Orders, Account and books, Market data, \
         Options, Symbols and settings, Tools and alerts, Other, All calls."
    );
}
