# In-app updates: how a release reaches installed copies

OpenAlgo Desktop checks for a newer version each time it starts, and when the
trader chooses **Check for Updates** in the profile menu. It only tells the
trader: nothing is downloaded until they choose **Update**. Then it downloads
the update, verifies its signature, installs it and restarts. This page is
for the maintainer: what the release workflow produces, what makes an update
visible, and how the signing key is handled.

## The chain, end to end

1. A `v*` tag on a green commit starts `.github/workflows/release.yml`. Its
   gate (`verify`) also fails when the `TAURI_SIGNING_PRIVATE_KEY` secret is
   not set; only whether it is set is read, never its value.
2. Each platform build adds `src-tauri/updater-artifacts.conf.json`
   (`bundle.createUpdaterArtifacts: true`) to the Tauri build. Besides the
   installers it then produces the platform's **update bundle** and a
   **`.sig`** next to it, signed with the private key:

   | Platform | Update bundle on the release | Installs as |
   | --- | --- | --- |
   | macOS Apple Silicon | `OpenAlgo.Desktop_<v>_aarch64.app.tar.gz` | replaces the `.app` |
   | macOS Intel | `OpenAlgo.Desktop_<v>_x64.app.tar.gz` | replaces the `.app` |
   | Windows x64 | `OpenAlgo.Desktop_<v>_x64-setup.exe` | NSIS installer, passive mode, per-user |
   | Linux x64 | `OpenAlgo.Desktop_<v>_amd64.AppImage` | replaces the AppImage file |
   | Linux arm64 | `OpenAlgo.Desktop_<v>_aarch64.AppImage` | replaces the AppImage file |

   A `.deb` is never updated in place: the app tells a `.deb` install that a
   new version exists and links to the release page.

   The committed `tauri.conf.json` does not ask for update bundles, because
   CI and local builds have no private key and the Tauri build fails without
   one. Only the release workflow passes the extra config file.
3. `tauri-action` uploads the installers, the update bundles and their
   `.sig` files to the **draft** release. It does not write `latest.json`
   (parallel platform jobs rewriting one file would race).
4. The `checksums` job downloads every asset of the draft and runs
   `scripts/ci/updater_manifest.py`, which writes **`latest.json`**:

   ```json
   {
     "version": "1.2.0",
     "pub_date": "2026-10-15T04:30:00Z",
     "notes": "<the CHANGELOG.md section of 1.2.0>",
     "platforms": {
       "darwin-aarch64":     {"url": "https://github.com/marketcalls/openalgo-desktop/releases/download/v1.2.0/OpenAlgo.Desktop_1.2.0_aarch64.app.tar.gz", "signature": "<contents of the .sig>"},
       "darwin-aarch64-app": {"url": "...same...", "signature": "...same..."},
       "windows-x86_64":      {"...": "..."},
       "windows-x86_64-nsis": {"...": "..."},
       "linux-x86_64":          {"...": "the AppImage"},
       "linux-x86_64-appimage": {"...": "the AppImage"}
     }
   }
   ```

   Every platform appears under `{os}-{arch}` and `{os}-{arch}-{installer}`,
   the two keys the update plugin looks up. The job fails, and nothing is
   attached, when:

   - a platform's update bundle or its `.sig` is missing, or there are two;
   - a `.sig` is not a Tauri signature, or names another version than the
     tag (the app enforces `requireSignedVersion`, so such an update would be
     refused on every machine);
   - a signature does not verify, with `minisign`, against the public key in
     `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`). This is the
     check that the GitHub secret is the private half of the key every
     installed copy trusts.

   The release notes are the `## <version>` section of `CHANGELOG.md` (the
   gate already requires it to be the newest section).
5. `latest.json` is attached to the draft, and `SHA256SUMS.txt` is computed
   over every asset, `latest.json` and the `.sig` files included.

## Publishing the draft is what makes the update visible

Installed copies ask one fixed address, built into the app:

```
https://github.com/marketcalls/openalgo-desktop/releases/latest/download/latest.json
```

GitHub answers it from the **latest published release**: never a draft, and
never a release marked as a pre-release. So:

- While the release is a draft, no installed copy sees it. Review the draft,
  download and try the installers and an update bundle if you like, then
  publish it by hand. From that moment every copy that starts, or whose
  trader chooses Check for Updates, is offered the new version.
- A pre-release (a tag with a suffix such as `v1.3.0-beta.1`, created as a
  pre-release by the workflow) is never served at that address. Beta testers
  install it by hand; they are offered the next final release in-app. A
  release build is never offered a pre-release even if one were published as
  the latest release: the app compares versions by semantic-version
  precedence and keeps release builds on releases.
- To withdraw an update that turned out bad, mark that release as a
  pre-release or delete its `latest.json` asset: the address then answers
  from the previous published release (an older version, which no copy
  installs over a newer one), or not at all. Then publish a fixed release.
- Until a release with `latest.json` is published (and while the newest
  release is one published before in-app updates existed), the address
  answers 404. Copies that check then report that they are up to date and
  log one info line, not an error; nothing else happens. Any other failing
  answer is still reported as a failed check.

## The signing key

- The public key is in `src-tauri/tauri.conf.json` (`plugins.updater.pubkey`)
  and is pinned by a Rust test. Every installed copy trusts exactly this key.
- The private key and its password are the GitHub secrets
  `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, with
  a backup kept by the maintainer. They are never printed, logged or
  committed; the workflow exports them only to the build step, and only when
  they are set.
- **If the private key is lost**, no further update can reach installed
  copies through the app: they only trust the old key. Traders must then
  install the next version by hand, and that version carries the new key.
- **To rotate the key** deliberately, publish one release signed with the
  old key that carries the new public key, then switch the secrets. Copies
  that skip that release update by hand.

## What the app checks before it installs

The update plugin verifies the download's minisign signature against the
built-in key, and that the version written into the signature is the one
`latest.json` announced, so a tampered answer cannot pair a new version
number with an older signed release. Before restarting, the app names every
Live strategy, OpenScript run, scalping stop and open order a restart would
interrupt and asks the trader to confirm; sandbox-only activity gets a
lighter warning. It asks again if something new started during the
download. See `docs/user/updating.md` for what the trader sees.
