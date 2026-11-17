# Custom chart indicators

Write your own indicators for the `/trading` charting terminal.

Put a `.js` file in the `indicators` folder of your OpenAlgo Desktop data
folder, open the indicator picker, and it is there beside the built-in
indicators. No build step, no Node.js, no restart of OpenAlgo.

OpenAlgo Desktop uses the same charting library as OpenAlgo web
(`openalgo-charts`, at the web's version, pinned in `package.json`) and
the same loader (`src/lib/trading/customIndicators.ts`, the web's file
unchanged), so an indicator written for the web works here, and the other
way round. What differs is where the file goes; see
[Differences from OpenAlgo web](#differences-from-openalgo-web).

## Only add indicators you have read

An indicator file is **not sandboxed**. It is JavaScript that the app's page
imports and runs with your signed-in session, so it can call `/api/v1`, place
orders, read your positions and send data anywhere. It is closer to pasting a
script into a browser console than to a scripting language.

Treat an indicator copied from a forum, a chat group or a stranger exactly as
you would treat any program you are about to run on your trading computer:
read it first, or do not add it. Nothing stands between a file in this folder
and your live broker session. If you want a study you can share safely, write
it in [OpenScript](openscript.md) instead.

- [Where the files go](#where-the-files-go)
- [How it works](#how-it-works)
- [Your first indicator](#your-first-indicator)
- [The descriptor](#the-descriptor)
- [The full reference](#the-full-reference)
- [Troubleshooting](#troubleshooting)
- [Differences from OpenAlgo web](#differences-from-openalgo-web)

## Where the files go

| System | Folder |
| --- | --- |
| Windows | `%APPDATA%\com.openalgo.desktop\indicators` |
| macOS | `~/Library/Application Support/com.openalgo.desktop/indicators` |
| Linux (x64 or ARM64) | `~/.local/share/com.openalgo.desktop/indicators` |

OpenAlgo does not create the folder; make it the first time. The data folder
exists once OpenAlgo has started once.

- A file name must end in `.js`, start with a letter or a digit, and hold
  only letters, digits, dots, dashes and underscores (up to 64 characters
  before `.js`).
- A file over 4 MB is refused. Links (symlinks) are not followed, so a file
  must really be in the folder.
- The folder is copied with the rest of the data folder when you back it up
  (README, [Where your data lives, and backups](../README.md#where-your-data-lives-and-backups))
  and is kept by updates.

## How it works

The chart loads your files at runtime, from OpenAlgo's own server:

```
/trading  ->  terminal.ts loadIndicators()
                 1. import 'openalgo-charts/indicators'   (the built-in indicators)
                 2. GET /custom-indicators/index.json     (your files, with their times)
                 3. import each /custom-indicators/<file>.js?v=<time>
                 4. call its default export with the charting API
```

- Both routes answer only your signed-in session.
- Your modules register **after** the built-ins, so an indicator that reuses a
  built-in id replaces it.
- The picker reads the folder again every time it opens, so a new file
  appears at once, and an edited file is imported again because its address
  carries its modification time.
- A chart layout you saved with a custom indicator on it loads that
  indicator again when you open the layout.
- These are plain `.js` files: nothing compiles them, so write plain
  JavaScript with no types, no JSX and no `import` lines. Everything you need
  is handed to your function.

## Your first indicator

Save this as `my_sma.js` in the folder above:

```js
export default function ({ registerIndicator, sourceValues }) {
  registerIndicator({
    id: 'my-sma',
    name: 'My SMA',
    category: 'Custom',
    placement: 'onchart',
    inputs: [
      { key: 'length', type: 'number', label: 'Length', default: 20, min: 1, step: 1 },
      { key: 'source', type: 'source', label: 'Source', default: 'close' },
    ],
    plots: [
      { key: 'ma', type: 'line', title: 'MA', style: { color: '#4f8cff', lineWidth: 2 } },
    ],
    calc(bars, settings) {
      const src = sourceValues(bars, settings.source)
      const n = Math.max(1, Math.floor(Number(settings.length) || 20))
      const ma = new Array(bars.length).fill(null)
      let sum = 0
      for (let i = 0; i < src.length; i++) {
        sum += src[i]
        if (i >= n) sum -= src[i - n]
        if (i >= n - 1) ma[i] = sum / n
      }
      return { ma }
    },
  })
}
```

Open the indicator picker on `/trading`: **My SMA** is under **Library**, in a
**Custom** category (the descriptor's `category`). **Yours**, **My scripts**
in the picker lists your OpenScript studies, not these files.

Two rules that are easy to miss:

- The module **default-exports a function**, called once with the charting
  API. A file that exports nothing, or exports an object, is reported as an
  error.
- `calc` returns **one array per plot key, the same length as `bars`**, with
  `null` for warmup. `null`, `undefined` and `NaN` are gaps: lines break across
  them and autoscale skips them. `0` is not a gap, and a `0` in a warmup slot
  drags the whole pane's scale down.

## The descriptor

```js
registerIndicator({
  id: 'my-thing',        // registry key, unique
  name: 'My Thing',      // shown in the picker and the legend
  category: 'Custom',    // groups it in the picker
  placement: 'onchart',  // 'onchart' overlays price, 'pane' gets its own pane
  inputs: [...],
  plots: [...],
  calc(bars, settings, store) { ... },
})
```

A bar is `{ time, open, high, low, close, volume }`, where `time` is **UTC
seconds**, not milliseconds and not local time.

**Inputs** become fields in the settings dialog; `key` is what `calc` reads
from `settings`, and every input needs a `default`.

| `type` | Renders as | Extra fields |
| --- | --- | --- |
| `number` | Number box with steppers | `min`, `max`, `step` |
| `boolean` | Tick box | |
| `color` | Colour swatch | |
| `text` | Text box | |
| `select` | Dropdown | `options: [{ label, value }]` |
| `source` | Price-source dropdown | `open`, `high`, `low`, `close`, `hl2`, `hlc3`, `ohlc4`, `volume` |
| `interval` | Timeframe dropdown | `''` means the chart's own interval |
| `time` | Date and time text | `YYYY-MM-DD HH:MM` in the chart's time zone |

Those eight are all `/trading` accepts; a descriptor with any other input
type is refused with a message. You do not need colour or line-width inputs:
the dialog's **Style** tab already has them for every plot. A cleared field
arrives as `''`, so read settings defensively (`Number(settings.length) ||
20`).

**Plots** name what `calc` returns. `type` is one of `line`, `line-markers`,
`step`, `area`, `histogram` or `column`; keys must be unique and must match
the keys `calc` returns.

**Optional fields** include `levels(settings)` (reference lines such as RSI
70/30), `range(settings)` (a fixed pane range), `markers(ctx)` (signal labels
on bars), `fills` (shading between two plots), `table(ctx)` (a summary grid in
a pane corner), `calcTail(...)` (incremental recompute on live ticks),
`alerts` and `attach(ctx)` (indicators that fetch their own data).

## The full reference

The repository's `chart-indicator` skill is kept in step with the pinned
library version and is the complete reference for writing indicators here:

| Read | For |
| --- | --- |
| `.claude/skills/chart-indicator/reference/contract.md` | Every descriptor field: inputs, plots, fills, markers, levels, range, table, `calcTail`, alerts, drawings, `attach` and external data |
| `.claude/skills/chart-indicator/reference/api.md` | Every function the charting API hands you, with what arrived in which version |
| `.claude/skills/chart-indicator/reference/cookbook.md` | Worked patterns (sessions, higher timeframes, shading, signals) |
| `.claude/skills/chart-indicator/reference/pitfalls.md` | The mistakes that draw nothing without an error, and the built-in ids a custom one would replace |
| `.claude/skills/chart-indicator/examples/` | Thirteen indicators that pass the validator, from a z-score to a session VWAP |

OpenAlgo web's `docs/custom-indicators.md` has a longer tutorial with the same
API (markers, shading, the Open Range Breakout example, porting from other
charting scripts). Where it disagrees with the skill's reference, the
reference is right for this version: for example the library now has more
built-in indicators, the `interval` and `time` input types, and higher
timeframe data and alerts that the web page says do not exist.

**Checking a file before you add it.** If you have the OpenAlgo Desktop
source code and Node.js, the skill's validator runs your file against the
real library and installs it only when it passes:

```bash
node .claude/skills/chart-indicator/validate.mjs my_sma.js
node .claude/skills/chart-indicator/validate.mjs my_sma.js --install --into appdata
```

`--into appdata` is the folder above. Without the validator, the chart's own
checks report the same structural problems as messages when the indicator
loads.

## Troubleshooting

**It does not appear in the picker.** Check the file is in the `indicators`
folder of the data folder, ends in `.js` and follows the name rule above.
Close and open the picker again.

**A red message appears on the chart.** That is the chart telling you what is
wrong with your file, as `<file>: <message>`. The other indicators keep
working: one broken file never takes the picker down. Before an indicator
reaches the picker its descriptor is checked (a bad `placement`, an unknown
input type, missing or repeated plot keys), and its first `calc` result is
measured against the bars, which catches the two silent mistakes: a column
that is not `bars.length` long, and a plot key `calc` never filled.

**"module has no default-exported function".** The file loaded but does not
`export default function (api) { ... }`.

**Edits do not take effect.** Close and reopen the picker: that is when the
folder is read again. An indicator already on the chart keeps the version it
was added with until you remove it and add it again.

**The pane is flat or the line stops partway.** `calc` returned `0` for warmup
bars instead of `null`, or an array shorter than `bars`.

**Nothing loads and there is no message.** The chart treats a missing list as
"no custom indicators", which is also what it sees when you are signed out,
or when the folder does not exist. Sign in again and check the folder. The
app window has no developer tools in a release build; to see the list the
chart reads, open `http://127.0.0.1:5000/custom-indicators/index.json` in a
browser on this computer where you are signed in to OpenAlgo (use your port
if you changed it).

## Differences from OpenAlgo web

| | OpenAlgo web | OpenAlgo Desktop |
| --- | --- | --- |
| Folder | `strategies/indicators/` in the install (kept out of git, a Docker volume) | `indicators/` in the data folder |
| Served by | `blueprints/custom_indicators.py` | `src-tauri/src/server/routes/custom_indicators.rs`, signed-in session only |
| Size limit | None | 4 MB per file; links not followed |
| Library and loader | `openalgo-charts`, `customIndicators.ts` | The same version (pinned in `package.json`) and the same file |
| Developer tools for debugging | The browser's | Off in the app window in release builds; use a browser on the same computer |
