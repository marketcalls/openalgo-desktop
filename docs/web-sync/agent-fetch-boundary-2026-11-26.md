# The Agent's web fetch and search boundaries, three fixes to carry to OpenAlgo web (2026-11-26)

The desktop's 1.0.10 ports the web's `web_fetch` (46af9591f, a2ae9f1fa) and
the follow-up words of 5e19ce407. A security review of the port found two
places where the web's boundary lets the model reach further than the
trader's own words, and a review of the search chain found that a
DuckDuckGo choice no longer keeps searches on DuckDuckGo; the desktop does
not copy any of the three. Line numbers are from the web at 3f4c71e0f,
`services/agent/tools/websearch.py`.

## 1. A typed address is found anywhere in the message

`check_fetch_url` (line 635) accepts an address as the operator's when
`re.search(re.escape(form), operator_message, re.IGNORECASE)` (line 703)
finds it anywhere in the message. The match is not anchored, so:

- the operator writes `notevil.com/report` and the model asks for
  `evil.com/report`: the search finds `evil.com/report` inside the word and
  the page is fetched from a host the operator never wrote;
- the same for `x.evil.com/report`, and for `evil.com/repo` inside
  `evil.com/report` (a path cut inside a segment).

Since the URL sent is the matched text, the model can only send characters
the operator wrote, but it chooses which host they reach: any host name
that is the tail of one the operator typed. A page on that host is the
model's choice.

The desktop's rule (`src-tauri/src/agent/websearch/fetch_url.rs`,
`typed_by_operator`): the match counts only where one of the operator's
links starts (the start of the text, after white space or an opening
bracket or quote, or right after `scheme://`) and only when the link ends
there, ends in closing punctuation, or goes on at a path, query or
fragment boundary (`/`, `?`, `#`, and `&` once the address has a query).
So `evil.com/x` is refused in `notevil.com/x`, `x.evil.com/x`,
`evil.com/xyz`, `evil.com/x.html` and `evil.com:8443/x`, and still
accepted in `(https://www.evil.com/x)`, `evil.com/x?` and `evil.com/x/y`
(tests in `src-tauri/src/agent/websearch/tests/fetch_url.rs`). The desktop
also refuses an address with white space, a control character or an
invisible format character in it, the one requested and the one sent
alike: `urlsplit` drops a tab, a line break and a leading control
character, so a URL a search returned with one inside matches the model's
clean spelling through `url_key`, and the web then sends the search's text
as it came.

A Python form of the same rule:

```python
_OPENS = set("(<\"'[`")
_CLOSES = set(".,;:!?)]}>\"'`")

def _typed(form: str, message: str) -> str | None:
    for start in range(len(message)):
        before = message[:start]
        if before and not (before[-1].isspace() or before[-1] in _OPENS
                           or before.endswith("://")):
            continue
        link = re.split(r"\s", message[start:], maxsplit=1)[0]
        if link[:len(form)].lower() != form.lower():
            continue
        after = link[len(form):]
        if (not after or after[0] in "/?#" or (after[0] == "&" and "?" in form)
                or all(c in _CLOSES for c in after)):
            return link[:len(form)]
    return None
```

## 2. Earlier messages widen how many words one query carries

`constrain_query` (line 499) builds the haystack from this turn's message
and the redacted earlier messages, and keeps every model token found in
it. A follow-up such as "get the marketcap of it" needs one or two words
from an earlier message (the company); a long conversation instead lets
one query carry dozens of words the model picks from the history, each a
choice the model makes.

The desktop (`src-tauri/src/agent/websearch/taint.rs`,
`constrain_with_history`) keeps the follow-up feature: a word only the
earlier messages hold goes out at most once, and at most four such words
in one query, so a query holds at most this turn's words plus four. The
web's own follow-up cases (`test_a_follow_up_takes_its_company_from_an_earlier_message`
and its neighbours in `test/test_agent_webfetch.py`) pass unchanged.

## 3. A DuckDuckGo choice sends every search to a paid provider first

`_link_outcome` (line 2299) builds its chain from the configured provider
when it is a keyed one, then every other keyed provider that holds a key
(lines 2345-2354), and asks DuckDuckGo only after all of them failed. With
DuckDuckGo configured the first step adds nothing, so the chain is every
keyed provider that holds a key, and each search goes to Tavily, Firecrawl
or TinyFish before DuckDuckGo. Nothing failed, so no notice says so, and the
"configured provider has no key" notice (line 2356) fires only for a keyed
provider.

The everyday case: the trader keeps the keyless default and adds a
Firecrawl key, as the panel asks them to so the agent can open pages
("web_fetch needs a Firecrawl or TinyFish key. Add one below"). From then
on every web search, and the Firecrawl key, go to Firecrawl's paid search,
while the panel still says "Link searches run on DuckDuckGo, which needs no
key and works out of the box" and the provider description says no key
leaves the machine. Before 5e19ce407 (`_link_outcome` at 46af9591f) a
DuckDuckGo configuration searched DuckDuckGo only. The commit message
speaks of a failed search falling through, and the comment above the chain
says "the configured provider first", so this looks unintended. No web
test configures DuckDuckGo with a key stored.

The desktop (`src-tauri/src/agent/tools/websearch/search.rs`,
`link_outcome` and `duckduckgo_first`): with DuckDuckGo configured,
DuckDuckGo is asked first; only when it fails do the keyed providers that
hold a key get the search, in their order, each step said in a notice
("DuckDuckGo did not answer. It rate limits under load. Firecrawl was tried
instead.") and each failed request counted against the turn, as for the
rest of the chain. Tests in `src-tauri/src/agent/websearch/tests/chain.rs`
(`duckduckgo_configured_*`). A Python form of the same change:

```python
if provider == PROVIDER_DUCKDUCKGO:
    outcome = _duckduckgo_search(text, count)
    if outcome.ok or outcome.busy or not chain:
        return outcome, (_busy_payload(outcome, notices) if outcome.busy else None)
    self.budget.record_attempt()
    notices.append(f"{outcome.error} {PROVIDER_LABELS[chain[0][0]]} was tried instead.")
    # then walk `chain` as below, with nothing after the last provider:
    # its failure is the answer.
```
