# genai 0.7.1, vendored

This is the crates.io source of genai 0.7.1 (the Agent's provider layer),
with two local changes. `src-tauri/Cargo.toml` points `genai` here with
`[patch.crates-io]`; drop the patch and this folder when a genai release
needs neither change.

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
