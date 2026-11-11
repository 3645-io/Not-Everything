#!/bin/sh
# macOS release builder.
#
# Builds Not-Everything in release mode for one channel, from the locked
# dependencies (--locked), signs the app and the command with the owner's
# Developer ID (hardened runtime, secure timestamp), puts the app in a disk
# image and writes the files into a new folder:
#
#   no mode   the release, from main after the release PR. Notarized and
#             stapled (two uploads, below); tag release/vX.Y.Z, a full
#             GitHub release (--latest)
#             with the version's CHANGELOG.md entry as its notes.
#   --rc      release candidate N, from rc/X.Y.Z. Notarized and stapled; tag
#             release/vX.Y.Z-RC-N, a GitHub pre-release.
#   --beta    beta N, from dev. Signed but not notarized; tag
#             beta/vX.Y.Z-beta-N, no GitHub page.
#
# X.Y.Z is the root Cargo.toml's version and N is one more than the highest
# existing tag of that kind for it. Release candidates and betas are the
# separate Preview app (fx-channel): Not-Everything Preview.app, bundle ID
# io.3645.not-everything.preview, and the not-everything-preview command,
# with their own data folder and hotkey. The files:
#
#   release   Not-Everything-<version>-macos-arm64.dmg
#             Not-Everything-<version>-macos-arm64.zip
#             not-everything-cli-<version>-macos-arm64.tar.gz
#   Preview   Not-Everything-Preview-<label>-macos-arm64.dmg
#             Not-Everything-Preview-<label>-macos-arm64.zip
#             not-everything-preview-cli-<label>-macos-arm64.tar.gz
#             (label X.Y.Z-RC-N or X.Y.Z-beta-N)
#   both      SHA256SUMS.txt
#
# The disk image (volume "<app name> <label>") opens on a Finder window
# holding the app and a shortcut to /Applications to drag it onto. It is
# built from the signed app (for a release or an RC, once that app is
# notarized and stapled) in a fresh folder work/dmg.XXXXXX. Finder sets the
# window's size and icon positions on a read-write image, which is then
# converted to a read-only zlib-compressed one (UDZO, as Apple's packaging
# guide asks; APFS) and signed with the Developer ID (identifier
# <bundle ID>.dmg). If Finder can't be scripted, or macOS doesn't let this
# terminal control it, the image keeps Finder's plain layout and the script
# says so. The zip holds the same app; the tarball holds the command,
# cli.md, LICENSE and THIRD-PARTY-NOTICES.md. The app carries the notices
# too (in its About panel and bundle), and the script stops before building
# (or, for --resume, before going on) if they don't match Cargo.lock
# (scripts/third-party-notices.sh --check).
#
# Before building, it checks the git state and refuses unless:
#   release   on main with a clean tree; the version's latest release
#             candidate tag has main's tree and is merged into main; the
#             version is not released yet; CHANGELOG.md has its entry.
#   --rc      on rc/X.Y.Z with a clean tree; HEAD is not a release candidate
#             yet; the version is not released yet.
#   --beta    on dev with a clean tree.
# After signing, it checks that the app's designated requirement is the
# bundle ID plus the owner's team (so Full Disk Access carries over to the
# new build) before anything is uploaded or written. Before each staple it
# checks that Apple's ticket covers the signed items of that upload (their
# CDHash in the notary log). Before writing the files it checks a
# quarantined copy of the disk image, as a download arrives: hdiutil verify,
# codesign --verify and, unless it is a beta, spctl --assess --type open
# (which must report "Notarized Developer ID") and stapler validate; then it
# mounts the copy read-only and checks the app inside like the zipped one,
# syspolicy_check distribution included (which wants that app stapled).
# After writing them it checks a quarantined copy of the zipped app:
# codesign --verify --deep --strict and, unless it is a beta, spctl --assess
# (which must report "Notarized Developer ID") and syspolicy_check
# distribution.
#
# Tags and GitHub releases are made only with --publish: an annotated tag on
# the built commit, pushed to origin, then `gh release create` for a release
# candidate or a release. Without --publish the script stops after writing
# the files and prints the exact commands it would run. It never moves or
# deletes a tag or a release, and refuses one that already exists; only
# gh's "release not found" counts as no release, so any other gh error
# (signed out, no network) stops it before anything is tagged.
#
# The Plan prints origin's URL. --publish refuses, before fetching anything,
# unless origin is the project's GitHub repository, 3645-io/Not-Everything
# (NE_RELEASE_REPO names another as owner/name, to test the script on a
# fork). For a release or an RC it also needs the GitHub CLI (gh), signed in
# to github.com and pointing at that repository (gh repo view), checked
# before building and before --resume goes on.
#
# --perf PERF_DIR puts a performance table in a release candidate's or a
# release's notes. PERF_DIR is the output folder of
# scripts/perf-check.sh --tier rc for the commit being built. --publish of a
# release candidate or a release needs it, --resume included; without
# --publish it is optional, so a dry run shows the notes as they will be
# published; a beta takes none.
# Before fetching or building anything, the script refuses a PERF_DIR
# without metrics.txt, summary.md or run.txt, one of another tier, one whose
# run.txt git: line says (uncommitted changes) or names a commit whose code
# (its tree) differs from the commit being built (for --resume, the build's
# recorded one), and one whose last perf-compare: line in summary.md has a
# failing metric (a regression the owner approved is an exception in
# docs/gates/perf-baseline.toml, so it passes). A release built from the
# merged release PR can use its release candidate's run: same tree, same
# code. The run comes before the build, so it has no DMG size yet: once the
# disk image is built, its size is added and the whole rc tier is judged
# again (fx-bench perf-compare), so a failing or unmeasured metric stops the
# release then.
# The table compares with the previous release, the highest release/vX.Y.Z
# tag below the version (release candidates' tags don't count), so the
# script also refuses, before building, a version with no earlier release or
# whose previous release's measured values are not in
# docs/gates/releases/<X.Y.Z>.txt. The release build includes fx-bench,
# which is not shipped. Once the disk image is final, before the files are
# written, the script writes $OUT/perf-metrics.txt (PERF_DIR's metrics.txt
# and the disk image's size, dmg_bytes), has that fx-bench (perf-release)
# compare it with docs/gates/perf-baseline.toml and the previous release
# into $OUT/performance.md (with PERF_DIR's notes.md, the owner's own lines,
# if it has one), puts that table in the notes after the CHANGELOG.md entry,
# and keeps the values as $OUT/metrics-<label>.txt, which at the release the
# owner commits as docs/gates/releases/<X.Y.Z>.txt. --resume makes them
# again from its own --perf whenever it gets that far.
#
# Notarizing takes two uploads, in the order Apple documents for nested
# containers (sign and staple the inner item before packaging it):
#   1. the signed app and the command, in one zip (work/for-notary.zip).
#      Once Apple accepts it and its ticket covers both, the app is
#      stapled.
#   2. the disk image, built from the stapled app and signed, uploaded on
#      its own. Once Apple accepts it and its ticket covers it, it is
#      stapled.
# The app inside the disk image is then stapled too, which
# syspolicy_check distribution requires. A bare command can't hold a
# stapled ticket, so Gatekeeper looks the command's up online.
#
# Apple keeps an upload after this script stops (lost contact, a closed
# terminal, a restart). `--resume OUT_DIR` carries on with that folder at
# the stage it stopped in: it reads the build's channel (work/release-info)
# and the saved submissions (work/for-notary.json for the first upload,
# work/for-notary-dmg.json for the second, whose disk image's path is in
# work/for-notary-dmg.path). It never rebuilds or uploads again a stage
# whose submission was saved: it waits for the first upload, staples the
# app and builds and uploads the disk image, or, once the disk image was
# uploaded, waits for that and finishes with it. It refuses a folder with no
# saved first upload or channel record, a beta's, one missing the signed app
# or command (or, once the disk image was uploaded, that image), one that
# already holds release files and, with --perf, one whose build has no
# fx-bench.
#
# Needs, in the login keychain: the Developer ID Application certificate,
# named by NE_SIGN_IDENTITY as `security find-identity -v -p codesigning`
# shows it ("Developer ID Application: <name> (<team ID>)"; the team ID is
# read from its closing brackets), and the notarytool profile saved with
# `xcrun notarytool store-credentials` (NE_NOTARY_PROFILE overrides its
# name). The identity lives only in the builder's environment, never in the
# repository, which is public. A release or an RC checks the profile with
# Apple (xcrun notarytool history) before building, so a missing one never
# costs a build. Each upload usually takes Apple a few minutes.
#
# The build maps the builder's home folder to ~ in the binaries' source
# paths (Rust keeps them for panic messages), and the script stops before
# signing if the app or the command still holds the home folder. The CLI
# tarball lists its files as root:wheel with no extended attributes, ACLs or
# file flags, and the script stops after writing it if it names another
# owner or holds any.
#
# Safety: this script only creates the new <out> directory and files inside
# it (work/ holds a fresh build, the signed copies, the disk image builds
# and the uploads; check/ holds the quarantined copies); --resume only adds
# files inside the folder it resumes, besides rewriting its logs, records
# and performance table. It never deletes, renames or moves anything. It
# mounts the read-write layout image in /Volumes, where Finder sees it, and
# the quarantined copy read-only in check/mount, and detaches every image it
# attached before it exits. It fetches tags from origin before checking
# them.

