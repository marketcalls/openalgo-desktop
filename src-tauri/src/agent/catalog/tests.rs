use super::load::{bundled, with_fetched};
use super::*;
use serde_json::{json, Value};

#[test]
fn the_grid_offers_what_genai_reaches_sorted_by_name() {
    let c = bundled();
    let cards = c.list_providers();
    let ids: Vec<&str> = cards.iter().map(|p| p.id.as_str()).collect();
    for id in [
        "openai",
        "anthropic",
        "gemini",
        "ollama",
        "groq",
        "lm_studio",
        "chatgpt",
    ] {
        assert!(ids.contains(&id), "{}", id);
    }
    for hidden in ["bedrock", "vertex_ai", "sagemaker", "azure"] {
        assert!(!ids.contains(&hidden), "{}", hidden);
    }
    let names: Vec<String> = cards
        .iter()
        .map(|p| p.display_name.to_lowercase())
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    let card = c.get_provider(" openai ").unwrap().to_json();
    let mut keys: Vec<&str> = card
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut want = vec![
        "id",
        "display_name",
        "icon",
        "provider_kind",
        "needs_key",
        "needs_base_url",
        "model_count",
        "total_model_count",
    ];
    want.sort_unstable();
    assert_eq!(keys, want, "the web's fields, whatever their order");
    assert_eq!(card["provider_kind"], "openai");
    let lm = c.get_provider("lm_studio").unwrap();
    assert!(!lm.needs_key && lm.needs_base_url);
    assert_eq!(lm.provider_kind, "openai_compatible");
    assert_eq!(c.get_provider("groq").unwrap().provider_kind, "litellm");
    assert!(c.get_provider("bedrock").is_none());
}

#[test]
fn a_providers_models_carry_context_price_and_tool_support() {
    let c = bundled();
    let all = c.list_models("openai", false);
    let chat = c.list_models("openai", true);
    assert!(
        chat.len() < all.len(),
        "embedding and image models are dropped"
    );
    assert!(chat.iter().all(ModelInfo::is_chat));
    let gpt = chat.iter().find(|m| m.id == "gpt-4o").unwrap().to_json();
    let mut keys: Vec<&str> = gpt
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    let mut want = vec![
        "id",
        "provider",
        "qualified_id",
        "catalog_key",
        "mode",
        "max_input_tokens",
        "max_output_tokens",
        "input_price_per_million",
        "output_price_per_million",
        "supports_function_calling",
        "supports_vision",
        "supports_reasoning",
        "in_catalog",
        "is_chat",
    ];
    want.sort_unstable();
    assert_eq!(keys, want, "the web's fields, whatever their order");
    assert_eq!(gpt["qualified_id"], "openai/gpt-4o");
    assert_eq!(gpt["supports_function_calling"], json!(true));
    assert!(gpt["input_price_per_million"].as_f64().unwrap() > 0.0);
    let groq = c.list_models("groq", true);
    assert!(groq.iter().all(|m| m.qualified_id().starts_with("groq/")));
    assert!(c.list_models("nobody", true).is_empty());
}

#[test]
fn a_model_is_found_with_or_without_its_prefix() {
    let c = bundled();
    let bare = c.model_meta("gpt-4o", None).unwrap();
    assert_eq!(
        (bare.provider.as_str(), bare.catalog_key.as_deref()),
        ("openai", Some("gpt-4o"))
    );
    let prefixed = c.model_meta("openai/gpt-4o", None).unwrap();
    assert_eq!(prefixed.catalog_key.as_deref(), Some("gpt-4o"));
    assert_eq!(prefixed.id, "openai/gpt-4o");
    assert!(c.model_meta("ollama/not-a-real-model", None).is_none());
    assert!(c.model_meta("  ", None).is_none());
}

/// Never a guessed price: an unknown model costs `None`, a plan model too
/// (the web's test_the_catalogue_declines_to_price_a_plan_model), a priced
/// one the web's arithmetic.
#[test]
fn a_turn_is_costed_from_the_table_or_not_at_all() {
    let c = bundled();
    assert_eq!(c.estimate_cost("chatgpt/gpt-5.4", 1000, 500, 0), None);
    let api = c.estimate_cost("openai/gpt-5.4", 1000, 500, 0).unwrap();
    assert!(api > 0.0);
    assert_eq!(
        c.estimate_cost("openai/not-a-real-model", 1000, 500, 0),
        None
    );
    assert_eq!(c.estimate_cost("gpt-4o", 0, 0, 0), Some(0.0));
    let e = c.entry("gpt-4o").unwrap();
    let expected = 800.0 * e.input_cost.unwrap()
        + 200.0 * e.cache_read_cost.unwrap()
        + 100.0 * e.output_cost.unwrap();
    let got = c.estimate_cost("gpt-4o", 1000, 100, 200).unwrap();
    assert!((got - expected).abs() < 1e-12, "{} {}", got, expected);
    assert_eq!(
        c.estimate_cost("gpt-4o", 10, 0, 50),
        c.estimate_cost("gpt-4o", 10, 0, 10),
        "cached is part of the input"
    );
}

