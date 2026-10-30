//! The Flow documents against the code (parity IE-17..IE-23, F-412, F-413).
//!
//! `docs/prompt/flow-import-format.md` (the long form the Agent's Flow tools
//! cite and the flow-builder skill serves) and the user guide's chapter 21
//! say what the desktop does: the values the code uses, the files it lives
//! in, the sentences it answers with, and only the nodes and controls it
//! has. Each test reads the document and the code side by side, so a change
//! to either that makes them disagree fails here, not in a trader's hands.

use crate::flow::contract::client::app::{CALL_TIMEOUT, NO_ANSWER};
use crate::flow::contract::time::{DELAY_MAX_SECONDS, WAIT_UNTIL_MAX_SECONDS};
use crate::flow::executor::waits::{INLINE_WAIT_SECONDS, STARTED_IN_BACKGROUND};
use crate::flow::indicator_service::{list_supported_indicators, MAX_HISTORY_BARS};
use crate::flow::nodes::history::HISTORY_CACHE_TTL;
use crate::flow::service::webhook_guard::{LOCKOUT_FAILURES, LOCKOUT_WINDOW, LOCK_DURATION};
use crate::flow::store::executions::{INTERRUPTED, RETENTION_COUNT, RETENTION_DAYS};
use crate::flow::triggers::price_monitor::POLL_INTERVAL;
use crate::flow::triggers::scheduler::{INTERVAL_ALIGN_OFFSET_SECONDS, MISFIRE_GRACE_SECS};
use crate::flow::validator::{validate_workflow, ValidateOptions, MAX_NODES, VALID_NODE_TYPES};
use crate::server::ratelimit::Bucket;
use crate::services::core::WEBHOOK_LOCKED;
use regex::Regex;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const IMPORT_FORMAT: &str = "docs/prompt/flow-import-format.md";
const CHAPTER_21: &str = "docs/userguide/21-flow-visual-builder/README.md";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// A repository file as text, line endings as LF (a Windows checkout may
/// hold it with CRLF).
fn repo(relative: &str) -> String {
    let path = root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .replace("\r\n", "\n")
}

/// Whitespace runs as one space, so a phrase matches across line wraps.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// One Markdown section by its heading prefix, subsections included, up to
/// its next peer (as the web's prompt tests cut it).
fn section<'a>(doc: &'a str, prefix: &str) -> &'a str {
    let head = Regex::new(&format!(r"(?m)^{}(?:\s.*)?$", regex::escape(prefix))).unwrap();
    let found = head
        .find(doc)
        .unwrap_or_else(|| panic!("missing heading: {prefix}"));
    let level = prefix.len() - prefix.trim_start_matches('#').len();
    let next = Regex::new(&format!(r"(?m)^#{{1,{level}}}\s")).unwrap();
    let end = next
        .find(&doc[found.end()..])
        .map_or(doc.len(), |n| found.end() + n.start());
    &doc[found.start()..end]
}

