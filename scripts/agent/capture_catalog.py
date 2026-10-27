"""Record the Agent's model catalogue snapshot from the web's installed LiteLLM.

The web reads its provider grid and model metadata from LiteLLM at runtime
(`services/agent/catalog.py`): `LITELLM_CHAT_PROVIDERS`, `models_by_provider`
and `model_cost`, the price and context table LiteLLM ships with its wheel and
refreshes from GitHub when it is imported. The desktop has no LiteLLM, so it
bundles the same data, recorded here, and refreshes `model_cost` from the same
URL once at startup (`agent::catalog::refresh`).

What is written (`src-tauri/src/agent/catalog/snapshot.json`):

- `litellm`: the version the web pins, so a drift is visible in review.
- `chat_providers`: LiteLLM's chat provider ids in its own order, kept only
  for the providers the desktop can reach (decision 9 of the 1.0.4 plan:
  Bedrock, SageMaker and Vertex are hidden).
- `provider_ids`: every provider id LiteLLM names anywhere, used only to tell
  whether a slash in a model id is a provider prefix (`_read_provider_ids`).
- `models`: per offered provider, the names LiteLLM lists for it
  (`models_by_provider`, sorted: LiteLLM keeps sets) and `sources`, the
  `litellm_provider` values whose `model_cost` keys LiteLLM folds into that
  list when it loads a newer price table (`add_known_models`), measured by
  feeding it one probe entry per value.
- `backup_model_count`: how many models LiteLLM's bundled table holds; a
  fetched table under half of it is refused as LiteLLM refuses it.
- `entries`: the `model_cost` entries of those providers (and of the
  providers that feed them), reduced to the fields the catalogue reads.

Usage, from the desktop repository root, with the web's own interpreter
(read-only use of its virtual environment):

    /path/to/openalgo/.venv/bin/python -I scripts/agent/capture_catalog.py

Regenerate only from the web; never edit the snapshot by hand.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

# The wheel's own copy, never a network fetch: the snapshot is the bundled
# table, and the desktop's startup refresh is what brings it up to date.
os.environ["LITELLM_LOCAL_MODEL_COST_MAP"] = "True"

import litellm  # noqa: E402

OUT = Path("src-tauri/src/agent/catalog/snapshot.json")

# The providers the desktop offers (1.0.4 decision 9): what genai reaches.
OFFERED = (
    "openai",
    "anthropic",
    "gemini",
    "ollama",
    "cohere",
    "groq",
    "deepseek",
    "xai",
    "mistral",
    "openrouter",
    "together_ai",
    "perplexity",
    "fireworks_ai",
    "cerebras",
    "lm_studio",
    "vllm",
    "llamafile",
    "custom",
    "custom_openai",
    "openai_like",
    "chatgpt",
)

# The fields `services/agent/catalog.py` and `providers.py` read from an entry.
FIELDS = (
    "litellm_provider",
    "mode",
    "max_input_tokens",
    "max_tokens",
    "max_output_tokens",
    "input_cost_per_token",
    "output_cost_per_token",
    "cache_read_input_token_cost",
    "supports_function_calling",
    "supports_vision",
    "supports_reasoning",
)

PLACEHOLDERS = {"sample_spec", "fallback_generalizations"}


def _id(p: object) -> str:
    return str(getattr(p, "value", p)).strip()


def _sources(provider_values: set[str]) -> dict[str, list[str]]:
    """Which `litellm_provider` values LiteLLM folds into each offered list."""
    fed: dict[str, set[str]] = {p: set() for p in OFFERED}
    for value in sorted(provider_values):
        probe = f"zz-desktop-probe-{value}"
        before = {p: set(litellm.models_by_provider.get(p, ())) for p in OFFERED}
        litellm.add_known_models({probe: {"litellm_provider": value, "mode": "chat"}})
        for p in OFFERED:
            if probe in set(litellm.models_by_provider.get(p, ())) - before[p]:
                fed[p].add(value)
    return {p: sorted(v) for p, v in fed.items()}


def main() -> int:
    cost = {k: v for k, v in litellm.model_cost.items() if isinstance(v, dict)}
    chat = [_id(p) for p in litellm.LITELLM_CHAT_PROVIDERS]
    missing = [p for p in OFFERED if p not in chat]
    if missing:
        print(f"not chat providers in this LiteLLM: {missing}", file=sys.stderr)
        return 1
    ids: set[str] = set()
    for source in (litellm.models_by_provider, chat, litellm.provider_list):
        ids.update(_id(p) for p in source)
    ids.update(_id(e.get("litellm_provider", "")) for e in cost.values())
    ids.discard("")

    listed = {p: sorted({str(n).strip() for n in litellm.models_by_provider.get(p, ())}) for p in OFFERED}
    values = {_id(e.get("litellm_provider", "")) for e in cost.values()} - {""}
    sources = _sources(values)

    keep_providers = set(OFFERED)
    for fed in sources.values():
        keep_providers.update(fed)
    wanted_names = {n for names in listed.values() for n in names}
    entries = {}
    for key, entry in cost.items():
        name = key.strip()
        if not name or name in PLACEHOLDERS or name.startswith("zz-desktop-probe-"):
            continue
        if _id(entry.get("litellm_provider", "")) in keep_providers or name in wanted_names:
            entries[name] = {f: entry[f] for f in FIELDS if f in entry}

    snapshot = {
        "_comment": "Generated by scripts/agent/capture_catalog.py from the web's LiteLLM. Do not edit.",
        "litellm": getattr(litellm, "__version__", None) or _installed_version(),
        "backup_model_count": _backup_count(),
        "chat_providers": [p for p in dict.fromkeys(chat) if p in keep_providers and p in OFFERED],
        "provider_ids": sorted(ids),
        "models": {p: {"listed": listed[p], "sources": sources[p]} for p in OFFERED},
        "entries": dict(sorted(entries.items())),
    }
    OUT.write_text(json.dumps(snapshot, indent=0, sort_keys=False, ensure_ascii=False) + "\n")
    print(f"wrote {OUT}: {len(entries)} entries, {len(snapshot['chat_providers'])} providers")
    return 0


def _backup_count() -> int:
    backup = Path(litellm.__file__).parent / "model_prices_and_context_window_backup.json"
    table = json.loads(backup.read_text())
    return sum(1 for key in table if key not in PLACEHOLDERS)


def _installed_version() -> str:
    from importlib.metadata import version

    return version("litellm")


if __name__ == "__main__":
    sys.exit(main())
