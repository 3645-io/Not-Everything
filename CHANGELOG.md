# Changelog

## v0.4.0: a tenth of the memory, a quiet background (November 2026)

Not-Everything now uses about a tenth of the memory it did, barely touches the processor while its window is hidden, and scans the disk faster. Signed and notarized. Apple silicon only, macOS 13 or later.

### Before you update

- **Nothing to do.** Full Disk Access, Launch at Login, your hotkey, your settings and your index carry over from 0.3.2.

### Changed

- **About a tenth of the memory.** On the test Mac's whole disk (about 2 million files), the app takes 125 MB with its window hidden (1,281 MB in 0.3.2) and 169 MB showing a search (1,345 MB). The first scan peaks at 393 MB (2,175 MB). The saved index is now read straight from its file instead of being copied into memory, the app no longer keeps a second copy of every file's path, the first scan builds the index as the files are listed, and a folder macOS asks the app to look through again is read the same way.
- **A quiet background.** With the window hidden, the app uses 1.4% of one core (10.4% in 0.3.2) and 10 mW (68 mW), and wakes up 0.3 times a second (3.0). Changes are gathered for 2 seconds while the window is hidden, and a change that leaves the index as it was no longer starts a new search. Showing a search costs about 60% less processor time.
- **Faster to scan.** A first scan of the whole disk takes about 15 s (19.0 s in 0.3.2). From launch to the first result takes about 1 s, as before, and a search is usable 10 ms after a relaunch (13 ms).
- **A faster command.** The `not-everything` command answers a search over about 2 million files in about 0.2 s (0.6 s in 0.3.2): it now checks only the saved sort order it needs, and only on the files it found.
- **Set your own shortcut.** Settings › General › Show the search window records a new shortcut: click the keys, press the combination you want. Shortcuts that type a character, or that macOS uses, are refused, and Use Default brings back Option-Shift-Space.

### Fixed

- **A moved folder's contents could drop out of the index** when a change inside it was reported under the folder's old path.
- **A quit during a background save no longer leaves a temporary file** as large as the index; the next start removes any that a crash left.

### For testers

Release candidates install as **Not-Everything Preview**, with its own settings, index, hotkey (Option-Shift-Space) and command (`not-everything-preview`). It runs next to your everyday Not-Everything without touching it.

### Install

Open `Not-Everything-0.4.0-macos-arm64.dmg` and drag Not-Everything to Applications, replacing the old copy. The zip, `Not-Everything-0.4.0-macos-arm64.zip`, holds the same signed app. `not-everything-cli-0.4.0-macos-arm64.tar.gz` holds the `not-everything` command, its manual (`cli.md`), the license and the third-party notices.

## v0.3.2: less background work (November 2026)

A small update that makes Not-Everything far lighter while it waits in the background. Signed and notarized. Apple silicon only, macOS 13 or later.

### Before you update

- **Nothing to do.** Full Disk Access, Launch at Login, your hotkey, your settings and your index carry over from 0.3.1.

### Changed

- **No searching while the window is out of sight.** When the window is hidden, behind other windows, on another Space or the screen is locked, the results no longer refresh with every file change; they refresh once when the window shows again.
- **The index is saved once a day instead of every 5 minutes**, at a moment you are not using the keyboard or mouse, and when you quit. 0.3.1 rewrote the whole index (about 250 MB for a whole disk) every 5 minutes while files kept changing. After a crash, the next launch still catches up on everything that changed.
- **The index is no longer copied into Time Machine backups.** The app rebuilds it from the disk when needed.
- **The hotkey no longer wakes the app 10 times a second** to check whether it was pressed.
- **Background work runs at a lower priority**, so macOS can keep it on the efficiency cores.
- **The `not-everything` command reads the last saved index**, which can now be up to a day old while the app runs.

### For testers

Release candidates install as **Not-Everything Preview**, with its own settings, index, hotkey (Option-Shift-Space) and command (`not-everything-preview`). It runs next to your everyday Not-Everything without touching it.

