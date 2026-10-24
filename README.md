# OpenAlgo Desktop downloads

How many times the files of each [OpenAlgo Desktop](https://github.com/marketcalls/openalgo-desktop) release were downloaded from GitHub, read on 2026-10-24 (UTC). Updated every 6 hours.

## Totals

| | Downloads |
| --- | ---: |
| All time | 276 |
| Last 30 days | 276 |
| Windows | 147 |
| macOS | 84 |
| Linux | 45 |

Update checks, which are not downloads and are in no total: 607.

## By release

| Release | Published | Windows | macOS installer | macOS update | Linux AppImage | Linux .deb | Total | Update checks |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| v1.0.2 | 2026-10-24 | 11 | 4 | 5 | 4 | 4 | 28 | 71 |
| v1.0.1 | 2026-10-23 | 44 | 13 | 15 | 10 | 8 | 90 | 257 |
| v1.0.0 | 2026-10-22 | 92 | 32 | 15 | 11 | 8 | 158 | 279 |

## By file

| Release | File | Downloaded by | Downloads |
| --- | --- | --- | ---: |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_aarch64.AppImage` | a person or the in-app update | 2 |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_aarch64.app.tar.gz` | the in-app update | 3 |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_aarch64.dmg` | a person (release page) | 2 |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_amd64.AppImage` | a person or the in-app update | 2 |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_amd64.deb` | a person (release page) | 2 |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_arm64.deb` | a person (release page) | 2 |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_x64-setup.exe` | a person or the in-app update | 11 |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_x64.app.tar.gz` | the in-app update | 2 |
| v1.0.2 | `OpenAlgo.Desktop_1.0.2_x64.dmg` | a person (release page) | 2 |
| v1.0.2 | `latest.json` | update checks (not a download) | 71 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_aarch64.AppImage` | a person or the in-app update | 4 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_aarch64.app.tar.gz` | the in-app update | 9 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_aarch64.dmg` | a person (release page) | 9 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_amd64.AppImage` | a person or the in-app update | 6 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_amd64.deb` | a person (release page) | 5 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_arm64.deb` | a person (release page) | 3 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_x64-setup.exe` | a person or the in-app update | 44 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_x64.app.tar.gz` | the in-app update | 6 |
| v1.0.1 | `OpenAlgo.Desktop_1.0.1_x64.dmg` | a person (release page) | 4 |
| v1.0.1 | `latest.json` | update checks (not a download) | 257 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_aarch64.AppImage` | a person or the in-app update | 6 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_aarch64.app.tar.gz` | the in-app update | 10 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_aarch64.dmg` | a person (release page) | 20 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_amd64.AppImage` | a person or the in-app update | 5 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_amd64.deb` | a person (release page) | 5 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_arm64.deb` | a person (release page) | 3 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_x64-setup.exe` | a person or the in-app update | 92 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_x64.app.tar.gz` | the in-app update | 5 |
| v1.0.0 | `OpenAlgo.Desktop_1.0.0_x64.dmg` | a person (release page) | 12 |
| v1.0.0 | `latest.json` | update checks (not a download) | 279 |

## What the counts mean

- These are GitHub's counts of release file downloads. OpenAlgo Desktop sends no usage data, so nothing here comes from the app itself.
- A download is not a trader: one person may download a file more than once, and a downloaded file may never be installed.
- Windows: the same setup file installs OpenAlgo Desktop and updates it from inside the app, so Windows downloads include in-app updates.
- macOS: the `.dmg` is downloaded by a person to install; the `.app.tar.gz` is downloaded by the in-app update.
- Linux: the AppImage is downloaded to install and by the in-app update, so its count includes updates. A `.deb` is always downloaded by a person: a `.deb` install is told about a new version and links to the release page.
- Last 30 days: the count now minus the count recorded 30 days ago (until that much history exists, the earliest count recorded); every download of a release published in the last 30 days is in it.
- Update checks: each start of an installed copy, and each Check for Updates, reads `latest.json` from the newest release. They are counted apart and are in no total.
- Not counted: draft releases, signatures (`.sig`) and `SHA256SUMS.txt`.
- `downloads.csv` keeps one row per day, release and file with that day's cumulative count; `badges/` holds the badges the project README shows.