set -eu

usage() {
    cat >&2 <<'EOF'
usage: scripts/release-macos.sh [--rc | --beta] [--perf PERF_DIR] [--publish] NEW_OUT_DIR
       scripts/release-macos.sh --resume OUT_DIR [--perf PERF_DIR] [--publish]
EOF
    exit 2
}

fail() {
    echo "release-macos: $*" >&2
    exit 1
}

step() {
    printf '\n== %s\n' "$*"
}

# The team ID in a Developer ID certificate's name: the ten capitals and
# digits in its closing brackets.
team_of() {
    printf '%s\n' "$1" | sed -n 's/^Developer ID Application: .* (\([A-Z0-9]\{10\}\))$/\1/p'
}

# The signing identity comes from NE_SIGN_IDENTITY, never from this script,
# so the public repository names no one; the team comes from it.
check_identity() {
    [ -n "$IDENTITY" ] ||
        fail "set NE_SIGN_IDENTITY to the Developer ID Application certificate's name, as security find-identity -v -p codesigning shows it"
    TEAM_ID=$(team_of "$IDENTITY")
    [ -n "$TEAM_ID" ] ||
        fail "NE_SIGN_IDENTITY must be a Developer ID Application certificate's name, ending in its team ID in brackets"
}

# The app, the command and the tarball carry THIRD-PARTY-NOTICES.md, the
# notices their open-source crates ask for; it must match Cargo.lock, for a
# new build and for the tarball a resumed one makes.
check_notices() {
    sh "$ROOT/scripts/third-party-notices.sh" --check ||
        fail "THIRD-PARTY-NOTICES.md must match Cargo.lock before a release"
}

# Rust keeps source paths in a binary for its panic messages, the
# dependencies' under ~/.cargo. The build maps the home folder to ~, so a
# binary that still holds it would ship the builder's user name.
check_no_home() {
    for f in "$@"; do
        if LC_ALL=C grep -a -q -F "$HOME/" "$f"; then
            fail "$f holds the home folder $HOME; build with RUSTFLAGS=--remap-path-prefix=\$HOME=~"
        fi
    done
}

# tar records each file's owner, extended attributes, ACLs and file flags,
# so a plain tarball would ship the builder's account name (as the owner, or
# inside an ACL) and com.apple.provenance. Writes the CLI tarball $1 from
# the folder $2 with its files as root:wheel and none of those
# (COPYFILE_DISABLE keeps out AppleDouble ._ files).
make_cli_tar() {
    COPYFILE_DISABLE=1 tar --no-mac-metadata --no-xattrs --no-acls --no-fflags \
        --uid 0 --gid 0 --uname root --gname wheel \
        -czf "$1" -C "$2" "$CLI_NAME" cli.md LICENSE THIRD-PARTY-NOTICES.md
}

# Stops unless the tarball $1 lists every file as root:wheel and holds no
# pax extended header (the SCHILY. and LIBARCHIVE. keys that carry extended
# attributes, ACLs and file flags) and no AppleDouble ._ files, which tar -t
# hides but pax lists.
check_tarball() {
    [ "$(tar -tvzf "$1" | awk '{ print $3 ":" $4 }' | sort -u)" = root:wheel ] ||
        fail "$1 must list every file as root:wheel"
    if gzip -dc "$1" | LC_ALL=C grep -a -q -E '(LIBARCHIVE|SCHILY)\.'; then
        fail "$1 holds extended headers (attributes, ACLs or file flags)"
    fi
    if pax -z -f "$1" | grep -q -E '(^|/)\._'; then
        fail "$1 holds AppleDouble ._ files"
    fi
}

MODE=release
RESUME=0
PUBLISH=0
PERF_DIR=
DIR=
while [ "$#" -gt 0 ]; do
    case $1 in
    --rc | --beta)
        [ "$MODE" = release ] && [ "$RESUME" -eq 0 ] || usage
        MODE=${1#--}
        ;;
    --resume)
        [ "$MODE" = release ] && [ "$RESUME" -eq 0 ] || usage
        RESUME=1
        ;;
    --perf)
        # Once, with a folder, never another option.
        [ -z "$PERF_DIR" ] && [ "$#" -ge 2 ] || usage
        case $2 in '' | -*) usage ;; esac
        PERF_DIR=$2
        shift
        ;;
    --publish) PUBLISH=1 ;;
    -*) usage ;;
    *)
        [ -z "$DIR" ] || usage
        DIR=$1
        ;;
    esac
    shift
