# Product rules for the MVP

- Read-only. No delete, rename, move or trash action in the app or the CLI.
- Dragging a result out offers copy and link only, never move or delete, so dropping it on the Trash or a Finder folder can't remove the file.
- Allowed actions: in the results, Enter opens and Cmd+Enter reveals in Finder; copy full path; Space shows the selection in macOS's Quick Look panel (run 2.5). Return in the search field only moves to the results, never opens a file, and Space there types a space.
- Never trigger cloud downloads: switch off materialization of dataless files and never recurse into cloud-only folders.
- Network drives are out of the MVP (Phase 6).

## Names

The product is named Not-Everything, bundle ID `io.3645.not-everything`, with the command-line tool `not-everything` and the data folder `~/Library/Application Support/Not-Everything/`. Its release candidates and betas are Not-Everything Preview, bundle ID `io.3645.not-everything.preview`, command `not-everything-preview`, data folder `~/Library/Application Support/Not-Everything Preview/`. The notary profile is `not-everything-notary`, and the disk images' signing identifiers are `io.3645.not-everything.dmg` and `io.3645.not-everything.preview.dmg`.

The owner chose these exact names, and they are the only exceptions: never use "Everything" in crate names, any other name or bundle ID, or the Homebrew cask, so nothing else the project names can be taken for voidtools' product. `fx_ui_macos::bundle` refuses any other app name or bundle ID that contains the word. The app's old working name, data folder and bundle ID are gone; don't bring them back.
