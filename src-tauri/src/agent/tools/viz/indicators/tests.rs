//! The web's `TestChartIndicators` (test_agent_tool_arguments.py) and the
//! refusal wordings of `tools/viz.py`, ported; expected suggestion lists
//! were recorded from Python's own `difflib` over the committed catalogue.

use super::*;

fn ids(list: &[Value]) -> Vec<&str> {
    list.iter()
        .map(|i| i["id"].as_str().unwrap_or_default())
        .collect()
}

fn refusal(outcome: Result<impl std::fmt::Debug, ToolOutcome>) -> String {
    match outcome {
        Err(ToolOutcome::Retry(text)) => text,
        other => panic!("not refused: {:?}", other),
    }
}

/// Q-438.
#[test]
fn aliases_and_hyphenated_ids() {
    let got = normalise_indicators(Some(&json!([
        "bbands:20",
        "psar",
        "parabolic-sar",
        "stochastic_rsi",
        "ema(50)"
    ])))
    .unwrap();
    assert_eq!(
        ids(&got),
        [
            "bollinger",
            "parabolic-sar",
            "parabolic-sar",
            "stochastic-rsi",
            "ema"
        ]
    );
    assert_eq!(got[0]["inputs"], json!({"length": 20}));
    assert_eq!(got[4]["inputs"], json!({"length": 50}));
}