done
[ -n "$DIR" ] || usage
# A beta has no GitHub page, so no performance table.
[ "$MODE" != beta ] || [ -z "$PERF_DIR" ] || usage

IDENTITY=${NE_SIGN_IDENTITY:-}
PROFILE=${NE_NOTARY_PROFILE:-not-everything-notary}
RELEASE_REPO=${NE_RELEASE_REPO:-3645-io/Not-Everything}
TEAM_ID=
ROOT=$(cd "$(dirname "$0")/.." && pwd)

# The names of the app built for $CHANNEL, as fx-channel sets them, and the
# tag and files for $LABEL.
set_names() {
    case $CHANNEL in
    release)
        APP_NAME="Not-Everything"
        BUNDLE_ID="io.3645.not-everything"
        CLI_NAME="not-everything"
        APP_ZIP=Not-Everything-$LABEL-macos-arm64.zip
        APP_DMG=Not-Everything-$LABEL-macos-arm64.dmg
        CLI_TAR=not-everything-cli-$LABEL-macos-arm64.tar.gz
        TAG=release/v$LABEL
        ;;
    rc | beta)
        APP_NAME="Not-Everything Preview"
        BUNDLE_ID="io.3645.not-everything.preview"
        CLI_NAME="not-everything-preview"
        APP_ZIP=Not-Everything-Preview-$LABEL-macos-arm64.zip
        APP_DMG=Not-Everything-Preview-$LABEL-macos-arm64.dmg
        CLI_TAR=not-everything-preview-cli-$LABEL-macos-arm64.tar.gz
        if [ "$CHANNEL" = rc ]; then
            TAG=release/v$LABEL
        else
            TAG=beta/v$LABEL
        fi
        ;;
    esac
    TITLE="$APP_NAME $LABEL"
    VOLUME="$APP_NAME $LABEL"
}

# The highest N of the tags named $1<N>, or 0.
highest() {
    n=$(git -C "$ROOT" tag -l "$1*" | sed -n "s|^$1\([1-9][0-9]*\)\$|\1|p" | sort -n | tail -n 1)
    echo "${n:-0}"
}

# The CHANGELOG.md entry of $VERSION, without its heading.
changelog_entry() {
    awk -v heading="## v$VERSION" '
        index($0, "## ") == 1 {
            if (found) exit
            found = ($0 == heading || index($0, heading ":") == 1 || index($0, heading " ") == 1)
            next
        }
        found { print }
    ' "$ROOT/CHANGELOG.md"
}

# Full Disk Access and the other privacy grants carry over to a new build
# only while its designated requirement is the bundle ID plus the team, so
# stop on any other.
check_requirement() {
    want="designated => identifier \"$BUNDLE_ID\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = $TEAM_ID"
    got=$(codesign -d -r- "$APP" 2>&1 | sed -n '/^designated => /p')
    [ "$got" = "$want" ] ||
        fail "the designated requirement of $APP is not the expected one, so Full Disk Access would not carry over; want: $want; got: ${got:-none}"
    echo "Designated requirement: $got"
}

# $1 in lower case: GitHub matches owner and repository names without
# regard to case.
lower() { printf '%s\n' "$1" | tr '[:upper:]' '[:lower:]'; }

# The owner/name of the GitHub repository at the git URL $1 (https, ssh or
# scp-like), or nothing for any other URL.
github_repo() {
    printf '%s\n' "$1" |
        sed -n -E 's#^(https://([^/@]+@)?github\.com/|ssh://git@github\.com/|git@github\.com:)([^/]+/[^/]+)$#\3#p' |
        sed 's#\.git$##'
}

# --publish pushes the tag to origin and makes the GitHub release there, so
# origin must be the project's repository, never a fork or an old copy.
check_origin() {
    url=$(git -C "$ROOT" remote get-url origin 2>/dev/null) ||
        fail "--publish pushes the tag to origin, and this checkout has no origin remote"
    [ "$(lower "$(github_repo "$url")")" = "$(lower "$RELEASE_REPO")" ] ||
        fail "--publish needs origin to be github.com/$RELEASE_REPO (or NE_RELEASE_REPO set to the repository to test with); origin is $url"
}

# Checks what --publish needs before anything is built or awaited: origin,
# and for a release or an RC (a beta only pushes its tag) a gh signed in
# and pointing at the same repository, which `gh release` then uses.
check_publish_tools() {
    check_origin
    [ "$CHANNEL" != beta ] || return 0
    command -v gh >/dev/null || fail "--publish needs the GitHub CLI (gh) on PATH"
    gh auth status --hostname github.com >/dev/null 2>&1 ||
        fail "--publish needs gh signed in to github.com; run gh auth login"
    gh_repo=$(cd "$ROOT" && gh repo view --json nameWithOwner --jq .nameWithOwner 2>&1) ||
        { printf '%s\n' "$gh_repo" >&2; fail "gh cannot open this checkout's GitHub repository"; }
    [ "$(lower "$gh_repo")" = "$(lower "$RELEASE_REPO")" ] ||
        fail "gh points at $gh_repo, not $RELEASE_REPO; run gh repo set-default $RELEASE_REPO"
}

# Stops unless GitHub has no release $1. Only gh's "release not found" means
# none: any other failure (signed out, no network) is not taken for it.
check_no_release() {
    if view=$(gh release view "$1" 2>&1); then
        fail "GitHub already has a release $1; nothing was tagged or published"
    fi
    case $view in
    *"release not found"*) ;;
    *)
        printf '%s\n' "$view" >&2
        fail "cannot tell whether GitHub has a release $1 (gh release view failed); nothing was tagged or published"
        ;;
    esac
}

# A release or an RC is notarized with the profile $PROFILE, so it must work
# before anything is built and signed.
check_notary_profile() {
    xcrun notarytool history --keychain-profile "$PROFILE" >/dev/null 2>&1 ||
        fail "the notary profile $PROFILE is missing or can't reach Apple; check it with xcrun notarytool history --keychain-profile $PROFILE, or save it with xcrun notarytool store-credentials $PROFILE"
}

# A release candidate's or a release's GitHub page carries the performance
# table, so publishing the channel $1 needs --perf; a beta has no page.
need_perf() {
    if [ "$PUBLISH" -eq 1 ] && [ "$1" != beta ] && [ -z "$PERF_DIR" ]; then
        fail "--publish of a release candidate or a release needs --perf PERF_DIR, the output folder of scripts/perf-check.sh --tier rc for the commit being built; its table goes in the release notes"
    fi
}