/// Paragraphs, list items and table rows, each one block.
fn blocks(doc: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in doc.lines() {
        let t = line.trim_start();
        let starts = t.starts_with("- ") || t.starts_with("* ") || t.starts_with('|');
        if line.trim().is_empty() || starts {
            if !current.trim().is_empty() {
                out.push(std::mem::take(&mut current));
            }
            current.clear();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out
}

/// A count as the documents write it in prose.
fn word(n: usize) -> String {
    let words = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    words
        .get(n)
        .map_or_else(|| n.to_string(), |w| (*w).to_string())
}

/// The text of a file, or of every source file under a folder.
fn sources(path: &Path) -> Vec<String> {
    if path.is_file() {
        return vec![std::fs::read_to_string(path).unwrap()];
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(path).unwrap().flatten() {
        let p = entry.path();
        if p.is_dir() {
            out.extend(sources(&p));
        } else if p
            .extension()
            .is_some_and(|e| e == "rs" || e == "ts" || e == "tsx")
        {
            out.push(std::fs::read_to_string(&p).unwrap());
        }
    }
    out
}

/// The heading anchors of a Markdown file, as GitHub makes them: lower
/// case, punctuation other than `-` and `_` dropped, spaces to `-`.
fn anchors(markdown: &str) -> BTreeSet<String> {
    markdown
        .lines()
        .filter(|l| l.starts_with('#'))
        .map(|l| {
            l.trim_start_matches('#')
                .trim()
                .to_lowercase()
                .chars()
                .filter_map(|c| match c {
                    ' ' => Some('-'),
                    '-' | '_' => Some(c),
                    c if c.is_alphanumeric() => Some(c),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// Every relative link of a document names a file that exists, and every
/// `#anchor` a heading of that file.
fn links_resolve(relative: &str, text: &str) {
    let link = Regex::new(r"\]\(([^)\s]+)\)").unwrap();
    let dir = root().join(relative).parent().unwrap().to_path_buf();
    for c in link.captures_iter(text) {
        let target = &c[1];
        if target.starts_with("http") || target.starts_with("mailto:") {
            continue;
        }
        let (file, anchor) = target
            .split_once('#')
            .map_or((target, None), |(f, a)| (f, Some(a)));
        let body = if file.is_empty() {
            text.to_string()
        } else {
            let path = dir.join(file);
            assert!(
                path.exists(),
                "{relative} links to {target}, which does not exist"
            );
            if path.is_file() {
                std::fs::read_to_string(&path).unwrap()
            } else {
                String::new()
            }
        };
        if let Some(anchor) = anchor {
            assert!(
                anchors(&body).contains(anchor),
                "{relative} links to {target}: no such heading"
            );
        }
    }
}

/// Trader wording (CLAUDE.md, Vocabulary): sandbox and analyzer mode, never
/// paper or virtual trading, never arm or armed.
fn trader_words(relative: &str, text: &str) {
    let banned = Regex::new(r"(?i)\b(paper|virtual)[ -]trad|\b(re-?)?arm(s|ed|ing)?\b").unwrap();
    if let Some(m) = banned.find(text) {
        panic!("{relative} says {:?}", m.as_str());
    }
    // The web's proper nouns mid-sentence; "Analyzer mode" may start one.
    let web_words = Regex::new(r"Sandbox engine|[a-z,]\s+Analyzer mode").unwrap();
    if let Some(m) = web_words.find(text) {
        panic!("{relative} says {:?}", m.as_str());
    }
}

/// The editor palette's `(type, label)` pairs.
fn palette() -> Vec<(String, String)> {
    let source = repo("src/components/flow/panels/NodePalette.tsx");
    let entry = Regex::new(r"type: '(\w+)',\s*label: '([^']+)'").unwrap();
    entry
        .captures_iter(&source)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

// ------------------------------------------------- the import format

/// IE-18: no terminal or Python path, and the two ways to replace a graph
/// in place are the editor's and the route's.
#[test]
fn the_import_format_offers_no_terminal_path() {
    let doc = repo(IMPORT_FORMAT);
    for web_only in [
        "uv run",
        "--dry-run",
        "python scripts/",
        "Tunable via",
        "Set either to",
    ] {
        assert!(!doc.contains(web_only), "{IMPORT_FORMAT} says {web_only:?}");
    }
    let update = flat(section(&doc, "## 0.1"));
    for said in [
        "**Replace from JSON**",
        "`POST /flow/api/workflows/<id>/replace`",
        "`scripts/update_flow_workflow.py`",
        "The desktop has no Python and does not carry them",
    ] {
        assert!(update.contains(said), "0.1 does not say {said:?}");
    }
    assert!(repo("src-tauri/src/server/routes/flow/mod.rs")
        .contains("\"/flow/api/workflows/{id}/replace\""));
    assert!(repo("src/pages/flow/FlowEditor.tsx").contains("Replace from JSON"));
}

/// IE-19: every web setting is named as the web's, and the desktop's fixed
/// values are the code's.
#[test]
fn the_import_format_states_the_values_the_code_uses() {
    let doc = repo(IMPORT_FORMAT);
    for block in blocks(&doc) {
        if block.contains("FLOW_") {
            assert!(
                block.contains("OpenAlgo web") && block.contains("**Desktop:**"),
                "a FLOW_ setting is not named as the web's:\n{block}"
            );
        }
        if block.contains("`.env`") {
            assert!(block.contains("no `.env`"), "{block}");
        }
    }
    let doc = flat(&doc);
    let offset = INTERVAL_ALIGN_OFFSET_SECONDS;
    let ttl = HISTORY_CACHE_TTL.as_secs();
    let (limit, window) = Bucket::FlowWebhook.limit();
    assert_eq!(window.as_secs(), 60);
    for phrase in [
        format!("fires at HH:MM:{offset:02}"),
        format!("A small offset of {offset} seconds"),
        format!("the offset is fixed at {offset} seconds"),
        format!("reuse an identical history fetch for {ttl} seconds"),
        format!("the {ttl} seconds are fixed"),
        format!("requests at most the latest {MAX_HISTORY_BARS} bars"),
        format!("**Desktop:** fixed at {MAX_HISTORY_BARS}"),
        format!(
            "among the newest {RETENTION_COUNT} runs of its workflow and no more than {RETENTION_DAYS} days old"
        ),
        format!("within {MISFIRE_GRACE_SECS} seconds of it"),
        format!("Capped at {DELAY_MAX_SECONDS} seconds"),
        format!("more than {} minutes away is refused", WAIT_UNTIL_MAX_SECONDS / 60),
        format!("add up to more than {} seconds", INLINE_WAIT_SECONDS as i64),
        format!("symbol every {} seconds", POLL_INTERVAL.as_secs()),
        format!("no answer within {} seconds", CALL_TIMEOUT.as_secs()),
        format!("takes {limit} requests a minute per caller and {limit} a minute per workflow"),
        format!(
            "{} wrong secrets for one workflow within {} minutes lock its webhook for {} minutes",
            word(LOCKOUT_FAILURES),
            LOCKOUT_WINDOW.as_secs() / 60,
            LOCK_DURATION.as_secs() / 60
        ),
        STARTED_IN_BACKGROUND.to_string(),
        NO_ANSWER.to_string(),
        INTERRUPTED.to_string(),
    ] {
        assert!(doc.contains(&phrase), "{IMPORT_FORMAT} does not say {phrase:?}");
    }
    let too_large = "the response is larger than 5 MB, so it was not read";
    assert!(doc.contains(too_large));
    assert!(repo("src-tauri/src/flow/nodes/http/client.rs").contains(too_large));
}

/// IE-20: every file, folder, constant and function section 14 names is in
/// the repository, and each name in the file its bullet cites.
#[test]
fn every_file_and_name_section_14_cites_exists() {
    let doc = repo(IMPORT_FORMAT);
    let enforced = section(&doc, "## 14.");
    let token = Regex::new(r"`([^`]+)`").unwrap();
    let identifier = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap();
    let mut paths = 0;
    for bullet in blocks(enforced)
        .into_iter()
        .filter(|b| b.trim_start().starts_with("- "))
    {
        let mut base: Option<PathBuf> = None;
        let mut files: Vec<PathBuf> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for c in token.captures_iter(&bullet) {
            let t = &c[1];
            if t.starts_with('/') || t.contains(' ') {
                continue;
            }
            if identifier.is_match(t) {
                names.push(t.to_string());
                continue;
            }
            let path = if t.starts_with("src") {
                root().join(t)
            } else {
                base.as_ref()
                    .unwrap_or_else(|| panic!("{t} comes before any path in:\n{bullet}"))
                    .join(t)
            };
            assert!(
                path.exists(),
                "section 14 names {t}, which is not in the repository"
            );
            if t.starts_with("src") {
                base = Some(if t.ends_with('/') {
                    path.clone()
                } else {
                    path.parent().unwrap().to_path_buf()
                });
            }
            files.push(path);
            paths += 1;
        }
        let text: String = files.iter().flat_map(|p| sources(p)).collect();
        for name in names {
            assert!(text.contains(&name), "{name} is not in {files:?}");
        }
    }
    assert!(paths > 30, "only {paths} paths found in section 14");
}

/// IE-21: the alert nodes are the desktop's own bots, with the client's own
/// sentences.
#[test]
fn the_alert_nodes_describe_the_desktops_own_bots() {
    let doc = repo(IMPORT_FORMAT);
    let client = flat(&repo("src-tauri/src/flow/contract/client/app.rs"));
    let navigation = repo("src/config/navigation.ts");
    let telegram = flat(section(&doc, "#### telegramAlert"));
    let whatsapp = flat(section(&doc, "#### whatsappAlert"));
    for (node, menu) in [(&telegram, "Telegram Bot"), (&whatsapp, "WhatsApp Bot")] {
        assert!(
            node.contains(&format!(
                "**Desktop:** the bot is the desktop's own (**{menu}** in the profile menu"
            )),
            "{menu}"
        );
        assert!(navigation.contains(&format!("label: '{menu}'")), "{menu}");
    }
    for sentence in [
        "Telegram bot is stopped",
        "No Telegram account linked for user: ",
        "Telegram notifications are disabled",
        "*Flow Alert*",
    ] {
        assert!(
            telegram.contains(sentence),
            "telegramAlert does not say {sentence:?}"
        );
        assert!(
            client.contains(sentence),
            "the client does not say {sentence:?}"
        );
    }
    for sentence in [
        "WhatsApp is not paired or not connected. Pair the device from /whatsapp.",
        "Invalid WhatsApp phone number: ",
    ] {
        assert!(
            whatsapp.contains(sentence),
            "whatsappAlert does not say {sentence:?}"
        );
        assert!(
            client.contains(sentence),
            "the client does not say {sentence:?}"
        );
    }
    assert!(client.contains("crate::messaging::telegram") && client.contains("messaging.whatsapp"));
}

/// IE-22: the desktop's words, the Historify source kept, and every link
/// and referenced prompt file resolves.
#[test]
fn the_import_format_uses_the_desktops_words_and_its_links_resolve() {
    let doc = repo(IMPORT_FORMAT);
    trader_words(IMPORT_FORMAT, &doc);
    assert!(doc.contains("`\"analyze\"` in analyzer mode"));
    assert!(section(&doc, "#### indicator").contains("`\"db\"` reads Historify"));
    for referenced in [
        "docs/prompt/symbol-format.md",
        "docs/prompt/order-constants.md",
        "docs/prompt/services_documentation.md",
    ] {
        assert!(doc.contains(&format!("`{referenced}`")), "{referenced}");
        assert!(root().join(referenced).is_file(), "{referenced}");
    }
    links_resolve(IMPORT_FORMAT, &doc);
}

/// IE-23: the Import dialog's sentence is told apart from the route's 400,
/// and each is quoted as the code says it.
#[test]
fn the_import_dialogs_sentence_and_the_routes_answer_are_told_apart() {
    let doc = repo(IMPORT_FORMAT);
    let dialog = "Invalid workflow format. Must have name, nodes, and edges.";
    assert!(repo("src/pages/flow/FlowIndex.tsx").contains(dialog));
    let errors = validate_workflow(
        &json!({"nodes": [], "edges": []}),
        ValidateOptions::COMPLETE,
    );
    let no_name = format!("`{}`", errors[0].message);
    assert_eq!(no_name, "`Workflow needs a non-empty name`");
    let contract = flat(section(&doc, "## 0. Output contract"));
    for said in [
        dialog,
        "That sentence is the Import dialog's own check",
        "`\"error\": \"Invalid workflow format\"`",
        no_name.as_str(),
    ] {
        assert!(contract.contains(said), "section 0 does not say {said:?}");
    }
    assert!(flat(section(&doc, "## 13. Pitfalls")).contains(&no_name));
    let importer = flat(section(&doc, "### Importer validation"));
    assert!(importer.contains("\"error\": \"Invalid workflow format\""));
    assert!(repo("src-tauri/src/flow/service/transfer.rs").contains("\"Invalid workflow format\""));
    let service = flat(&repo("src-tauri/src/flow/service/mod.rs"));
    for quoted in [
        "The workflow is not valid JSON (values such as NaN or Infinity are not allowed). Check it and try again.",
        "{\"error\": \"No data provided\"}",
        "Workflow must be a JSON object",
    ] {
        assert!(importer.contains(quoted), "Importer validation does not say {quoted:?}");
        assert!(
            service.contains(quoted) || repo("src-tauri/src/flow/service/transfer.rs").contains(quoted),
            "the import route does not say {quoted:?}"
        );
    }
}

// ------------------------------------------------- chapter 21

/// F-412: the chapter's numbers are the code's.
#[test]
fn chapter_21_states_the_values_the_code_uses() {
    let raw = repo(CHAPTER_21);
    assert!(raw.starts_with("# 21 - Flow Visual Builder\n"));
    let chapter = flat(&raw);
    let offset = INTERVAL_ALIGN_OFFSET_SECONDS;
    let (limit, _) = Bucket::FlowWebhook.limit();
    let within = if MISFIRE_GRACE_SECS == 60 {
        "within a minute of it".to_string()
    } else {
        format!("within {MISFIRE_GRACE_SECS} seconds of it")
    };
    for phrase in [
        format!("There are {} node types", VALID_NODE_TYPES.len()),
        format!("Any of {} indicators", list_supported_indicators().len()),
        format!("Waits up to {} minutes", DELAY_MAX_SECONDS / 60),
        format!("at most {} minutes ahead", WAIT_UNTIL_MAX_SECONDS / 60),
        format!("checked every {} seconds", POLL_INTERVAL.as_secs()),
        format!("at most the latest {MAX_HISTORY_BARS}"),
        format!("newest {RETENTION_COUNT} runs of each workflow, up to {RETENTION_DAYS} days"),
        format!("more than {} seconds in total", INLINE_WAIT_SECONDS as i64),
        format!(
            "{} wrong secrets within {} minutes lock that workflow's webhook for {} minutes",
            word(LOCKOUT_FAILURES),
            LOCKOUT_WINDOW.as_secs() / 60,
            LOCK_DURATION.as_secs() / 60
        ),
        format!("up to {limit} alerts a minute"),
        format!("09:15:{offset:02}, 09:20:{offset:02}"),
        within,
        INTERRUPTED.to_string(),
    ] {
        assert!(
            chapter.contains(&phrase),
            "{CHAPTER_21} does not say {phrase:?}"
        );
    }
    trader_words(CHAPTER_21, &raw);
    links_resolve(CHAPTER_21, &raw);
}

/// F-412, F-413: the chapter's node tables name exactly the palette's 61
/// nodes (the backend's list), in the palette's six tabs, and none of the
/// web chapter's invented ones.
#[test]
fn chapter_21_lists_only_the_nodes_flow_has() {
    let raw = repo(CHAPTER_21);
    let palette = palette();
    let types: BTreeSet<&str> = palette.iter().map(|(t, _)| t.as_str()).collect();
    let backend: BTreeSet<&str> = VALID_NODE_TYPES.iter().copied().collect();
    assert_eq!(
        types, backend,
        "the palette and the backend's node list differ"
    );
    assert_eq!(
        palette.len(),
        VALID_NODE_TYPES.len(),
        "a palette entry is repeated"
    );
    let labels: BTreeSet<&str> = palette.iter().map(|(_, l)| l.as_str()).collect();

    let bold = Regex::new(r"\*\*([^*]+)\*\*").unwrap();
    let mut named = BTreeSet::new();
    for row in section(&raw, "## The nodes")
        .lines()
        .filter(|l| l.starts_with("| **"))
    {
        let first = row.trim_start_matches('|').split('|').next().unwrap();
        named.extend(
            bold.captures_iter(first)
                .map(|c| c.get(1).unwrap().as_str()),
        );
    }
    assert_eq!(named, labels, "chapter 21's nodes are not the palette's");

    let tab = Regex::new(r#"<TabsTrigger value="\w+"[^>]*>\s*([^<]+?)\s*</TabsTrigger>"#).unwrap();
    let source = repo("src/components/flow/panels/NodePalette.tsx");
    let tabs: Vec<String> = tab
        .captures_iter(&source)
        .map(|c| format!("**{}**", &c[1]))
        .collect();
    let (last, rest) = tabs.split_last().unwrap();
    let listed = format!(
        "has {} tabs: {} and {last}",
        word(tabs.len()),
        rest.join(", ")
    );
    assert!(
        flat(&raw).contains(&listed),
        "chapter 21 does not say {listed:?}"
    );

    for invented in [
        "**Timer**",
        "**Manual**",
        "**If/Else**",
        "**If-Else**",
        "**Compare**",
        "**Switch**",
        "**Stop**",
        "Templates",
        "Use Template",
        "History tab",
        "View Execution History",
        "ngrok http",
        "Duration:",
    ] {
        assert!(!raw.contains(invented), "chapter 21 says {invented:?}");
    }
}

/// F-413: the controls the chapter describes are the editor's, by their
/// own labels.
#[test]
fn chapter_21_describes_the_editors_real_controls() {
    let chapter = flat(&repo(CHAPTER_21));
    let index = "src/pages/flow/FlowIndex.tsx";
    let editor = "src/pages/flow/FlowEditor.tsx";
    let config = "src/components/flow/panels/ConfigPanel.tsx";
    for (label, file) in [
        ("Flow Editor", "src/config/navigation.ts"),
        ("New Workflow", index),
        ("Enable Webhook", index),
        ("Authentication Method", index),
        ("Secret in Payload", index),
        ("Secret in URL", index),
        ("Webhook URL (with symbol)", index),
        ("Webhook URL (with secret)", index),
        ("Regenerate URL & Secret", index),
        (
            "Webhook URL not accessible!",
            "src/components/desktop/FlowWebhookReachability.tsx",
        ),
        ("Unsaved", editor),
        ("Run Now", editor),
        ("Export Workflow", editor),
        ("Replace from JSON", editor),
        ("Keyboard Shortcuts", editor),
        ("Workflow active", editor),
        ("Workflow inactive", editor),
        (
            "Execution Log",
            "src/components/flow/panels/ExecutionLogPanel.tsx",
        ),
        (
            "Running in background",
            "src/components/flow/panels/ExecutionLogPanel.tsx",
        ),
        (
            "Leave without saving?",
            "src/components/desktop/FlowLeaveGuard.tsx",
        ),
        ("Only Once", config),
        ("Every Time", config),
        ("Greater Than", config),
        ("Crosses Above", config),
        ("Play Sound", config),
        ("Alert Message", config),
    ] {
        assert!(
            chapter.contains(&format!("**{label}**")),
            "chapter 21 does not name {label:?}"
        );
        assert!(repo(file).contains(label), "{file} has no {label:?}");
    }
}

/// F-413: the webhook section names the addresses the public routes serve
/// and the built-in tunnel reaches, where the trader turns the tunnel on,
/// and the answers decisions 13 and 16 give.
#[test]
fn chapter_21_names_the_tunnel_address_and_the_webhook_answers() {
    let raw = repo(CHAPTER_21);
    let needs = flat(section(&raw, "## What you need"));
    let webhook = flat(section(&raw, "### Webhook"));
    assert!(needs.contains("the built-in ngrok tunnel"));
    let routes = repo("src-tauri/src/server/routes/flow/triggers.rs");
    let tunnel = repo("src-tauri/src/server/tunnel.rs");
    assert!(tunnel.contains(r#"["flow", "webhook", _] | ["flow", "webhook", _, _] => Reach::App"#));
    for path in ["/flow/webhook/{token}", "/flow/webhook/{token}/{symbol}"] {
        assert!(
            routes.contains(&format!("path: \"{path}\"")),
            "no route {path}"
        );
        let shown = path.replace('{', "<").replace('}', ">");
        assert!(
            webhook.contains(&shown),
            "the webhook section does not show {shown:?}"
        );
    }
    assert!(webhook.contains("`https://<your ngrok domain>/flow/webhook/<token>`"));
    // The settings show the tunnel's address while it is online
    // (decision 13), else this computer's, which the dialog flags.
    assert!(repo("src/api/flowDesktop.ts").contains("tunnel?.state === 'online'"));
    let reachability = repo("src/components/desktop/FlowWebhookReachability.tsx");
    assert!(webhook.contains("**Webhook URL not accessible!**"));
    assert!(
        webhook.contains("the built-in ngrok tunnel")
            && reachability.contains("built-in ngrok tunnel")
    );
    let profile = repo("src/pages/Profile.tsx");
    for label in ["Server Configuration", "Enable Ngrok Tunnel"] {
        assert!(webhook.contains(&format!("**{label}**")), "{label}");
        assert!(profile.contains(label), "{label}");
    }
    // Decision 13: a secret always, its refusals; decision 16: signed out,
    // the locked answer; signed in without a broker, the run goes ahead.
    let service = repo("src-tauri/src/flow/service/webhooks.rs");
    for answer in [
        "Webhook is disabled",
        "Workflow is not active",
        "Invalid webhook secret",
    ] {
        assert!(webhook.contains(&format!("\"{answer}\"")), "{answer}");
        assert!(service.contains(&format!("\"{answer}\"")), "{answer}");
    }
    assert!(webhook.contains("Every webhook needs its secret"));
    assert!(webhook.contains(&format!("\"{WEBHOOK_LOCKED}\"")));
    assert!(routes.contains("webhook_while_locked"));
    assert!(
        webhook.contains("Signed in with no broker logged in, an alert still runs the workflow")
    );
}

/// F-413: the Import and Replace sections quote the dialogs' own sentences
/// and the route's own answer.
#[test]
fn chapter_21_import_section_quotes_the_dialogs_and_the_routes_sentences() {
    let import = flat(section(&repo(CHAPTER_21), "## Import and export:"));
    let index = repo("src/pages/flow/FlowIndex.tsx");
    let editor = repo("src/pages/flow/FlowEditor.tsx");
    for (said, source, text) in [
        ("**Import**", &index, "Import"),
        ("**Import Workflow**", &index, "Import Workflow"),
        ("**Upload File**", &index, "Upload File"),
        (
            "\"Invalid JSON format. Please check the workflow data.\"",
            &index,
            "'Invalid JSON format. Please check the workflow data.'",
        ),
        (
            "\"Invalid workflow format. Must have name, nodes, and edges.\"",
            &index,
            "'Invalid workflow format. Must have name, nodes, and edges.'",
        ),
        ("`Workflow \"<name>\" imported`", &index, "`Workflow \"${data.name}\" imported`"),
        ("\"That is not valid JSON.\"", &editor, "'That is not valid JSON.'"),
        (
            "\"Workflow JSON needs a nodes array.\"",
            &editor,
            "'Workflow JSON needs a nodes array.'",
        ),
        ("\"Workflow replaced.\"", &editor, "`Workflow replaced.${notes}`"),
        (
            "\"Workflow replaced. The trigger changed, so the workflow was deactivated. Activate it again once the trigger is valid.\"",
            &editor,
            "The trigger changed, so the workflow was deactivated. Activate it again once the trigger is valid.",
        ),
    ] {
        assert!(import.contains(said), "the import section does not say {said}");
        assert!(source.contains(text), "the dialog does not say {text}");
    }
    // The route's own answer: the first problem the validator names.
    let nodes: Vec<serde_json::Value> = (0..=MAX_NODES)
        .map(|i| json!({"id": format!("n{i}"), "type": "log", "position": {"x": 0, "y": 0}, "data": {}}))
        .collect();
    let errors = validate_workflow(
        &json!({"name": "W", "nodes": nodes, "edges": []}),
        ValidateOptions::COMPLETE,
    );
    let too_many = format!("Workflow has more than {MAX_NODES} nodes");
    assert!(errors.iter().any(|e| e.message == too_many), "{errors:?}");
    assert!(import.contains(&format!("\"{too_many}\"")));
    assert!(repo("src-tauri/src/flow/service/transfer.rs").contains("\"{} (imported)\""));
    assert!(import.contains("`<name> (imported)`"));
}
