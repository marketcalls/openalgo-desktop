use super::*;

/// The routing alone; what reaches `/api/v1` is pinned in `tools::tests`.
#[test]
fn an_index_routes_to_its_own_exchange_and_a_named_one_wins() {
    assert_eq!(quote_exchange("sensex", ""), "BSE_INDEX");
    assert_eq!(quote_exchange("NIFTY", ""), "NSE_INDEX");
    assert_eq!(quote_exchange("BSE", ""), "NSE");
    assert_eq!(quote_exchange("SENSEX", "nse"), "NSE");
    assert_eq!(derivatives_exchange("BANKEX", ""), "BFO");
    assert_eq!(derivatives_exchange("NIFTY", ""), "NFO");
    assert_eq!(derivatives_exchange("CRUDEOIL", "mcx"), "MCX");
}

/// Web test/test_mcp_integrity.py::test_exchange_is_optional_and_described:
/// the schema and description are all a client's model sees.
#[test]
fn exchange_is_optional_and_described() {
    for (name, must_mention) in [
        ("get_quote", ["BSE_INDEX", "NSE_INDEX"]),
        ("get_expiry_dates", ["BFO", "NFO"]),
    ] {
        let Some(tool) = crate::mcp::tool(name) else {
            panic!("{name} is not registered");
        };
        let d = tool.descriptor();
        let schema = &d["inputSchema"];
        assert!(schema["properties"].get("exchange").is_some(), "{name}");
        let required = schema["required"].as_array().cloned().unwrap_or_default();
        assert!(!required.contains(&serde_json::json!("exchange")), "{name}");
        let description = d["description"].as_str().unwrap_or_default();
        for code in must_mention {
            assert!(description.contains(code), "{name} does not mention {code}");
        }
    }
}