# Stops unless $PERF_DIR is a finished scripts/perf-check.sh --tier rc run,
# from a clean tree, of the commit $1 or of a commit with the same tree,
# whose last perf-compare: line has no failing metric (a regression the
# owner approved is an exception in the baseline, so it passes). Unmeasured
# metrics are allowed here, because the DMG size can't be measured before
# the build; perf_section judges the whole tier once it is. Sets PERF_DIR
# to its full path.
check_perf() {
    [ -d "$PERF_DIR" ] ||
        fail "--perf $PERF_DIR is not a folder; give the output folder of scripts/perf-check.sh --tier rc"
    PERF_DIR=$(cd "$PERF_DIR" && pwd)
    for f in metrics.txt summary.md run.txt; do
        [ -f "$PERF_DIR/$f" ] ||
            fail "$PERF_DIR has no $f, so it is not a finished scripts/perf-check.sh run"
    done
    perf_tier=$(sed -n 's/^tier: //p' "$PERF_DIR/run.txt" | head -n 1)
    [ "$perf_tier" = rc ] ||
        fail "the performance run in $PERF_DIR is of the ${perf_tier:-unknown} tier; a release needs scripts/perf-check.sh --tier rc"
    perf_git=$(sed -n 's/^git: //p' "$PERF_DIR/run.txt" | head -n 1)
    case $perf_git in
    *"(uncommitted changes)"*)
        fail "the performance run in $PERF_DIR measured uncommitted changes; run scripts/perf-check.sh --tier rc again on a clean checkout of $1"
        ;;
    esac
    if [ "$perf_git" != "$1" ]; then
        # The same code under another commit, such as the release PR's
        # merge of the release candidate, needs no second run.
        perf_tree=$(git -C "$ROOT" rev-parse -q --verify "${perf_git:-none}^{tree}" 2>/dev/null) || perf_tree=
        [ -n "$perf_tree" ] && [ "$perf_tree" = "$(git -C "$ROOT" rev-parse "$1^{tree}")" ] ||
            fail "the performance run in $PERF_DIR measured commit ${perf_git:-unknown}, not $1, the commit being built, and its code differs; run scripts/perf-check.sh --tier rc on it"
    fi
    perf_line=$(grep '^perf-compare: ' "$PERF_DIR/summary.md" | tail -n 1)
    case $perf_line in
    *", 0 fail,"*) ;;
    *) fail "the performance run in $PERF_DIR did not pass every metric (${perf_line:-summary.md has no perf-compare: line}); fix the failures and measure again, or record the owner's approval of a regression as its exception in docs/gates/perf-baseline.toml" ;;
    esac
}

