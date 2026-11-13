# fx-ui-macos

The native macOS shell, built on AppKit through `objc2`. It holds the
`fx-ui-macos` app (the search window over `fx-engine`) and the Phase 0 UI
spike whose code and instrumentation decided how the shell is built. The
spike and the app's measurement modes build only with the `measure`
feature (see [Measurement builds](#measurement-builds)); release builds
leave it off.

## The app

One window: a search field on top, a view-based `NSTableView` (Name, Path,
Size, Date Modified; fixed row height) and a status bar ("Indexing… 1,234
objects", plus the selection count). The first window, after
`docs/img/mock-ui.png`, used standard controls only; since v0.3.0 it is
custom-drawn in the 3645 design system instead (see
[The 3645 interface](#the-3645-interface)), in Light and Dark following the
system appearance.

- Each change in the search field runs the query on one background search
  thread (the previous search is cancelled) and the page is applied on the
  main thread. Rows come from `Engine::search`, fetched a window at a time.
- Clicking a column header sorts through the engine (`set_sort`); AppKit
  never sorts. Clicking it again flips the direction.
- A thin divider sits between the column titles after Name, Path and
  Size; it turns green while hovered, focused or dragged. Dragging it
  resizes the column on its left, in the header and every row together,
  never below that column's minimum (Name 140, Path 120, Size 64 points)
  nor above 800; Date Modified takes the rest, never less than 140, and
  when the columns don't fit, the table scrolls sideways with the titles
  following it. Double-clicking a divider restores the default widths.
  With the keyboard focus (Full Keyboard Access on, or VoiceOver running),
  Left and Right move it 8 points, 32 with Shift; VoiceOver reads each
  divider as a splitter ("Resize the Name column", its width in points)
  and moves it with increment and decrement. The widths are saved as the
  `column_widths` line of the settings file (`300,480,88`; no line for the
  default layout) and restored at launch. The card draws its own titles
  and dividers; the table's own header and column resizing are off.
- A background thread receives the engine's new snapshot generations and
  wakes the main thread, which re-runs the query (at most every 100 ms), so
  results grow during the first scan and update when files change.

### Menu bar, hotkey and login item

The app is a resident app with a menu-bar item. Closing the window (red
button or Cmd+W) only hides it; the app keeps running and indexing. Quit
comes from the app menu, Cmd+Q or the status menu.

- **Status item.** A magnifying-glass template image in the menu bar. Its
  menu is rendered from `menu::build_menu`: the app's name and version or
  label, greyed out, Show Search Window (with the hotkey), Settings…,
  Launch at Login (checked when on), Grant Full Disk Access… (only without
  access) and Quit. No item deletes, renames, moves or trashes anything.
- **Global summon hotkey.** Default **⌥Space** (`option+space`); change it
  with the `hotkey` line of
  `~/Library/Application Support/Not-Everything/settings` (for example
  `hotkey=cmd+shift+space`) and relaunch. It is registered through the
  `global-hotkey` crate (Carbon `RegisterEventHotKey`, no Accessibility
  prompt); the crate hands each event to a handler that wakes the main
  thread, so nothing polls for it (0.3.1 polled every 100 ms). Pressing it
  shows the window, activates the app and focuses the search field;
  pressing it again while the window is key hides it and hands the focus
  back to the app you were using before (see Escape below). If the
  combination cannot be registered (another app holds it) the app logs it
  to stderr and keeps running; the status menu still shows the window.
- **Launch at Login.** The menu item registers or unregisters
  `SMAppService.mainAppService` and saves `launch_at_login` in the settings.
  A login item that needs approval is reported in the status bar
  ("requires approval in System Settings › Login Items"); errors are shown
  there too, never as a crash. It only works from the bundled `.app`: run
  unbundled (`cargo run`) the item is disabled.
- **Limited mode (no Full Disk Access).** Before the scanner is built the
  app probes access with `fx_fs_macos::fda::FdaProbe`. Unless access is
  granted, the scan excludes `consent_prompt_paths` (Desktop, Documents,
  Downloads, iCloud Drive, cloud storage, Photos, Mail, Messages, Safari,
  containers, the TCC folders and `/Volumes`), so no TCC consent dialog
  appears. The status bar then ends with "Limited mode: protected folders
  not indexed" and shows **Grant Full Disk Access…** (opens the Privacy &
  Security pane) and **Not Now** (hides the banner, remembered in the
  settings). A `--root` inside one of those folders is refused in limited
  mode with a message instead of raising the prompt. Access is re-probed
  whenever the app becomes active and every 750 ms while the banner shows
  and the app is active, so a grant clears the banner within 2 s of
  re-activating the app.

Manual check that no consent prompt appears without Full Disk Access:

```sh
cargo build --release -p fx-ui-macos
cargo run --release -p fx-ui-macos --example make_bundle -- \
    target/app-bundle target/release/fx-ui-macos --app
tccutil reset All io.3645.not-everything
open target/app-bundle/Not-Everything.app   # no TCC dialog may appear
```

### Dock, menu bar and Settings

- **Dock and menu bar.** The app uses the regular activation policy and the
  bundle does not set `LSUIElement`, so it has a Dock icon and its own menu
  bar. Clicking the Dock icon shows the window. The menu bar is rendered
  from `main_menu::main_menu`: the app menu (About, Settings… ⌘,, Hide,
  Hide Others, Show All, Quit), File (Close Window), Edit (Undo, Redo, Cut,
  Copy, Paste, Select All) and Window (Minimize, Zoom, Bring All to Front).
  No item deletes, renames, moves or trashes a file.
- **Settings window** (`src/appkit/window/settings_window.rs`), drawn in
  the 3645 design (`settings_controls.rs`); its copy, contents and layout
  are in `settings_view`. **Appearance** is System, Light or Dark
  (`NSApp.appearance`). **App Icon** offers the six approved designs
  (`app_icon::AppIcon`). Each change applies at once and is saved as the
  `appearance` and `app_icon` lines of the settings file.
- **Dock icon** (`app_icon::dock_icon`; a Preview build's is
  `dock_ribbon::dock_tile`, below). With Match Row and the System
  appearance the Dock shows the bundle's icon, which macOS draws in every
  appearance and icon style. Any other choice sets
  `NSApp.applicationIconImage` to the design's embedded image, its dark
  artwork when the app is dark; the controller observes
  `NSApp.effectiveAppearance` to follow changes. macOS has no public API for
  alternate Mac app icons, so Finder and Launchpad always show Match Row.
- **Icons.** `scripts/build-icons.sh` renders the approved designs
  (`scripts/icons/neu-icon.mjs` holds the NeuIcon artwork: Green Rim
  border, Glass) with headless Chrome and compiles them with Xcode's
  `actool`. Each icon is an opaque fill (the design's ground colour) under
  one translucent foreground layer, so macOS can draw its clear and tinted
  styles. It writes `assets/AppIcon.icon` (the Icon Composer source of the
  app icon, Match Row light and dark), `assets/bundle/` (`AppIcon.icns` and
  `Assets.car`, which `make_bundle --app` copies into the bundle) and
  `assets/icons/` (each design's light and dark image as macOS renders it,
  256 px, embedded in the binary). The results are checked in, so building
  needs no Xcode; rerun the script only when the designs change.
- **Name.** The app is Not-Everything, bundle ID `io.3645.not-everything`
  (`APP_NAME`, `APP_BUNDLE_ID`), the owner's choice. A release candidate or
  beta build (`NE_CHANNEL=rc` or `beta` at compile time, read by
  `fx-channel`) is a separate app, so testing never touches the daily one:
  Not-Everything Preview, `io.3645.not-everything.preview`, with the data
  folder `Not-Everything Preview` and Option+Shift+Space as its default
  hotkey. `bundle::write_app_bundle` allows "Everything" only in exactly the
  release and Preview names and bundle IDs. The plist's `CFBundleVersion` is
  the build number (`bundle::BUILD_NUMBER`, 200 for 0.2.0).
- **Channel and version** (`build_info`, `dock_ribbon`). The release app's
  window reads "Not-Everything" over the whole disk; a `--root` folder,
  synthetic rows and the measurement modes add " — <what it shows>". A
  release candidate or beta puts its label after the name
  ("Not-Everything Preview 0.2.0-RC-1"). The status menu starts with the
  name and version or label ("Not-Everything 0.2.0"), and About shows the
  standard About panel with "Version <label> (<build number>)" and the
  third-party notices as its credits. A release candidate or beta draws a
  corner ribbon, "RC" or "BETA", across the top-right of whichever Dock icon
  is chosen while it runs, redrawn when the appearance or the icon changes;
  it always shows the design's embedded image so the ribbon can be drawn on
  it. The Finder icon is unchanged and a release draws no ribbon.

### The 3645 interface

Since v0.3.0 the windows are custom-drawn after the owner's app mockups, in
the 3645 design system, rather than built from standard controls. The
values and rules are pure modules, each pinned by tests:

- `theme`: every 3645 colour token for Light and Dark, the four shadows
  (raised, pressed, float, key) with their CSS definition and `CALayer`
  form, type styles, spacing, radii and the mockups' component metrics.
- `formats`: sizes in 1,024-based units ("182 KB", "1.2 MB", "—" for a
  folder), dates as "14 Mar 2026 09:12" in local time, "~" for the home
  folder in the Path column (copied paths stay full), counts and search
  times.
- `file_glyph`: which glyph a row shows (the search's `pic:` list for
  images, `xls`, `xlsx`, `numbers` and `csv` for spreadsheets, folders,
  everything else a document) and the glyphs' paths.
- `status_view`: the status bar's counts, key hints or notes, and index
  state; `key_caps`: key caps in Apple's order and the result hints.
- `chips`, `main_layout` and `window_view`: the Aa, Path and .* search
  options, where the search window's parts go, and what it shows in each
  state; `column_sizes`: the widths the column dividers set.
- `ui_text`, `settings_view` and `build_info`: the mockups' copy, the
  Settings copy and the channel tag.
- `surface`: every surface of the mockups rendered once per theme and size
  class into a nine-part template (CSS box shadows as true Gaussians of
  σ = blur ÷ 2), and the glyphs as template masks. In the AppKit layer
  `appkit::window::surfaces` caches them as shared `NSImage`s with 1x and
  2x representations and cap insets, and `appkit::window::paint` makes the
  theme's colours and fonts. No view draws a shadow of its own, and
  scrolling draws no surface, so the drawn window scrolls as fast as
  standard controls.

### Signing, executable name and data folder

- **Signing.** `make_bundle` signs every bundle it writes
  (`bundle::sign`, then `codesign --verify --deep --strict`): a bundle with
  only the executable's linker signature does not cover the bundle, so
  Gatekeeper calls a downloaded copy damaged. By default the signature is
  ad hoc (`bundle::Signer::AdHoc`), which needs the user's approval on
  first open and loses Full Disk Access on every update.
- **Developer ID.** `make_bundle --sign-identity NAME` signs with a keychain
  certificate instead (`bundle::Signer::Identity`, with a secure
  timestamp). Every signature, ad hoc included, turns on the hardened
  runtime; the app needs no entitlements. Releases come from
  `scripts/release-macos.sh`, which signs with the owner's Developer ID,
  notarizes and staples (`--resume OUT_DIR` finishes a release whose upload
  Apple was still processing when the script stopped). To check a Developer
  ID signature on a Mac that has one: `NE_SIGN_IDENTITY="Developer ID
  Application: …" cargo test -p fx-ui-macos --test bundle -- --ignored`.
- **Executable.** The product bundle names its executable after the app,
  `Contents/MacOS/Not-Everything`, and the app's log messages start with
  `Not-Everything:`. The Cargo binary stays `fx-ui-macos`.
- **Data folder.** Settings and stores live in
  `~/Library/Application Support/Not-Everything/` (`app_args::APP_FOLDER`).
  The index holds the names of files in protected folders, so at launch
  the app creates the folder and its stores folder with mode 0700, or
  tightens them to 0700 if an older version left them open (`data_folder`,
  with fx-store's `make_dir_private`). The settings file and the stores are
  written with mode 0600, and a settings save also tightens its folder.
  The engine's saves only create a missing folder and never change one
  that exists.

**Store scope.** Each checkpoint store remembers the access it was built
with: `app_args::store_path_for(home, root, StoreScope)` gives every root a
`Full` store under `stores/` and a separate `Limited` store under
`stores-limited/`. A launch with access granted opens only the full store,
so the first one after a limited launch finds no store and starts a fresh
full scan that includes the protected folders; it never replays the limited
store's snapshot or cursor. Revoking access likewise switches to the
limited store.

**Grant without a relaunch.** A resident launch in limited mode runs its
engine behind a `switchable::SwitchableSource`. When onboarding first sees
access granted (`OnboardingOutput::rescan_excluded`, once per session and
only after a limited launch), `apply_onboarding` in
`src/appkit/window/resident.rs` opens a second engine over the same root on
a background thread, with no exclusions and the `Full` store (loaded when
present, else a first scan); the status bar says "Indexing protected
folders…" meanwhile. Once that engine has its first snapshot the source
switches to it, the window re-runs its search through the normal refresh
path, and the limited engine saves its store as on a quit
(`ResultsSource::save_for_quit`: only what its store lacks) and shuts
down. Generations carry the switch count in their top
bits, so the controller takes the switch as a newer generation and never
mixes pages or rows of the two indexes. Both engines run side by side until
the switch, so memory and scan work briefly double.

```sh
# Index / (default) or a folder. Needs a logged-in session (window server).
cargo run -p fx-ui-macos --release
cargo run -p fx-ui-macos --release -- --root ~/Projects
```

| Option | Meaning |
| --- | --- |
| `--root PATH` | Folder or volume to index (default `/`) |

A bad option prints the usage to stderr and exits with code 2 before any
window opens. On other hosts the binary says it is macOS-only and exits 0.

The engine's store for a root lives in
`~/Library/Application Support/Not-Everything/stores/` (one file per root),
so the next launch serves the saved index at once; without a store the
engine starts with a first scan.

**Saving the index.** A save rewrites the whole store (about 251 MB for a
whole disk) and flushes it to the disk, so since 0.3.2 the app saves it
rarely, as Everything does by default: once a day, when the app quits, and
right after a full scan. 0.3.1 saved at most 5 minutes after the first
unsaved change, and a whole disk always has one, so it wrote about 3 GB an
hour. The daily save (`daily_save`) is checked every 5 minutes by the
resident app: it is due 24 hours after the last save, once nobody has used
the keyboard or mouse for a minute (`CGEventSourceSecondsSinceLastEventType`),
and the engine's checkpoint thread writes it (`Engine::request_checkpoint`).
The day counts from the store's last write, whatever made it: since 0.4.0
the engine also saves after each compaction, and that save restarts the
day, so a compaction never adds a daily save of its own. The engine's own
idle save is off, and its longest delay, 48 hours, is only a backstop for a
Mac never idle for a minute at a check; it counts awake time, so on a
laptop that sleeps at night it can take three days or more.
`tests/daily_save_day.rs` runs the decision (`daily_save_due_at`, which
takes the clock as an argument) through simulated weeks: an office
desktop, a laptop asleep at night, a Mac never idle at a check, and
compactions at random times. Every save marks the store to be left out of Time Machine
backups: the index is rebuilt from the disk when lost. A crash loses
nothing: `Engine::open` replays the changes made since the saved cursor.

**Quitting.** Cmd+Q, the status menu's Quit or a logout first hides every
window and the menu-bar item (`quit::quit_steps`), then saves what the
store lacks (`Engine::checkpoint_for_quit`), so the next launch does not
replay up to a day of changes. Writing the whole store on the main thread
took 0.54 s from the request to process exit with the window still on
screen (2,673,111 entries, a 218 MB store, idle, M4 Pro, warm cache); when
the store already holds the latest snapshot nothing is written, and when a
save is running the quit waits for it instead of starting another.

**Background work.** The window re-runs its search for each new snapshot
only while it is on screen (`NSWindowDidChangeOcclusionStateNotification`):
hidden, covered, on another Space or behind a locked screen, a new snapshot
is only remembered, and the window re-runs once when it shows again. In
0.3.1 those re-runs took about 85% of the app's background CPU. The
engine's writer and checkpoint threads run at the utility
quality-of-service class and the search worker at user-initiated
(`fx_engine::qos`), so macOS can keep background work on the efficiency
cores.

Keys and actions (all read-only):

| Input | Action |
| --- | --- |
| Return / Enter in the results, double-click | Open the selection with its default app (at most 20 items) |
| Cmd+Return / Cmd+Enter | Reveal the selection in Finder |
| Option+Cmd+C | Copy the selection's full paths, one per line. Cmd+C and Edit > Copy do nothing in the results; Cmd+C will copy the files themselves, as in Finder, in a later version |
| Space in the results | Show the selection in the system Quick Look panel (at most 1,000 files); Space again closes it |
| Escape with Quick Look open | Close the Quick Look panel (nothing else) |
| Return / Enter or Down arrow in the search field | Move into the results, selecting the first row (nothing happens with no results) |
| Escape in the search field | Clear the search; with the field already empty, hide the window and go back to the previous app |
| Escape in the results | Clear the search and move back to the field; with the field empty, hide the window and go back to the previous app |
| Drag rows out | File URLs, with copy and link operations only |

Return in the search field moves into the results and never opens a file
(`actions::search_field_key`); the arrow keys then pick the file and Return
opens it. Cmd+Return in the search field reveals the selection, or the
first row, in Finder. Space in the search field types a space.

Quick Look works as in Finder (`quick_look`, `appkit::window::quick_look`):
Space in the results opens macOS's own Quick Look panel (`QLPreviewPanel`)
on the selected files, in row order; Space again, or Escape, closes it.
While it is open the arrow keys move the selection and the panel follows
(the panel hands the key presses it doesn't use to the results), reloading
only when the selected files change, so a refresh while indexing never
makes it flicker. The panel zooms from the selected row when the row is in
view, and closes when the window hides. QuickLookUI is a system framework,
loaded the first time the panel opens (so launches don't pay for it); no
crate was added.

VoiceOver (`accessibility`): the window draws its own look, so its views
report what VoiceOver reads. Each cell names its column ("Name", "Path",
"Size", "Date Modified") and reads its text in words, so a row reads in
the order the columns show it: "report.pdf, Home/Documents/Finance, 182
KB, 14 March 2026 at 09:12", with "folder" after a folder's name and no
size for it. (AppKit reports a view-based table's rows through elements
of its own, so the reading lives in the cells, which are the app's views.)
The table ("Results") reports the result count as its row count, the
status bar reads "8 results, 2,014,377 objects", "Up to date, 9 ms" and
each hint as "Reveal in Finder, Command Return". The chips, column titles and search field keep their
labels ("Match case", "Sort by Size, ascending", "Search file and folder
names"); glyphs and texts their control already reads are hidden. While
VoiceOver runs, a new search announces its count ("1,234 results", "No
matches") once typing pauses for half a second. To check what VoiceOver
reads, read the app's tree from outside it with the Accessibility API
(`AXUIElementCopyAttributeValue`), as VoiceOver does: asked from inside
the app, AppKit leaves out labels its controls carry. A debug build's
script (`NE_DEBUG_SCRIPT`) has `key:space`, `key:escape`, `activate`,
`announce` and `ax` (the table's row counts, the announcement and Quick
Look's state) for checking both in the app.

Escape backs out one step, as in Spotlight (`actions::escape_action`): it
clears the search, and with the search field empty it hides the window as
the summon hotkey does. The search field clears itself (`NSSearchField`'s
own Escape). Hiding is for the resident app only: in the measurement modes
and with a modifier held, Escape does not hide the window.

Hiding the window, by Escape or the hotkey, hands the focus back to the app
you were using before, as Spotlight does (`actions::hide_mode`): the app
hides as with Cmd+H, so macOS activates that app, and its windows come back
with the hotkey, the Dock or Cmd+Tab. The exception is a summon from
Not-Everything's own Settings window: then only the search window goes, and
Settings stays in front. The app keeps running either way.

## Measurement builds

The measurement modes, the `table_spike` example and the code behind them
(`model`, `frame_stats`, `spike_args`, `gate_probe`, `summon_probe`,
`sort_probe`, `latency_probe`, `source::SyntheticSource`, `appkit::spike`,
`appkit::window::probes` and the `fx-bench` dependency)
build only with the `measure` feature. Release builds leave it off, so a
release app rejects these options as unknown and carries none of that code.
The gate scripts (`scripts/phase0-gate.sh`, `scripts/phase1-gate.sh`)
build with `--features fx-ui-macos/measure`. Test and lint them with:

```sh
cargo test -p fx-ui-macos --features measure
cargo clippy -p fx-ui-macos --all-targets --features measure -- -D warnings
```

The app's measurement options:

| Option | Meaning |
| --- | --- |
| `--synthetic N` | Show N synthetic rows instead of an index (1 to 2³²−2) |
| `--page-cost-ms MS` | Make every synthetic page fetch take at least MS ms (0 to 3,600,000), to approximate engine paging cost |
| `--autoscroll` | Scroll 600 frames (5 rows each), print frame stats and the GATE line, exit |
| `--launch-probe` | Print launch-to-first-paint and launch-to-results times, exit |
| `--gate-probe OUT` | Phase 1 gate probe over the real index: write `GATE` lines for first result, keystroke to repaint and autoscroll frames to `OUT`, a new file, exit (not with `--synthetic`; see below) |
| `--summon-probe OUT` | Shortcut probe: hide the window as Escape does and show it as the global hotkey does, 50 times; write the shortcut-to-ready-window times to `OUT`, a new file, exit (see below) |
| `--sort-probe OUT` | Re-sort probe: with every entry listed, sort by every column both ways, one untimed round then 5 timed rounds; write the re-sort-to-repaint times to `OUT`, a new file, exit (see below) |
| `--data-dir DIR` | Keep the stores and settings in `DIR`, an existing folder, instead of the app's data folder; register no global hotkey and leave the login item alone |
| `--window hidden` | Start the resident app and hide the window as Escape does once it is active; then run until quit or SIGTERM |
| `--window search --query TEXT` | Start the resident app showing the search for `TEXT` and its results; then run until quit or SIGTERM |

A probe is a measurement of its own: `--summon-probe` and `--sort-probe`
refuse each other, `--gate-probe`, `--autoscroll` and `--launch-probe`.
`--window` refuses every measurement option, `search` needs `--query`, and
`--query` needs `search`. A bad combination exits with code 2, as any bad
option does.

Measurements on the real window (release build, one at a time):

```sh
cargo run -p fx-ui-macos --release --features measure -- --synthetic 2000000 --autoscroll
cargo run -p fx-ui-macos --release --features measure -- --synthetic 2000000 --launch-probe
cargo run -p fx-ui-macos --release --features measure -- --root ~/Projects --launch-probe
```

The autoscroll frame is timed as in the spike (scroll, layout and draw on the
main thread), and includes fetching row windows from the source. In
measurement modes a watchdog exits with code 1 after 120 s without a report.
Screenshots for docs and PRs use `--synthetic` rows, never real files.

**The scroll p95 is synthetic.** `--autoscroll` runs on `SyntheticSource`,
whose pages come from an in-memory `fx-bench` tree with precomputed sort
orders, so a page fetch costs far less than `Engine::search` over a real
2M-entry index (query matching, a sorted window, then row reads from the
snapshot). The p95 it reports is the cost of AppKit scrolling, layout and
drawing plus a near-free fetch; it is not the frame time over a real
engine. To approximate engine paging, add a simulated per-page cost:

```sh
# Every page fetch takes at least 3 ms (sleeping for what the real work did
# not use); the rows and their order are unchanged.
cargo run -p fx-ui-macos --release --features measure -- --synthetic 2000000 --autoscroll --page-cost-ms 3
```

`--page-cost-ms` maps to `SyntheticSource::with_page_cost`. Pick the cost
from a measured `Engine::search` page time on the same machine (for example
from the `fx-engine` benchmarks) and record it next to the p95. Without the
option the cost is zero. Record which variant a number came from next to
it.

## Phase 1 gate probe

`--gate-probe OUT` measures three Phase 1 criteria on the real engine over
the saved fx-store index for `--root` (it refuses `--synthetic`), writes the
report to `OUT`, a new file (it never replaces one), prints it, then exits:

```sh
cargo build --release -p fx-ui-macos --features measure
# Build the store first (one normal launch over the root), then:
target/release/fx-ui-macos --root /Volumes/Bench2M --gate-probe ui-probe.log
```

| Gate line | What is timed | Budget |
| --- | --- | --- |
| `ui launch to first result painted` | Process start (first line of `main`) to the first table draw after the first non-empty results page | ≤2.0 s per launch; the gate script runs 20 launches and takes the p95 |
| `ui keystroke to repaint p95` | For each step of `gate_probe::default_keystroke_script()` (44 steps typing `DEFAULT_QUERIES` one character at a time): setting the search field and running the query, to the first table draw after that query's page is applied | p95 ≤50 ms |
| `ui autoscroll frame p95` | 600 autoscroll frames (5 rows each) over all entries (empty query) of the real engine | p95 ≤16.7 ms |

Each line has the form `GATE <criterion>: PASS|FAIL (<detail>)`, which the
fx-bench gate summary parses; the report's header names the entry count.
After the GATE lines come the measured values, one `key=value` line each,
for the performance gate's script: `launch_ms` (process start to the first
result painted), `keystroke_p95_ms`, `scroll_p95_ms` (the autoscroll frame
p95) and `ui_entries`. Times are milliseconds with two decimals; a value
without samples has no line, and the lines before them read as they always
have, as `scripts/phase1-gate.sh` parses them. A
criterion without samples is a FAIL line. A keystroke whose repaint does not
come within 10 s is recorded as that slow; no first result within 60 s
leaves goal 1 without a sample. A watchdog exits with code 1 after 900 s, and
a report that cannot be written (for example because `OUT` exists) exits
with code 1. The keystroke samples start at the query change (what the
field's action does), so they leave out AppKit's key-event dispatch into the
field. The probe only reads the index and writes its report file.

The pure parts (argument parsing, the keystroke script, the
[`ProbeRun`](src/gate_probe.rs) state machine, timing aggregation and the
report) live in `src/gate_probe.rs` and are tested on any host
(`tests/gate_probe.rs`, with `--features measure`); the AppKit glue only
performs the actions `ProbeRun::tick` asks for.

## Shortcut and re-sort probes

Two more probes time the window for the performance gate. Both run on
`--synthetic` rows (cheap and the same every run) or on the engine over
`--root`, wait until the list is steady (the first results painted, the
index not scanning, no new page for 300 ms; at most 300 s), write their
report to `OUT`, a new file, print it and exit: code 0, or 1 without samples
or when `OUT` cannot be written. A watchdog exits with code 1 after 1,200 s.

```sh
cargo build --release -p fx-ui-macos --features measure
DATA=$(mktemp -d)
target/release/fx-ui-macos --synthetic 200000 --data-dir "$DATA" --summon-probe summon.txt
target/release/fx-ui-macos --synthetic 200000 --data-dir "$DATA" --sort-probe sort.txt
```

The report of each:

```text
fx-ui-macos summon probe: <n> entries
GATE ui shortcut to window ready p95 <= 100 ms: PASS|FAIL (p95=<x>ms p50=<y>ms n=<k>)
summon_p95_ms=<x>
summon_p50_ms=<y>
summon_samples=<k>
summon_entries=<n>
```

The sort probe's is the same with `sort probe`, `GATE ui re-sort to repaint
p95 <= 50 ms` and `sort_` keys. Without samples the GATE line is
`FAIL (no samples)` and the p95 and p50 lines are left out.

| Probe | What is timed | Budget |
| --- | --- | --- |
| `--summon-probe` | 50 cycles. The window is hidden with what Escape in the empty field calls, and the probe waits until it is off screen and the app no longer active, then 100 ms more. The sample starts at the call the global hotkey's handler makes (`toggle_window`) and ends when the window is ready: on screen, the key window of the active app, the search field holding the focus, and its display pass drawn and committed | p95 ≤100 ms |
| `--sort-probe` | With the empty query (every entry listed), each column title is clicked twice per round (Name, Path, Size, Date Modified, so every column both ways). The sample runs from sending the title's action, as a click does, to the first table draw after the re-sorted page is applied. One untimed round first, then 5 timed rounds (40 samples) | p95 ≤50 ms |

**Window ready.** The probe looks at the window every millisecond. Once it
is on screen, key and active with the search field focused, the probe runs
the window's display pass (`displayIfNeeded`, what AppKit runs at the end
of the run-loop turn) and flushes the Core Animation transaction to the
window server; the sample ends when that returns. The window server shows
that frame at its next refresh, up to one frame later, which the sample
leaves out. The sample starts at the app's hotkey handler, not at the key
press: posting real key presses needs the Accessibility permission, which a
measurement run should not ask for, so the system's hotkey dispatch and the
app's wake-up are not in it. A window that will not go off screen within
3 s skips its cycle; one not ready within 5 s is recorded as that slow.

**Repainted.** As for the keystroke gate: the first draw of the results
table after the page answering the new sort is applied. A click not
repainted within 10 s is recorded as that slow. The untimed round is there
because `SyntheticSource` builds each column's sort order the first time it
is asked for, which the engine never does.

## Background runs: `--window` and `--data-dir`

For CPU, energy and memory sampling the app runs as it does every day,
resident with its menu-bar item, only started in a known state:

```sh
DATA=$(mktemp -d)
target/release/fx-ui-macos --root / --data-dir "$DATA" --window hidden &
target/release/fx-ui-macos --root / --data-dir "$DATA" --window search --query main &
```

- `--window hidden`: once the launched app is active (at most 3 s), the
  window is hidden with what Escape calls, so the app hides and the previous
  app takes the focus; for 2 s more it is hidden again if it comes back,
  then no probe timer is left running.
- `--window search --query TEXT`: the window shows with `TEXT` in the
  search field and its results listed, set as typing sets them.

Either way the app then runs until it is quit or gets SIGTERM, which ends
it at once (in about 10 ms in a smoke run), without the quit's index save.
There is no watchdog.

`--data-dir DIR` makes `DIR`, which must exist, the data folder: the stores
go to `DIR/stores/` or `DIR/stores-limited/` and the settings to
`DIR/settings`, so a measurement never touches the daily app's (or the
Preview app's) index or settings. `DIR` and the folders in it are made
owner-only (0700), as the default data folder is, since the index lists the
names of files in protected folders; a folder from `mktemp -d` already is.
A run with `--data-dir` registers no global hotkey, so it never takes the
daily app's shortcut, and leaves the login item alone: it reports Launch at
Login as unavailable and refuses to change it.

## What the spike is

`table_spike` (a `measure` build) opens one window with:

- a search field at the top, which filters the rows by name (a plain
  case-insensitive substring scan, not the real search engine);
- a view-based `NSTableView` with a fixed row height and the columns Name,
  Path, Size and Date Modified, over 2,000,000 virtual rows by default. The
  rows are synthetic file names from `fx-bench`; the table asks for the cells
  on screen only and reuses cell views, so no per-row objects exist;
- a status bar with the row count ("2,000,000 objects").

It uses standard AppKit controls only, so light and dark mode work without
extra code.

The main modules, beside the interface modules above:

| Module | Builds on | Purpose |
| --- | --- | --- |
| `source` | any host | `ResultsSource` over the engine (and, with `measure`, synthetic rows) |
| `columns` | any host | The four columns and the header-click sort state |
| `column_sizes` | any host | The widths the column dividers set: drag, arrow keys, minimums, the settings value |
| `controller` | any host | `ResultsController`: query, sort, stale pages, throttled re-runs, status |
| `actions` | any host | Read-only actions, key map, drag-out mask |
| `app_args` | any host | Command line of the app, the data folder and the per-root store path |
| `data_folder` | any host | The data folder's owner-only folders and files |
| `bundle` | any host | `.app` bundle writer and signer, and `dir_size` |
| `model` | any host, `measure` | Virtual row model: cell text, size and date formatting, filter |
| `frame_stats` | any host, `measure` | Per-frame timings, percentiles and the GATE line |
| `spike_args` | any host, `measure` | Command-line options of `table_spike` |
| `gate_probe` | any host, `measure` | The Phase 1 gate probe's script, timings and report |
| `summon_probe` | any host, `measure` | The shortcut probe's cycles and timing |
| `sort_probe` | any host, `measure` | The re-sort probe's clicks and timing |
| `latency_probe` | any host, `measure` | The shortcut and re-sort probes' report |
| `appkit` | macOS only | `appkit::window` (product window), `appkit::app` (what the binary runs) and, with `measure`, `appkit::spike` (the spike window) |

`objc2`, `objc2-foundation`, `objc2-app-kit`, `global-hotkey` and
`objc2-service-management` are dependencies only under
`cfg(target_os = "macos")`. On other hosts the examples print "macOS only"
(or, for `make_bundle`, still write a bundle) and the host-side modules are
tested as usual with `cargo test -p fx-ui-macos`.

## Running it

Build in release; debug builds are much slower and their numbers mean nothing:

```sh
cargo build --release --features measure --example table_spike -p fx-ui-macos
```

Run it from Terminal in a logged-in session (it needs the window server):

```sh
# Interactive: type in the search field, scroll, quit with Cmd+Q.
target/release/examples/table_spike

# Scroll 600 frames, print frame statistics and the GATE line, then exit.
target/release/examples/table_spike --autoscroll 600
target/release/examples/table_spike --autoscroll 600 --rows-per-frame 3000

# Print the time from start to the first painted table, then exit.
target/release/examples/table_spike --launch-probe
```

Options:

| Option | Default | Meaning |
| --- | --- | --- |
| `--rows N` | 2000000 | Number of virtual rows |
| `--seed S` | 1 | Seed of the synthetic names |
| `--autoscroll FRAMES` | 0 (interactive) | Scroll this many frames, report, exit |
| `--rows-per-frame K` | 5 | Rows scrolled per autoscroll frame |
| `--launch-probe` | off | Report launch to first paint, exit |
| `--json` | off | Print one JSON object instead of text |

A bad option prints the usage text and exits with code 2. In `--autoscroll`
and `--launch-probe` modes a watchdog exits with code 1 if no report appears in
time, for example when there is no window server, so a measurement never
hangs.

To measure the `.app` size, wrap the release binary in a minimal bundle:

```sh
cargo run --release -p fx-ui-macos --example make_bundle -- target/spike-bundle target/release/examples/table_spike
```

This writes `target/spike-bundle/Table Spike.app` and prints its size. It
refuses to replace an existing bundle; use a new directory to measure again.

The full procedure (repeat counts, warm and cold runs, what to record) is in
`docs/gates/phase-0.md`, and `scripts/phase0-gate.sh` runs it.

## Reading the GATE line

An autoscroll run ends with a line like:

```text
GATE: PASS p95=5.00ms budget=16.7ms
```

- `PASS` means at least one frame was recorded and the 95th-percentile frame
  time (nearest rank) is at or below the budget; otherwise it reads `FAIL`.
- `p95` is that percentile over all frames of the run; `p95=n/a` means no
  frame was recorded, which always fails.
- `budget` is one 60 Hz frame, 16.7 ms, the Phase 0 gate.

The lines above it give the frame count, p50, p99, the slowest frame and how
many frames went over budget. With `--json` the same values come as one
object, and its `pass` field replaces the GATE line. The process exit code
does not carry the verdict; read the line.

A frame is the main-thread time to scroll by `--rows-per-frame` rows, lay out
and draw. It does not include the window server's compositing, so treat a
result within about 2 ms of the budget as borderline.

## Read-only by design

The shell never changes files. The spike has no file actions at all: no open,
reveal, copy, drag, delete, rename, move or trash, and its cells are
non-editable labels. The app's actions are limited to opening a result,
revealing it in Finder, copying its full path and showing it in Quick Look
(macOS's preview panel, which only reads the file). There is no delete,
rename, move or trash menu item, action, key
binding or command-line option; the delete keys do nothing. Dragging a
result out offers copy and link only (`NSDragOperationCopy |
NSDragOperationLink`, for drags to other apps and within the app), never
move, generic or delete, so dropping it on the Trash or a Finder folder
cannot remove or move the file. The Edit menu's Undo, Redo, Cut and Paste
edit the search field's text only; Copy copies it or, with the table
focused, the selection's paths; Select All selects its text or, with the
table focused, every row.