fn table(count: usize) -> Value {
    let mut m = serde_json::Map::new();
    m.insert("sample_spec".into(), json!({"mode": "prose"}));
    for i in 0..count {
        m.insert(
            format!("filler-{}", i),
            json!({"litellm_provider": "azure", "mode": "chat"}),
        );
    }
    m.insert(
        "gpt-4o".into(),
        json!({"litellm_provider": "openai", "mode": "chat", "input_cost_per_token": 0.000_001,
               "output_cost_per_token": 0.000_002, "max_input_tokens": 999_999,
               "supports_function_calling": true}),
    );
    m.insert(
        "gpt-new-launch".into(),
        json!({"litellm_provider": "openai", "mode": "chat", "aliases": ["gpt-new"]}),
    );
    m.insert(
        "ft:gpt-4o:me".into(),
        json!({"litellm_provider": "openai", "mode": "chat"}),
    );
    Value::Object(m)
}

/// The startup refresh takes a newer table only when LiteLLM would: a
/// JSON object no less than half the bundled size; a new model of an
/// offered provider joins its list, a fine-tune does not.
#[test]
fn a_fetched_table_replaces_the_bundled_one_only_when_whole() {
    assert!(with_fetched(b"not json").is_err());
    assert!(with_fetched(b"[1, 2]").is_err());
    let small = serde_json::to_vec(&table(10)).unwrap();
    assert!(
        with_fetched(&small).is_err(),
        "a truncated table is refused"
    );
    let whole = serde_json::to_vec(&table(3000)).unwrap();
    let next = with_fetched(&whole).unwrap();
    let gpt = next.model_meta("gpt-4o", None).unwrap().entry.unwrap();
    assert_eq!(
        gpt.max_input_tokens,
        Some(999_999),
        "the fetched price wins"
    );
    let listed: Vec<String> = next
        .list_models("openai", false)
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert!(listed.contains(&"gpt-new-launch".to_string()));
    assert!(next.entry("gpt-new").is_some(), "an alias is an entry");
    assert!(
        !listed.contains(&"ft:gpt-4o:me".to_string()),
        "a fine-tune is priced, not listed"
    );
    assert!(
        next.entry("filler-1").is_none(),
        "providers not offered are not kept"
    );
}

#[test]
fn unknown_is_never_zero() {
    let c = bundled();
    let unpriced = ModelInfo {
        id: "x".into(),
        provider: "lm_studio".into(),
        catalog_key: None,
        entry: None,
    };
    let v = unpriced.to_json();
    assert_eq!(v["input_price_per_million"], Value::Null);
    assert_eq!(v["supports_function_calling"], Value::Null);
    assert_eq!(v["in_catalog"], json!(false));
    assert_eq!(v["is_chat"], json!(true));
    let answer = models_answer(Some("openai"), Some("FALSE")).unwrap();
    assert_eq!(answer["status"], "success");
    assert_eq!(
        answer["data"].as_array().unwrap().len(),
        c.list_models("openai", false).len()
    );
    let missing = models_answer(Some("  "), None).unwrap_err();
    assert_eq!(
        (missing.status, missing.message()),
        (400, "A provider is required")
    );
    let nobody = models_answer(Some("nobody"), None).unwrap();
    assert_eq!(nobody["provider"], Value::Null);
    assert_eq!(nobody["data"], json!([]));
    assert_eq!(providers_answer()["available"], json!(true));
}

/// Serve one HTTP answer on a local port; the address to fetch.
async fn serve_once(head: String, body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://127.0.0.1:{}/table.json", port), task)
}

#[tokio::test]
async fn the_refresh_fetches_one_bounded_table() {
    let client = crate::agent::run::client::http_client().unwrap();
    let table = serde_json::to_vec(&table(3000)).unwrap();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        table.len()
    );
    let (url, task) = serve_once(head, table.clone()).await;
    let body = refresh::fetch(&client, &url).await.unwrap();
    assert_eq!(body, table);
    task.await.unwrap();
    let huge = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        refresh::MAX_BYTES + 1
    );
    let (url, task) = serve_once(huge, Vec::new()).await;
    assert!(
        refresh::fetch(&client, &url).await.is_err(),
        "refused before it is read"
    );
    task.abort();
    let (url, task) = serve_once(
        "HTTP/1.1 503 X\r\nContent-Length: 0\r\n\r\n".into(),
        Vec::new(),
    )
    .await;
    assert_eq!(
        refresh::fetch(&client, &url).await.unwrap_err(),
        "the server answered 503"
    );
    task.await.unwrap();
}