/// Q-439.
#[test]
fn a_bare_string_a_json_string_and_an_object() {
    assert_eq!(
        normalise_indicators(Some(&json!("ema:20"))).unwrap(),
        vec![json!({"id": "ema", "inputs": {"length": 20}})]
    );
    let text = normalise_indicators(Some(&json!(r#"["ema:20", "rsi:14"]"#))).unwrap();
    assert_eq!(text[1]["id"], "rsi");
    let object = json!({"id": "supertrend", "inputs": {"multiplier": 3}});
    assert_eq!(
        normalise_indicators(Some(&object)).unwrap(),
        vec![object.clone()]
    );
    // A single object sent as text is one entry, not split on its commas.
    let one = normalise_indicators(Some(&json!(
        r#"{"id": "supertrend", "inputs": {"length": 10, "multiplier": 3}}"#
    )))
    .unwrap();
    assert_eq!(ids(&one), ["supertrend"]);
    assert_eq!(
        ids(&normalise_indicators(Some(&json!("ema:20, rsi:14\nmacd"))).unwrap()),
        ["ema", "rsi", "macd"]
    );
    for nothing in [None, Some(json!(null)), Some(json!("")), Some(json!("  "))] {
        assert!(normalise_indicators(nothing.as_ref()).unwrap().is_empty());
    }
}

/// Q-440: an unknown id is refused with the close matches.
#[test]
fn an_unknown_id_is_refused_with_close_matches() {
    let text = refusal(normalise_indicators(Some(&json!(["bolinger"]))));
    assert_eq!(
        text,
        "The 'indicators' argument is invalid: entry 1 names 'bolinger', which this chart cannot draw The closest ids the chart has are: bollinger. Use one exactly as written, or leave the indicator out."
    );
    let none = refusal(normalise_indicators(Some(&json!(["zzzz"]))));
    assert!(none.ends_with("Common ids are 'ema', 'sma', 'rsi', 'macd', 'bollinger', 'vwap', 'atr' and 'supertrend'. Use one exactly as written, or leave the indicator out."), "{}", none);
}

/// The suggestions are Python's: related ids first, then
/// `difflib.get_close_matches` (recorded from Python over the catalogue).
#[test]
fn suggestions_match_python_difflib() {
    let known = chart_catalogue::ids();
    for (word, expected) in [
        ("bolinger", vec!["bollinger"]),
        ("supertrnd", vec!["supertrend"]),
        ("stochastic-x", vec!["stochastic", "stochastic-rsi"]),
        ("macdx", vec!["macd", "adx"]),
        ("vwma-x", vec!["vwma", "wma", "vwap"]),
        ("zzzz", vec![]),
        ("keltner", vec!["keltner-channel"]),
        ("donchian-channel", vec!["donchian", "ma-channel"]),
    ] {
        assert_eq!(close_indicator_ids(word, known), expected, "{}", word);
    }
    let ordered: Vec<&str> = known.iter().map(String::as_str).collect();
    for (word, expected) in [
        ("sma-x", vec!["sma", "smma", "lsma"]),
        ("ema2", vec!["ema", "tema", "dema", "zlema"]),
        ("adxr", vec!["adx"]),
        ("heikin", vec![]),
        ("bbands-x", vec![]),
        (
            "williams",
            vec![
                "williams-vix-fix",
                "williams-fractals",
                "williams-percent-r",
            ],
        ),
        ("donchian-channel", vec!["ma-channel", "donchian"]),
    ] {
        assert_eq!(
            get_close_matches(word, &ordered, 6, 0.6),
            expected,
            "{}",
            word
        );
    }
    assert_eq!(ratio(&['a', 'b', 'c', 'd'], &['b', 'c', 'd', 'e']), 0.75);
    assert_eq!(ratio(&[], &[]), 1.0);
}

/// Q-441: every shape the docstring shows is admitted.
#[test]
fn the_parser_admits_every_shape_the_docstring_shows() {
    for value in [
        json!(["ema:20"]),
        json!(r#"["ema:20"]"#),
        json!("ema:20"),
        json!([{"id": "supertrend", "inputs": {"length": 10, "multiplier": 3}}]),
        json!(null),
    ] {
        assert!(normalise_indicators(Some(&value)).is_ok(), "{}", value);
    }
}

#[test]
fn refusals_read_as_the_web_words_them() {
    let cases = [
        (
            json!(["a", "b", "c", "d", "e", "f", "g"]),
            "it carries 7 entries, more than the 6 one chart shows Ask for the few that answer the question; a chart carrying more is unreadable.",
        ),
        (
            json!("[ema:20"),
            "it is a string that is not valid JSON Pass a list, for example [\"ema:20\", \"rsi:14\"].",
        ),
        (
            json!(5),
            "it is int, not a list Pass a list, for example [\"ema:20\", \"ema:50\"].",
        ),
        (
            json!(["ema 20 30"]),
            "entry 1 is 'ema 20 30', which is not an indicator Use 'ema:20', or an object such as {\"id\": \"supertrend\", \"inputs\": {\"length\": 10, \"multiplier\": 3}}.",
        ),
        (
            json!([7]),
            "entry 1 is int, not an indicator name or object Pass strings such as \"ema:20\", or objects carrying an \"id\".",
        ),
        (
            json!([{"inputs": {}}]),
            "entry 1 names nothing, which is not an indicator id Use a lower-case id such as 'ema', 'sma', 'rsi', 'macd', 'bollinger', 'vwap', 'atr' or 'supertrend'.",
        ),
        (
            json!([{"id": "ema", "inputs": {"a": 1, "b": 2, "c": 3, "d": 4, "e": 5, "f": 6, "g": 7, "h": 8, "i": 9}}]),
            "one indicator declares 9 inputs, more than the 8 allowed Name only the inputs you are changing; the chart supplies its own defaults for the rest.",
        ),
    ];
    for (value, tail) in cases {
        let text = refusal(normalise_indicators(Some(&value)));
        assert_eq!(
            text,
            format!("The 'indicators' argument is invalid: {}", tail),
            "{}",
            value
        );
    }
}

#[test]
fn an_object_reads_its_id_inputs_and_length_however_spelled() {
    let got = normalise_indicators(Some(&json!([
        {"Name": "EMA", "Period": 21},
        {"indicator": "sma", "length": 50.0, "inputs": {"source": "  hl2 ", "bad key": 1, "n": [1]}},
        {"id": "rsi", "length": 9, "inputs": {"length": 14}}
    ])))
    .unwrap();
    assert_eq!(got[0], json!({"id": "ema", "inputs": {"length": 21}}));
    assert_eq!(
        got[1],
        json!({"id": "sma", "inputs": {"source": "hl2", "length": 50}})
    );
    assert_eq!(got[2]["inputs"], json!({"length": 14}));
    assert_eq!(overlay_label(&got[0]), "ema(21)");
    assert_eq!(overlay_label(&json!({"id": "vwap", "inputs": {}})), "vwap");
}

/// T-222: the shapes, their aliases and the refusal.
#[test]
fn the_chart_shape_is_one_the_terminal_draws() {
    assert_eq!(chart_type(None).unwrap(), "candlestick");
    assert_eq!(chart_type(Some(&json!(""))).unwrap(), "candlestick");
    assert_eq!(chart_type(Some(&json!("Candles"))).unwrap(), "candlestick");
    assert_eq!(
        chart_type(Some(&json!("heikin_ashi"))).unwrap(),
        "heikin-ashi"
    );
    assert_eq!(
        refusal(chart_type(Some(&json!("Renko")))),
        "The 'chart_type' argument is invalid: 'renko' is not a chart shape this terminal draws Use one of: candlestick, hollow-candle, bar, high-low, line, step, area, baseline, heikin-ashi."
    );
}

/// Q-442 and T-230.
#[test]
fn surface_expiries_accept_a_string_and_name_the_right_argument() {
    assert_eq!(
        expiries(Some(&json!("28-NOV-25, 2025-12-26"))).unwrap(),
        ["28NOV25", "26DEC25"]
    );
    assert_eq!(
        expiries(Some(&json!(r#"["28NOV25", "28NOV25", "26DEC25"]"#))).unwrap(),
        ["28NOV25", "26DEC25"]
    );
    assert!(refusal(expiries(Some(&json!(["28XYZ25"])))).contains("'expiry_dates' argument"));
    assert_eq!(
        refusal(expiries(Some(&json!("[28NOV25")))),
        "The 'expiry_dates' argument is invalid: it is a string that is not valid JSON Pass a list, for example [\"28NOV25\", \"26DEC25\"]."
    );
    for empty in [json!([]), json!(""), json!({"a": 1}), json!(null)] {
        assert!(refusal(expiries(Some(&empty)))
            .contains("it is empty or is not a list Pass at least one expiry"));
    }
    let nine: Vec<String> = (1..=9).map(|d| format!("{:02}NOV25", d)).collect();
    assert_eq!(
        refusal(expiries(Some(&json!(nine)))),
        "The 'expiry_dates' argument is invalid: it carries 9 expiries, more than the 8 one surface spans Ask for the near expiries that answer the question."
    );
}