# Sets PREV to the previous release, the highest release/vX.Y.Z tag below
# $VERSION (a release candidate's tag doesn't count), and stops unless
# docs/gates/releases/$PREV.txt holds its measured values, which the
# performance table compares with.
check_previous_release() {
    PREV=$(git -C "$ROOT" tag -l 'release/v*' |
        sed -n -E 's#^release/v([0-9]+\.[0-9]+\.[0-9]+)$#\1#p' |
        awk -F . -v version="$VERSION" '
            BEGIN { split(version, v, "."); below = (v[1] * 1000 + v[2]) * 1000 + v[3] }
            {
                n = ($1 * 1000 + $2) * 1000 + $3
                if (n < below && (prev == "" || n > best)) { best = n; prev = $0 }
            }
            END { print prev }
        ')
    [ -n "$PREV" ] ||
        fail "no release before $VERSION (no release/vX.Y.Z tag below it), so the performance table has no previous release to compare with"
    [ -f "$ROOT/docs/gates/releases/$PREV.txt" ] ||
        fail "docs/gates/releases/$PREV.txt is missing: the performance table compares with the measured values of $PREV, the previous release; commit them first (its release folder's metrics-$PREV.txt)"
}

# The mount point in what `hdiutil attach` prints: the third tab-separated
# field of the volume's line, after the device lines and any warning.
mount_point() {
    awk -F '\t' 'NF >= 3 && $3 != "" { mp = $3 } END { print mp }'
}

# Attaches the disk image $1 with the hdiutil options after it and sets
# MOUNTED to its volume's mount point. detach_image detaches it, and runs
# when the script exits, so a failed check never leaves an image mounted.
MOUNTED=
attach_image() {
    image=$1
    shift
    attached=$(hdiutil attach -noautoopen "$@" "$image" 2>&1) ||
        { printf '%s\n' "$attached" >&2; fail "cannot attach $image"; }
    MOUNTED=$(printf '%s\n' "$attached" | mount_point)
    [ -n "$MOUNTED" ] || { printf '%s\n' "$attached" >&2; fail "no volume of $image was mounted"; }
}

detach_image() {
    [ -n "$MOUNTED" ] || return 0
    volume=$MOUNTED
    MOUNTED=
    # Finder or Spotlight may hold the volume for a moment.
    n=1
    until hdiutil detach -quiet "$volume"; do
        if [ "$n" -ge 5 ]; then
            hdiutil detach -quiet -force "$volume" && return 0
            echo "release-macos: cannot detach $volume; eject it in Finder" >&2
            return 1
        fi
        n=$((n + 1))
        sleep 2
    done
}
trap detach_image EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# Marks $1 as downloaded, as a browser does.
quarantine() {
    xattr -w com.apple.quarantine "0081;$(printf '%x' "$(date +%s)");release-macos;" "$1"
}

# Checks the app $1 as a download arrives: its signature and, unless it is a
# beta, Gatekeeper's verdict.
check_app() {
    codesign --verify --deep --strict --verbose=2 "$1"
    if [ "$CHANNEL" = beta ]; then
        echo "A beta is not notarized, so Gatekeeper would ask to approve it; skipping spctl and syspolicy_check"
        return 0
    fi
    spctl --assess --type execute --verbose=4 "$1" >"$WORK/spctl.log" 2>&1 ||
        { cat "$WORK/spctl.log" >&2; fail "Gatekeeper rejected $1"; }
    cat "$WORK/spctl.log"
    grep -q "source=Notarized Developer ID" "$WORK/spctl.log" ||
        fail "Gatekeeper did not see $1 as notarized"
    syspolicy_check distribution "$1"
}

# Makes the disk image from the signed $APP (stapled, for a release or an
# RC) and sets DMG to it, as Apple's "Packaging Mac software for
# distribution" describes: a folder with the app and a shortcut to
# /Applications, imaged read-write so Finder can lay out its window, then
# converted to a read-only zlib-compressed image (UDZO), signed with the
# Developer ID and verified. Apple advises against heavily compressed images,
# so UDZO keeps hdiutil's default zlib level. Each build gets a fresh folder,
# so a build cut short is never reused or overwritten.
make_dmg() {
    build=$(mktemp -d "$WORK/dmg.XXXXXX") || fail "cannot make a folder in $WORK"
    DMG=$build/$APP_DMG
    stage=$build/folder
    mkdir "$stage"
    ditto "$APP" "$stage/$APP_NAME.app"
    ln -s /Applications "$stage/Applications"
    hdiutil create -quiet -srcfolder "$stage" -volname "$VOLUME" -fs APFS \
        -format UDRW "$build/layout.dmg"
    # In /Volumes, where Finder lists it.
    attach_image "$build/layout.dmg" -readwrite -noverify
    echo "Finder sets the window's size and icon positions; macOS may ask to let this terminal control Finder"
    if osascript - "$MOUNTED" "$APP_NAME.app" >"$build/layout.log" 2>&1 <<'EOF'
on run argv
    set mountPoint to item 1 of argv
    set appName to item 2 of argv
    tell application "Finder"
        -- The disk mounted there, even if another disk has its name.
        set theDisk to missing value
        repeat with d in (get every disk)
            if POSIX path of (d as alias) is (mountPoint & "/") then set theDisk to contents of d
        end repeat
        if theDisk is missing value then error "Finder does not list " & mountPoint
        open theDisk
        set theWindow to container window of theDisk
        set current view of theWindow to icon view
        set toolbar visible of theWindow to false
        set statusbar visible of theWindow to false
        -- 560 x 360 points: the app on the left, Applications on the right.
        set bounds of theWindow to {200, 120, 760, 480}
        set viewOptions to icon view options of theWindow
        set arrangement of viewOptions to not arranged
        set icon size of viewOptions to 128
        set position of item appName of theDisk to {150, 160}
        set position of item "Applications" of theDisk to {410, 160}
        update theDisk without registering applications
        delay 1
        close theWindow
    end tell
end run
EOF
    then
        # Finder saves the layout in .DS_Store once the window has closed.
        n=0
        while [ ! -f "$MOUNTED/.DS_Store" ] && [ "$n" -lt 10 ]; do
            n=$((n + 1))
            sleep 1
        done
        if [ -f "$MOUNTED/.DS_Store" ]; then
            echo "Finder laid out the window"
        else
            echo "Finder did not save the layout, so the disk image keeps the plain layout (the app and the Applications shortcut)"
        fi
    else
        cat "$build/layout.log"
        echo "Finder could not be scripted (not running, or this terminal is not allowed under Privacy & Security > Automation), so the disk image keeps the plain layout (the app and the Applications shortcut)"
    fi
    sync
    detach_image
    hdiutil convert -quiet "$build/layout.dmg" -format UDZO -o "$DMG"
    codesign --sign "$IDENTITY" --timestamp --identifier "$BUNDLE_ID.dmg" "$DMG"
    codesign --verify --strict "$DMG"
    hdiutil verify "$DMG" >"$build/verify.log" 2>&1 ||
        { cat "$build/verify.log" >&2; fail "hdiutil verify rejected $DMG"; }
    echo "$DMG: signed and verified"
}

# Uploads $1 to Apple without waiting and saves Apple's answer, the
# submission, as $2.json (what --resume reads), only once the upload has
# worked.
upload() {
    xcrun notarytool submit "$1" --keychain-profile "$PROFILE" \
        --output-format json >"$2.out" 2>"$2.err" ||
        { cat "$2.err" >&2; fail "the upload of $1 to Apple failed"; }
    id=$(plutil -extract id raw -o - "$2.out") || fail "no submission ID in $2.out"
    cp "$2.out" "$2.json"
    echo "Submission $id uploaded; waiting for Apple"
}

# Waits for Apple's verdict on the submission saved in $1.json and keeps its
# log in $1.log. Waiting means polling Apple for minutes, so a dropped
# request (seen in testing: "The request timed out") retries the wait
# instead of failing a submission Apple still holds.
wait_for() {
    id=$(plutil -extract id raw -o - "$1.json") || fail "no submission ID in $1.json"
    tries=1
    while :; do
        xcrun notarytool wait "$id" --keychain-profile "$PROFILE" >"$1.log" 2>&1 || true
        if grep -q "status: Accepted" "$1.log"; then
            break
        fi
        if grep -Eq "status: (Invalid|Rejected)" "$1.log"; then
            cat "$1.log" >&2
            fail "Apple did not accept $id; see xcrun notarytool log $id --keychain-profile $PROFILE"
        fi
        if [ "$tries" -ge 5 ]; then
            cat "$1.log" >&2
            fail "lost contact with Apple 5 times; Apple still holds $id, so carry on later with scripts/release-macos.sh --resume $OUT"
        fi
        echo "Lost contact with Apple (try $tries of 5); waiting again in 30 s"
        tries=$((tries + 1))
        sleep 30
    done
    grep -v "Current status" "$1.log"
}

# Checks that Apple's ticket for the submission saved in $1.json lists the
# CDHash of each signed item after it. The ticket lists every signed item
# Apple accepted, so an item changed since the upload is caught here rather
# than by Gatekeeper.
check_ticket() {
    record=$1
    shift
    id=$(plutil -extract id raw -o - "$record.json") || fail "no submission ID in $record.json"
    xcrun notarytool log "$id" --keychain-profile "$PROFILE" "$record.ticket.json" \
        >"$record.ticket.err" 2>&1 ||
        { cat "$record.ticket.err" >&2; fail "cannot fetch Apple's notary log for $id"; }
    for item in "$@"; do
        cdhash=$(codesign -dvvv "$item" 2>&1 | sed -n 's/^CDHash=//p')
        [ -n "$cdhash" ] || fail "no CDHash for $item"
        grep -qi "$cdhash" "$record.ticket.json" ||
            fail "$item: CDHash $cdhash is not in Apple's ticket for $id (it changed since the upload, or Apple did not ticket it); start a new release"
        echo "$item: CDHash $cdhash is in the ticket"
    done
}

# The performance table, once the disk image $DMG is final: writes
# $OUT/perf-metrics.txt (PERF_DIR's measured values, then the disk image's
# size, which counts as the last dmg_bytes), has the release build's
# fx-bench compare it with the baseline and the previous release $PREV
# (with PERF_DIR's notes.md, the owner's own lines, if it has one) into
# $OUT/performance.md, and keeps the values as $OUT/metrics-$LABEL.txt.
perf_section() {
    dmg_bytes=$(stat -f %z "$DMG") || fail "cannot read the size of $DMG"
    {
        awk '{ print }' "$PERF_DIR/metrics.txt"
        echo "dmg_bytes=$dmg_bytes"
    } >"$OUT/perf-metrics.txt"
    # The whole rc tier again, now with the disk image's size (O1): a
    # failing or unmeasured metric stops the release here.
    "$TARGET/release/fx-bench" perf-compare --baseline "$ROOT/docs/gates/perf-baseline.toml" \
        --tier rc "$OUT/perf-metrics.txt" >"$OUT/perf-gate.md" ||
        fail "with the disk image's size added, the release's metrics don't pass the gate (see $OUT/perf-gate.md); fix them and measure again, or record the owner's approval of a regression as its exception in docs/gates/perf-baseline.toml"
    set --
    [ ! -f "$PERF_DIR/notes.md" ] || set -- --notes "$PERF_DIR/notes.md"
    "$TARGET/release/fx-bench" perf-release --baseline "$ROOT/docs/gates/perf-baseline.toml" \
        --previous "$ROOT/docs/gates/releases/$PREV.txt" --previous-label "$PREV" \
        --label "$LABEL" "$@" "$OUT/perf-metrics.txt" >"$OUT/performance.md" ||
        fail "fx-bench perf-release could not make the performance table from $OUT/perf-metrics.txt"
    cp "$OUT/perf-metrics.txt" "$OUT/metrics-$LABEL.txt"
    cat "$OUT/performance.md"
}

if [ "$RESUME" -eq 1 ]; then
    # Every check comes before anything is written.
    [ -d "$DIR" ] ||
        fail "$DIR is not a release folder; start one with scripts/release-macos.sh NEW_OUT_DIR"
    OUT=$(cd "$DIR" && pwd)
    WORK=$OUT/work
    NOTARY=$WORK/for-notary
    INFO=$WORK/release-info
    [ -f "$NOTARY.json" ] ||
        fail "$OUT has no upload to resume ($NOTARY.json is missing); start a new release"
    [ -f "$INFO" ] ||
        fail "$OUT has no record of the build's channel ($INFO is missing); start a new release"
    info() { sed -n "s/^$1=//p" "$INFO" | head -n 1; }
    CHANNEL=$(info channel)
    NUMBER=$(info number)
    VERSION=$(info version)
    LABEL=$(info label)
    COMMIT=$(info commit)
    case $CHANNEL in
    release | rc) ;;
    beta) fail "$OUT holds a beta; betas are not notarized, so there is nothing to resume" ;;
    *) fail "$INFO names no channel; start a new release" ;;
    esac
    set_names
    APP=$WORK/$APP_NAME.app
    CLI=$WORK/cli/$CLI_NAME
    NOTARY_DMG=$WORK/for-notary-dmg
    if [ ! -d "$APP" ] || [ ! -f "$CLI" ]; then
        fail "the signed app or command is missing from $WORK; start a new release"
    fi
    # Once the disk image was uploaded, finish with that image, never a new
    # one; until then it is built after the app is stapled.
    if [ -f "$NOTARY_DMG.json" ]; then
        [ -f "$NOTARY_DMG.path" ] && DMG=$(cat "$NOTARY_DMG.path") && [ -f "$DMG" ] ||
            fail "the disk image uploaded to Apple is missing from $WORK (see $NOTARY_DMG.path); start a new release"
        RECORD=$NOTARY_DMG
        STAGE="the disk image's upload"
    else
        RECORD=$NOTARY
        STAGE="the app and command's upload"
    fi
    for f in "$OUT"/Not-Everything-*-macos-arm64.zip "$OUT"/Not-Everything-*-macos-arm64.dmg \
        "$OUT"/not-everything-*cli-*-macos-arm64.tar.gz "$OUT/SHA256SUMS.txt"; do
        [ ! -e "$f" ] || fail "$OUT already holds release files ($f); nothing to resume"
    done
    # The performance table is made again from this run's --perf, with the
    # fx-bench of the build.
    TARGET=$WORK/target
    need_perf "$CHANNEL"
    if [ -n "$PERF_DIR" ]; then
        check_perf "$COMMIT"
        check_previous_release
        [ -x "$TARGET/release/fx-bench" ] ||
            fail "$TARGET/release/fx-bench is missing, so the performance table can't be made; start a new release"
    fi
    APP_VERSION=$(plutil -extract CFBundleShortVersionString raw -o - \
        "$APP/Contents/Info.plist") || fail "no version in $APP"
    [ "$APP_VERSION" = "$VERSION" ] ||
        fail "$APP is version $APP_VERSION, but $INFO says $VERSION; start a new release"
    ID=$(plutil -extract id raw -o - "$RECORD.json") ||
        fail "no submission ID in $RECORD.json"
    [ "$PUBLISH" -eq 0 ] || check_publish_tools
    check_identity
    check_notices
    check_notary_profile
    check_requirement
    step "Resume $TITLE in $OUT at $STAGE (submission $ID)"