### Install

Open `Not-Everything-0.3.2-macos-arm64.dmg` and drag Not-Everything to Applications, replacing the old copy. The zip, `Not-Everything-0.3.2-macos-arm64.zip`, holds the same signed app. `not-everything-cli-0.3.2-macos-arm64.tar.gz` holds the `not-everything` command, its manual (`cli.md`), the license and the third-party notices.

## v0.3.1: Copy path moves to Option-Command-C (November 2026)

A small update that frees ⌘C for copying files, as in Finder. Signed and notarized. Apple silicon only, macOS 13 or later.

### Before you update

- **Nothing to do.** Full Disk Access, Launch at Login, your hotkey and your settings carry over from 0.3.0.

### Changed

- **Copy path is now Option-Command-C (⌥⌘C)**, the shortcut Finder uses to copy a pathname. It copies the selected results' full paths, one per line, as ⌘C did, and the status bar's hint shows the new keys.
- **⌘C in the results no longer copies paths**, and Edit > Copy is unavailable while the results have the focus: ⌘C will copy the files themselves, as in Finder, in a later version. In the search field, ⌘C still copies the selected text.

### For testers

Release candidates install as **Not-Everything Preview**, with its own settings, index, hotkey (Option-Shift-Space) and command (`not-everything-preview`). It runs next to your everyday Not-Everything without touching it.

### Install

Open `Not-Everything-0.3.1-macos-arm64.dmg` and drag Not-Everything to Applications, replacing the old copy. The zip, `Not-Everything-0.3.1-macos-arm64.zip`, holds the same signed app. `not-everything-cli-0.3.1-macos-arm64.tar.gz` holds the `not-everything` command, its manual (`cli.md`), the license and the third-party notices.

## v0.3.0: a new look, search as you type (November 2026)

The search window and Settings are redrawn in the new 3645 design, results follow each key you type, and full-path search is ten times faster. The first release published from this repository. Signed and notarized. Apple silicon only, macOS 13 or later.

### Before you update

- **Nothing to do.** Full Disk Access, Launch at Login, your hotkey and your settings carry over from 0.2.0.

### Changed

- **A new look, light and dark.** A raised top bar holds the search field and three option chips. Results sit in a card with file icons and highlighted matches, and a status bar shows the counts, key hints and whether the index is up to date. The window opens at 1280 × 800 (smaller on smaller screens) and never shrinks below 760 × 480. Settings follows the same design.
- **Results follow each key.** The search now runs as you type, about 200 ms sooner than before, when it waited for a pause in typing.
- **"plumb's" finds only names with an apostrophe.** A search without one still ignores apostrophes, so "plumbs" finds "Plumb’s". A search with one matches only names that have one. ' ’ ‘ and ʼ count as the same character.
- **Full-path search is ten times faster.** With the Path chip on or `path:`, a search over 2 million names takes under 25 ms (95th percentile), down from about 275 ms.
- **Dates use your local time** ("14 Mar 2026 09:12"). Sizes use 1,024-based units, "~" stands for your home folder, and folders show "—" for size.
- **Your index and settings are private to your account.** The app's data folder and its files are now readable only by you, because the index lists file names from folders macOS protects.
- **The command shows control characters in names as `?` on a terminal**, in txt and csv output, so a file name can't send your terminal an escape sequence. Pipes and exported files still get the exact names. json escapes every control character wherever it is written, now U+0080 to U+009F too.
- **A smaller download.** The disk image is about 4.2 MB, and the app shows its first results about 10% sooner.

### Added

