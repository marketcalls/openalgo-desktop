# genai 0.7.1, vendored

This is the crates.io source of genai 0.7.1 (the Agent's provider layer),
unchanged apart from one line in `Cargo.toml`: `serde_json` without the
`preserve_order` feature. genai turns that feature on for every crate in the
build, which changes the key order of every `serde_json` map in OpenAlgo
Desktop (broker request bodies, tool outputs, sandbox decisions). 1.0.0 to
1.0.3 were built and tested on sorted maps, and genai itself never relies on
the order. `Cargo.toml.orig` is kept as published. `src-tauri/Cargo.toml`
points `genai` here with `[patch.crates-io]`; drop the patch and this folder
when a genai release no longer turns the feature on.
