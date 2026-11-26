# genai 0.7.1, vendored

This is the crates.io source of genai 0.7.1 (the Agent's provider layer),
with four local changes. `src-tauri/Cargo.toml` points `genai` here with
`[patch.crates-io]`; drop the patch and this folder when a genai release
needs none of them.

1. `Cargo.toml`: `serde_json` without the `preserve_order` feature. genai
   turns that feature on for every crate in the build, which changes the key
   order of every `serde_json` map in OpenAlgo Desktop (broker request
   bodies, tool outputs, sandbox decisions). 1.0.0 to 1.0.3 were built and
   tested on sorted maps, and genai itself never relies on the order.
   `Cargo.toml.orig` is kept as published.

2. No effort is read from a model name. `ReasoningEffort::from_model_name`
   (`src/chat/chat_options.rs`) returns no effort and the name unchanged,
   and the Gemini adapter (`src/adapter/adapters/gemini/adapter_impl.rs`)
   no longer infers one from a `-zero`, `-low` ... `-max` suffix. genai sent
   `gpt-5.1-codex-max` as `gpt-5.1-codex` with effort max, a different
   model, and a name ending in `-high` as a high-effort request nobody
   chose. With no effort set, the request now carries the model name exactly
   as configured and no effort or thinking field, as the web (LiteLLM)
   sends it; with an effort set, nothing changes. genai's own tests of the
   suffix no longer hold and are not run here.

3. A custom tool named `web_search` stays a custom tool. The Anthropic
   (`src/adapter/adapters/anthropic/adapter_shared.rs`) and OpenAI Responses
   (`src/adapter/adapters/openai_resp/adapter_impl.rs`) adapters turned any
   tool with that name into the provider's own server-side search
   (`web_search_20250305`, `{"type": "web_search"}`) and dropped its schema,
   so the Agent's `web_search` (the web's tool name) ran as Anthropic's or
   OpenAI's paid search, outside the trader's chosen provider and the
   web-search boundary, and on Anthropic the reply then failed to parse
   (`server_tool_use`). Only `ToolName::WebSearch`, genai's built-in tool, is
   the provider's search now. Pinned by
   `agent::run::tests::a_tool_named_web_search_is_sent_as_ours`.

4. OpenRouter carries a message's cache mark to Claude. The Agent marks a
   Claude model's system prompt and last message for prompt caching, as
   the web's LiteLLM does (web d941bb3b1,
   `agent::providers::prompt_cache`). On OpenRouter the OpenAI-format
   adapter (`src/adapter/adapters/openai/adapter_shared.rs`) dropped the
   mark, since genai's own cache policy there covers only OpenAI's
   explicit-cache models. A marked message on OpenRouter now carries
   Anthropic's `cache_control` on its last content part, as LiteLLM sends
   it: the system message and the trader's message as text parts, the last
   tool result of a tool message as a text part; an unmarked message, and
   every request to any other provider, is unchanged. The mapping is the
   Anthropic adapter's `cache_control_to_json`, made visible to the other
   adapters (`src/adapter/adapters/anthropic/adapter_shared.rs`). Pinned
   by `agent::chat::prompt_cache_tests` on the request body OpenRouter
   receives.
