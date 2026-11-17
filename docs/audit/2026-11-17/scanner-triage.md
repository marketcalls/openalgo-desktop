# Scanner triage 2026-11-17

Base: `3527ecbb` (the 1.0.9 release commit on master). Trigger: the daily
Security Scan of 2026-11-17 (run 44063594615, scheduled) failed its
cargo-deny job; the Security Scan of the push itself (run 44056866041) had
passed an hour earlier, before the advisory database carried the notices
below. Reviewer: Claude (agent) for the maintainer.

## Summary

Two RustSec notices, both `informational = "unmaintained"`, both from the
same upstream announcement (chronotope/chrono#1768), both with no patched
version. Neither is a vulnerability. Both are **accepted for 1.0.9** in
`.github/deny.toml` with a review date of 2027-01-31.

| ID | Crate | Kind | Patched | How the app depends on it | Disposition |
| --- | --- | --- | --- | --- | --- |
| RUSTSEC-2026-0543 | chrono 0.4 | unmaintained | none | direct; also arrow, duckdb, rmcp/schemars, whatsapp-rust and others | accepted until a move to `jiff` or `time` |
| RUSTSEC-2026-0555 | chrono-tz 0.10 | unmaintained | none | direct (`Asia::Kolkata` in 107 files, `UTC` in the master contract cut-off); also arrow, rmcp/schemars, whatsapp-rust | accepted; the direct dependency can go by moving those uses to the fixed IST offset in `crate::clock` (IST has no daylight saving) |

## Why they fail the build

`.github/deny.toml` sets `unmaintained = "workspace"`: an unmaintained
notice fails only for a crate the app depends on directly. Both crates are
direct dependencies of `openalgo-desktop`, so both fail; their transitive
uses alone would only be reported.

## Other scanners

| Tool | Result on `3527ecbb` |
| --- | --- |
| cargo-audit (RustSec) | pass (informational notices are warnings there) |
| trivy fs | pass (no GHSA or CVE for either notice) |
| npm audit, production | pass |
| gitleaks, full history | pass |

So nothing is added to `.trivyignore`, and there is no Dependabot alert to
dismiss.

## Checked

```
cargo deny --manifest-path src-tauri/Cargo.toml --config .github/deny.toml --locked check advisories
```

Before the entries: `advisories FAILED` with exactly these two IDs. After:
`advisories ok` (cargo-deny 0.21.0, advisory database of 2026-11-17).

## Follow-up (after 1.0.9)

- Replace the direct `chrono_tz::Asia::Kolkata` uses with the fixed IST
  offset already in `crate::clock` (`IST_OFFSET_SECS`), keep `UTC` from
  chrono, and drop the `chrono-tz` dependency; that turns RUSTSEC-2026-0555
  into a transitive notice that no longer fails the build. A refactor
  commit of its own, with the sandbox clock and the 03:00 boundary tests
  as the guard.
- Decide on chrono itself (`jiff` or `time`) before the review date.
