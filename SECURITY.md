# Security

## Reporting a problem

Please report security problems privately to **ne@3645.io**, not in a public issue. Say
which version you use (the app's About box or `not-everything -version`), what you found,
and how to reproduce it.

## Supported versions

Fixes go into the latest release. Release candidates (the Not-Everything Preview app) are
test builds and are replaced by the next one. Beta builds are never published.

## What the app does and doesn't do

Not-Everything is read-only: it indexes file names and metadata on local disks and never
changes, moves or deletes a file. It makes no network connections.

The app writes only its own index and settings, in its data folder,
`~/Library/Application Support/Not-Everything/` (`Not-Everything Preview` for the test
builds). The `not-everything` command writes nothing but an export file you name with one of
its `-export-*` options, and it replaces that file if it already exists.

Once you grant Full Disk Access, the index holds the names of files in the folders macOS
protects, such as Desktop, Documents, Downloads and Mail. The app therefore creates its data
folder and the files in it readable only by your user account.

Releases and release candidates are signed with a Developer ID and notarized by Apple;
`SHA256SUMS.txt` on each lists the files' checksums.