else
    need_perf "$MODE"
    VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)
    [ -n "$VERSION" ] || fail "no version in $ROOT/Cargo.toml"
    echo "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]?)\.(0|[1-9][0-9]?)$' ||
        fail "the Cargo version $VERSION is not X.Y.Z with the minor and patch parts below 100"

    git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1 || fail "$ROOT is not a git checkout"
    ORIGIN_URL=$(git -C "$ROOT" remote get-url origin 2>/dev/null) || ORIGIN_URL=
    # A wrong origin is refused before anything is fetched from it.
    [ "$PUBLISH" -eq 0 ] || check_origin
    # So is a performance run of anything but this commit, or one that failed.
    [ -z "$PERF_DIR" ] || check_perf "$(git -C "$ROOT" rev-parse HEAD)"
    if [ -n "$ORIGIN_URL" ]; then
        git -C "$ROOT" fetch --quiet --tags origin || fail "cannot fetch the tags from origin"
    fi
    BRANCH=$(git -C "$ROOT" symbolic-ref --quiet --short HEAD) ||
        fail "HEAD is not on a branch"
    case $MODE in
    release) [ "$BRANCH" = main ] || fail "a release builds from main; this is $BRANCH" ;;
    rc)
        [ "$BRANCH" = "rc/$VERSION" ] ||
            fail "--rc builds from rc/$VERSION, the branch of the Cargo version; this is $BRANCH"
        ;;
    beta) [ "$BRANCH" = dev ] || fail "--beta builds from dev; this is $BRANCH" ;;
    esac
    [ -z "$(git -C "$ROOT" status --porcelain)" ] ||
        fail "the working tree has uncommitted changes; commit or stash them first"
    if [ "$MODE" != beta ] &&
        git -C "$ROOT" rev-parse -q --verify "refs/tags/release/v$VERSION" >/dev/null; then
        fail "$VERSION is already released (tag release/v$VERSION); bump the version"
    fi

    CHANNEL=$MODE
    case $MODE in
    release)
        RC=$(highest "release/v$VERSION-RC-")
        [ "$RC" -gt 0 ] ||
            fail "no release candidate for $VERSION; make one with scripts/release-macos.sh --rc --publish"
        RC_TAG=release/v$VERSION-RC-$RC
        [ "$(git -C "$ROOT" rev-parse "$RC_TAG^{tree}")" = "$(git -C "$ROOT" rev-parse "HEAD^{tree}")" ] ||
            fail "main differs from the latest release candidate $RC_TAG; release only what was tested (merge the release PR, or make a new release candidate)"
        git -C "$ROOT" merge-base --is-ancestor "$RC_TAG" HEAD ||
            fail "$RC_TAG is not merged into main; merge the release PR first"
        [ -n "$(changelog_entry | tr -d '[:space:]')" ] ||
            fail "CHANGELOG.md has no entry for v$VERSION; add the release notes first"
        NUMBER=0
        LABEL=$VERSION
        ;;
    rc)
        CURRENT_RC=$(git -C "$ROOT" tag --points-at HEAD -l 'release/v*-RC-*' | head -n 1)
        [ -z "$CURRENT_RC" ] ||
            fail "HEAD is already release candidate $CURRENT_RC; commit the next change first"
        NUMBER=$(($(highest "release/v$VERSION-RC-") + 1))
        LABEL=$VERSION-RC-$NUMBER
        ;;
    beta)
        NUMBER=$(($(highest "beta/v$VERSION-beta-") + 1))
        LABEL=$VERSION-beta-$NUMBER
        ;;
    esac
    COMMIT=$(git -C "$ROOT" rev-parse HEAD)
    set_names
    # Once the tags are fetched.
    [ -z "$PERF_DIR" ] || check_previous_release
    MAJOR=${VERSION%%.*}
    MINOR_PATCH=${VERSION#*.}
    BUILD=$((MAJOR * 10000 + ${MINOR_PATCH%.*} * 100 + ${MINOR_PATCH#*.}))

    step "Plan"
    echo "Channel: $CHANNEL ($APP_NAME, $BUNDLE_ID, command $CLI_NAME)"
    echo "Version: $LABEL (build $BUILD)"
    echo "Commit:  $COMMIT on $BRANCH"
    echo "Origin:  ${ORIGIN_URL:-none}"
    [ "$CHANNEL" != release ] || echo "Release candidate: $RC_TAG"
    echo "Tag:     $TAG"
    echo "Disk image: $APP_DMG (volume \"$VOLUME\")"
    [ -z "$PERF_DIR" ] || echo "Performance: $PERF_DIR, compared with $PREV"

    [ ! -e "$DIR" ] || fail "$DIR already exists; pick a new folder"
    command -v cargo >/dev/null || fail "cargo is not on PATH"
    [ "$PUBLISH" -eq 0 ] || check_publish_tools
    check_identity
    security find-identity -v -p codesigning | grep -F "$IDENTITY" >/dev/null ||
        fail "no valid signing identity \"$IDENTITY\" in the keychain"
    check_notices
    [ "$CHANNEL" = beta ] || check_notary_profile

    # mkdir without -p fails on any existing entry, so an old folder is never
    # touched.
    mkdir "$DIR" || fail "$DIR already exists; pick a new folder"
    OUT=$(cd "$DIR" && pwd)
    WORK=$OUT/work
    mkdir "$WORK" "$WORK/cli" "$OUT/check"
    # What --resume needs to finish this build.
    {
        echo "channel=$CHANNEL"
        echo "number=$NUMBER"
        echo "version=$VERSION"
        echo "label=$LABEL"
        echo "tag=$TAG"
        echo "commit=$COMMIT"
    } >"$WORK/release-info"

    step "Build $LABEL in release"
    # A fresh target folder, so every binary is linked and stripped now.
    # rust-objcopy strips them and needs the toolchain's libLLVM.
    TARGET=$WORK/target
    DYLD_FALLBACK_LIBRARY_PATH="$(rustc --print sysroot)/lib"
    export DYLD_FALLBACK_LIBRARY_PATH
    # fx-channel reads the channel at compile time.
    NE_CHANNEL=$CHANNEL
    export NE_CHANNEL
    if [ "$CHANNEL" = release ]; then
        unset NE_CHANNEL_NUMBER
    else
        NE_CHANNEL_NUMBER=$NUMBER
        export NE_CHANNEL_NUMBER
    fi
    # The home folder becomes ~ in the binaries' source paths. --locked
    # builds exactly the crates in Cargo.lock, whose notices ship.
    build() {
        (cd "$ROOT" && RUSTFLAGS="--remap-path-prefix=$HOME=~" \
            cargo build --release --locked --target-dir "$TARGET" "$@") \
            >>"$WORK/build.log" 2>&1 ||
            { tail -n 30 "$WORK/build.log" >&2; fail "cargo build $* failed"; }
    }
    # fx-bench makes the performance table; it is not shipped.
    build -p fx-ui-macos -p fx-cli -p fx-bench --bins
    build -p fx-ui-macos --example make_bundle
    if grep -q "stripping debug info.*failed" "$WORK/build.log"; then
        fail "the binaries were not stripped; see $WORK/build.log"
    fi
    check_no_home "$TARGET/release/fx-ui-macos" "$TARGET/release/not-everything"

    step "Sign the app and the command with $IDENTITY"
    "$TARGET/release/examples/make_bundle" "$WORK" "$TARGET/release/fx-ui-macos" \
        --app --sign-identity "$IDENTITY"
    APP=$WORK/$APP_NAME.app
    CLI=$WORK/cli/$CLI_NAME
    [ -d "$APP" ] || fail "the build did not write $APP"
    cp "$TARGET/release/not-everything" "$CLI"
    codesign --force --options runtime --timestamp --identifier "$CLI_NAME" \
        --sign "$IDENTITY" "$CLI"
    codesign --verify --strict "$CLI"
    check_requirement

    NOTARY=$WORK/for-notary
    NOTARY_DMG=$WORK/for-notary-dmg
    if [ "$CHANNEL" = beta ]; then
        echo "A beta is signed but not notarized"
    else
        step "Notarize the app and the command in a first upload to Apple"
        mkdir "$NOTARY"
        ditto "$APP" "$NOTARY/$APP_NAME.app"
        cp "$CLI" "$NOTARY/$CLI_NAME"
        ditto -c -k --keepParent "$NOTARY" "$NOTARY.zip"
        upload "$NOTARY.zip" "$NOTARY"
    fi
fi

if [ "$CHANNEL" != beta ]; then
    # A resumed run whose disk image was uploaded skips to its wait.
    if [ ! -f "$NOTARY_DMG.json" ]; then
        step "Wait for Apple to notarize the app and the command"
        wait_for "$NOTARY"
        step "Check that Apple's ticket covers the signed app and command"
        check_ticket "$NOTARY" "$APP" "$CLI"
        step "Staple the ticket to the app"
        xcrun stapler staple "$APP"
        xcrun stapler validate "$APP"

        # Apple's order for nested containers: the disk image holds the
        # stapled app, so the app copied out of it carries its ticket.
        step "Make the disk image $APP_DMG from the stapled app and sign it"
        make_dmg
        step "Notarize the disk image in a second upload to Apple"
        # Its path first, so --resume finds the image Apple got.
        printf '%s\n' "$DMG" >"$NOTARY_DMG.path"
        upload "$DMG" "$NOTARY_DMG"
    fi
    step "Wait for Apple to notarize the disk image"
    wait_for "$NOTARY_DMG"
    step "Check that Apple's ticket covers the signed disk image"
    check_ticket "$NOTARY_DMG" "$DMG"
    step "Staple the ticket to the disk image"
    xcrun stapler staple "$DMG"
    xcrun stapler validate "$DMG"
else
    step "Make the disk image $APP_DMG and sign it"
    make_dmg
fi

step "Check a quarantined copy of the disk image, as a download arrives"
# cat copies the data alone, as a download does, so a stapled ticket must be
# inside the image itself.
CHECK_DMG=$OUT/check/$APP_DMG
cat "$DMG" >"$CHECK_DMG"
quarantine "$CHECK_DMG"
hdiutil verify "$CHECK_DMG" >"$WORK/verify.log" 2>&1 ||
    { cat "$WORK/verify.log" >&2; fail "hdiutil verify rejected the disk image"; }
codesign --verify --strict --verbose=2 "$CHECK_DMG"
if [ "$CHANNEL" = beta ]; then
    echo "A beta is not notarized; skipping spctl and stapler for the disk image"
else
    spctl --assess --type open --context context:primary-signature --verbose=4 "$CHECK_DMG" \
        >"$WORK/spctl-dmg.log" 2>&1 ||
        { cat "$WORK/spctl-dmg.log" >&2; fail "Gatekeeper rejected the disk image"; }
    cat "$WORK/spctl-dmg.log"
    grep -q "source=Notarized Developer ID" "$WORK/spctl-dmg.log" ||
        fail "Gatekeeper did not see the disk image as notarized"
    xcrun stapler validate "$CHECK_DMG"
fi
mkdir -p "$OUT/check/mount"
attach_image "$CHECK_DMG" -readonly -nobrowse -mountroot "$OUT/check/mount"
[ "$(readlink "$MOUNTED/Applications")" = /Applications ] ||
    fail "the disk image has no shortcut to /Applications"
check_app "$MOUNTED/$APP_NAME.app"
detach_image

# Before the files, so a failure here can still be resumed.
if [ -n "$PERF_DIR" ]; then
    step "Make the performance table for the release notes"
    perf_section
fi

step "Write the files"
ditto -c -k --norsrc --keepParent "$APP" "$OUT/$APP_ZIP"
cp "$DMG" "$OUT/$APP_DMG"
cp "$ROOT/LICENSE" "$ROOT/THIRD-PARTY-NOTICES.md" "$ROOT/docs/cli.md" "$WORK/cli/"
make_cli_tar "$OUT/$CLI_TAR" "$WORK/cli"
check_tarball "$OUT/$CLI_TAR"
(cd "$OUT" && shasum -a 256 "$APP_ZIP" "$APP_DMG" "$CLI_TAR" >SHA256SUMS.txt)

step "Check a quarantined copy of the zipped app, as a download arrives"
CHECK=$OUT/check/$APP_NAME.app
ditto -x -k "$OUT/$APP_ZIP" "$OUT/check"
quarantine "$CHECK"
check_app "$CHECK"

step "Done: the files are in $OUT"
ls -l "$OUT/$APP_DMG" "$OUT/$APP_ZIP" "$OUT/$CLI_TAR" "$OUT/SHA256SUMS.txt"
cat "$OUT/SHA256SUMS.txt"
if [ -n "$PERF_DIR" ]; then
    echo "The performance table in the release notes: $OUT/performance.md"
    if [ "$CHANNEL" = release ]; then
        echo "Commit $OUT/metrics-$LABEL.txt as docs/gates/releases/$VERSION.txt: the next release's table compares with these values"
    else
        echo "The measured values: $OUT/metrics-$LABEL.txt (at the release, the release's own are committed as docs/gates/releases/$VERSION.txt)"
    fi
fi

# The release notes: the version's CHANGELOG.md entry, after a note saying
# what a release candidate is and, with --perf, before the performance
# table, one blank line between them.
release_notes() {
    if [ "$CHANNEL" = rc ]; then
        echo "Release candidate $NUMBER of Not-Everything $VERSION, for testing. It installs as Not-Everything Preview, a separate app with its own settings, index and hotkey (Option-Shift-Space), next to the release app; its command is not-everything-preview."
        echo
    fi
    if [ -z "$PERF_DIR" ]; then
        changelog_entry
        return 0
    fi
    # The entry without its trailing blank lines.
    changelog_entry | awk 'NF == 0 { held = held $0 "\n"; next } { printf "%s", held; held = ""; print }'
    echo
    cat "$OUT/performance.md"
}

NOTES=$WORK/release-notes.md
[ "$CHANNEL" = beta ] || release_notes >"$NOTES"

# A shell word for $1, quoted only when it needs to be.
shell_word() {
    case $1 in
    '' | *[!A-Za-z0-9_./:=@+,-]*) printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")" ;;
    *) printf '%s' "$1" ;;
    esac
}

# Runs a tagging or publishing command with --publish; prints it otherwise.
publish_step() {
    if [ "$PUBLISH" -eq 1 ]; then
        "$@" || fail "$* failed"
    else
        printf ' '
        for word in "$@"; do
            printf ' %s' "$(shell_word "$word")"
        done
        printf '\n'
    fi
}

cd "$ROOT"
if [ "$PUBLISH" -eq 1 ]; then
    step "Tag $TAG and publish"
    # Never move or delete a tag or a release: stop if one exists.
    git cat-file -e "$COMMIT^{commit}" 2>/dev/null ||
        fail "the built commit $COMMIT is not in this checkout; nothing was tagged or published"
    if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
        fail "tag $TAG already exists; nothing was tagged or published"
    fi
    if git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
        fail "origin already has tag $TAG; nothing was tagged or published"
    fi
    [ "$CHANNEL" = beta ] || check_no_release "$TAG"
else
    step "Not tagged or published (no --publish); to publish, run in $ROOT"
fi
publish_step git tag -a "$TAG" -m "$TITLE" "$COMMIT"
publish_step git push origin "refs/tags/$TAG"
case $CHANNEL in
rc)
    publish_step gh release create "$TAG" --verify-tag --prerelease --title "$TITLE" --notes-file "$NOTES" \
        "$OUT/$APP_DMG" "$OUT/$APP_ZIP" "$OUT/$CLI_TAR" "$OUT/SHA256SUMS.txt"
    ;;
release)
    publish_step gh release create "$TAG" --verify-tag --latest --title "$TITLE" --notes-file "$NOTES" \
        "$OUT/$APP_DMG" "$OUT/$APP_ZIP" "$OUT/$CLI_TAR" "$OUT/SHA256SUMS.txt"
    ;;
beta) ;;
esac
