# Web sync checkpoints

Next sync starts after f83592a0b230459f3cb0ce845fe16c9c011f2043 (OpenAlgo web main, 2026-11-26 14:40 IST) <!-- confirm after merge: unit late-fixes -->

The desktop carries OpenAlgo web's commits about once a week (the
`web-sync` skill). Each sync covers the web commits after the line above,
records what it did with every one of them, adds its row to the table below
and moves the line to the last web commit it covered. Start the next sync
from that line, not from a date.

A web range is written as git reads it: `a..b` is every web commit after
`a`, up to and including `b`.

| Date | Web range | Desktop release | Record |
| --- | --- | --- | --- |
| 2026-11-26 | `b53816a76..f83592a0b` | 1.0.10 | [sync-2026-11-26.md](sync-2026-11-26.md) <!-- confirm after merge: unit late-fixes --> |
| 2026-11-17 | `a1b9e3250..b53816a76` | 1.0.8 and 1.0.9 | [sync-2026-11-17.md](sync-2026-11-17.md) |
| 2026-11-08 | `a9370edf8..a1b9e3250` | 1.0.6 | message of desktop commit `e8d91272` |
| 2026-10-31 | `8a2fbf1d0..a9370edf8` | 1.0.5 | message of desktop commit `4ec8f076` |
| 2026-10-30 | `96e5927e5..8a2fbf1d0` | 1.0.5 | message of desktop commit `8c19b358` |
| not recorded | `df1073cc2..96e5927e5` | not recorded | No triage covers these commits. The Flow port read the web at `96e5927e5` ([flow-2026-10-30.md](flow-2026-10-30.md)). |
| 2026-10-28 | `79fe4cf18..df1073cc2` | 1.0.4 | message of desktop commit `60b49f97` |
| 2026-10-27 | `f3e7de829..79fe4cf18` | 1.0.3 | message of desktop commit `3b8aeef6` |
| not recorded | up to `f3e7de829` | not recorded | not recorded |

1.0.7 carried no new web range: its WebSocket parity work was checked
against the web at `a1b9e3250`.

The other files in this folder are notes for OpenAlgo web (defects the
desktop fixed and did not copy, accessibility fixes) and the desktop's
recorded departures from a carried web area (`flow-2026-10-30.md`). Each
sync record names the ones it wrote.

## The second checkpoint: the feed corpus pin

`src-tauri/tests/fixtures/feed_corpus/web_oracle.json` holds `web_sha`, the
web commit whose broker streaming adapters computed every `web.json` in the
feed corpus, and `tool_sha256`, the digest of the oracle's own scripts.
CI's `web-oracle` job checks the corpus against that commit. It is narrower
than the line above: it follows only the web's feed adapters and
order-update service, so a sync moves it only when it carries a change
there (`scripts/feed_corpus/web_expected.py --web <private web tree>
--repin`, on a private clone checked out at the new commit), and it may
stay behind the sync line when the later web commits change no adapter.
Pins so far: `a1b9e3250` (1.0.7 and 1.0.8), `ee123273b` (1.0.9),
`3f4c71e0f`, `5749d4126` and `249916b27` (during the 1.0.10 sync), and
`f83592a0b` for 1.0.10's release (`be38b42f` on `lane110/late-fixes`). <!-- confirm after merge: unit late-fixes -->