- **Option chips: Aa, Path and .\*** for match case, match the full path, and a regular expression for the whole search. They are remembered between launches.
- **Matches are highlighted** in names and paths.
- **No matches** shows example searches you can click. An invalid regular expression says what is wrong.
- **Indexing shows progress** in the status bar, with "of about N" from the previous index's size.
- **Open-source notices.** About Not-Everything lists the licences of the open-source code the app is built with, and `not-everything -licenses` prints them.
- **`not-everything -0`** ends each result with a NUL byte instead of a newline, for file names that contain line breaks (`xargs -0`).

### Fixed

- **Clicking a result left the arrow keys in the search field.** A click now moves the keyboard focus to the results, so the arrow keys move from the clicked row.

### For testers

Release candidates install as **Not-Everything Preview**, with its own settings, index, hotkey (Option-Shift-Space) and command (`not-everything-preview`). It runs next to your everyday Not-Everything without touching it.

### Install

Open `Not-Everything-0.3.0-macos-arm64.dmg` and drag Not-Everything to Applications, replacing the old copy. The zip, `Not-Everything-0.3.0-macos-arm64.zip`, holds the same signed app. `not-everything-cli-0.3.0-macos-arm64.tar.gz` holds the `not-everything` command, its manual (`cli.md`), the license and the third-party notices. Releases are now published at https://github.com/3645-io/Not-Everything/releases.

## v0.2.0: notarized, quick to quit, your files first (November 2026)

The first release signed with a Developer ID and notarized by Apple, and the first one tested through release candidates. Apple silicon only, macOS 13 or later.

Published from the project's earlier, private repository; its downloads are not on this one.

### Before you update

- **Grant Full Disk Access once more.** The app's signature changes from ad hoc to the Developer ID, so macOS treats it as a new app once. From this release on, updates keep the grant.
- **Switch Launch at Login back on** in the menu-bar menu if it turned off.
- **No more Open Anyway.** The app is notarized, so macOS opens it without the extra approval.

### Fixed

- **Granting Full Disk Access needed a quit and reopen.** The full scan now starts as soon as access is granted, and the window switches to it when it is ready.
- **Cmd+Q took about half a second, with the window still showing.** Quitting now hides the app at once and skips rewriting the saved index when the next launch can catch up from it.
- **"plumbs" did not find "Plumb’s".** Search now ignores apostrophes (' ’ ‘ ʼ) in names and searches, as it ignores accents. `diacritics:` and `regex:` still see the exact text.
- **A refresh could move the selection to another file.** The selected files stay selected when results update. Copy, open and drag never pick up the wrong files during a refresh.
- **The window title read "Not-Everything — /".** It now reads "Not-Everything".

### Added

- **Your files first**, on by default in Settings. Results in your home folder, iCloud Drive and cloud drives (Google Drive, OneDrive, Dropbox) come first under every sort, then the rest. Hidden files and `~/Library` don't count as yours.
- **The sort is remembered** between launches.
- **Escape hides the window** when the search field is empty, and returns you to the app you were using, as Spotlight does. With text, Escape clears the search first.
- **The version is shown** in the menu-bar menu and the About panel.
- **A disk image.** Download `Not-Everything-0.2.0-macos-arm64.dmg`, open it and drag the app to Applications.

### For testers

Release candidates install as a separate app, **Not-Everything Preview**. It has its own settings and index, its own hotkey (Option-Shift-Space) and its own command (`not-everything-preview`), and an RC or BETA ribbon on its Dock icon. It runs next to your everyday Not-Everything without touching it.

### Not in this release

- **Search from Spotlight** moves to a later version. A spike showed that Spotlight's Tab search lists only content copied into Spotlight's own index, and Not-Everything keeps its index to itself.

### Install

Open `Not-Everything-0.2.0-macos-arm64.dmg` and drag Not-Everything to Applications, replacing the old copy. The zip, `Not-Everything-0.2.0-macos-arm64.zip`, holds the same signed app. `not-everything-cli-0.2.0-macos-arm64.tar.gz` holds the `not-everything` command, its manual (`cli.md`) and the license.

## Earlier versions

v0.1.0 to v0.1.3 were private macOS previews.
