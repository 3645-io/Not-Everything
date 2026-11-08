#!/bin/sh
# App icon builder.
#
# Renders the six approved app icon designs (the owner's private design
# canvas "Search App Icon Concepts", page Green Rim, "Approved set", Glass)
# and compiles them with Xcode's actool, writing the checked-in icon files
# of fx-ui-macos:
#
#   crates/fx-ui-macos/assets/AppIcon.icon/   Icon Composer source of the app
#                                             icon: Match Row, with its Dark
#                                             artwork for the dark appearance
#   crates/fx-ui-macos/assets/bundle/         AppIcon.icns (macOS 13 to 15) and
#                                             Assets.car (macOS 26 and later),
#                                             compiled from AppIcon.icon;
#                                             make_bundle --app copies them
#                                             into the .app
#   crates/fx-ui-macos/assets/icons/          <design>-<light|dark>.png, 256 px:
#                                             each design as macOS renders it
#                                             (masked, with its edge and
#                                             shadow), for the Settings window
#                                             and the Dock icon the user picks
#
# Steps: scripts/icons/neu-icon.mjs writes each design's foreground (every
# part above the ground, on a transparent 1024 x 1024 page) for the light
# and dark appearances, plus the ground colours; headless Chrome screenshots
# each page to a PNG layer; actool compiles icons whose fill is the ground
# colour and whose one layer is the foreground. With the ground and the
# parts apart, macOS draws the clear and tinted styles the way it draws its
# own apps': a see-through tile with translucent parts. Icon Composer has no
# per-size artwork, so the canvas's tuned 16 and 32 px artwork is not used.
#
# Needs macOS with Xcode 26 or later (actool with .icon support), Node and
# Google Chrome (set CHROME to use another Chrome binary). Run it from the
# repository root after the approved designs change, then commit the
# results. Work files go to a new temporary directory, which is left in
# place and printed at the end.

set -eu

CHROME=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
ASSETS=crates/fx-ui-macos/assets
DEFAULT_ICON=match-row
DESIGNS="soft-lens pressed-lens match-row glow-row front-stack lit-slot"

if [ ! -f scripts/icons/neu-icon.mjs ] || [ ! -d "$ASSETS" ]; then
    echo "build-icons.sh: run from the repository root" >&2
    exit 2
fi
for tool in node xcrun iconutil; do
    command -v "$tool" >/dev/null || { echo "build-icons.sh: $tool not found" >&2; exit 2; }
done
[ -x "$CHROME" ] || { echo "build-icons.sh: Chrome not found at $CHROME (set CHROME)" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/fx-icons.XXXXXX")
echo "work directory: $work"

# 1. Each design's foreground per appearance, then a 1024 px PNG of each.
node scripts/icons/neu-icon.mjs "$work/pages"
for page in "$work"/pages/*.html; do
    "$CHROME" --headless=new --disable-gpu --hide-scrollbars \
        --force-device-scale-factor=1 --window-size=1024,1024 \
        --default-background-color=00000000 \
        --screenshot="${page%.html}.png" "file://$page" >/dev/null 2>&1
    [ -s "${page%.html}.png" ] || { echo "build-icons.sh: no screenshot of $page" >&2; exit 1; }
done

# The ground colour of page $1 (for example match-row-dark) as Icon
# Composer writes a colour: srgb:<r>,<g>,<b>,<alpha> from 0 to 1.
fill() {
    hex=$(awk -v page="$1" '$1 == page { print substr($2, 2) }' "$work/pages/fills.txt")
    [ ${#hex} -eq 6 ] || { echo "build-icons.sh: no ground colour for $1" >&2; exit 1; }
    awk -v r=$((0x$(echo "$hex" | cut -c1-2))) -v g=$((0x$(echo "$hex" | cut -c3-4))) \
        -v b=$((0x$(echo "$hex" | cut -c5-6))) \
        'BEGIN { printf "srgb:%.5f,%.5f,%.5f,1.00000", r / 255, g / 255, b / 255 }'
}

# Writes an Icon Composer icon.json to $1: the ground colour as the fill and
# one foreground layer, with no glass, translucency or extra shadow, so the
# approved artwork shows as drawn. $2 is the light page (its PNG must be in
# the icon's Assets); $3, when given, the page for the dark appearance.
icon_json() {
    if [ $# -ge 3 ]; then
        fills="\"fill-specializations\" : [
    { \"value\" : { \"solid\" : \"$(fill "$2")\" } },
    { \"appearance\" : \"dark\", \"value\" : { \"solid\" : \"$(fill "$3")\" } }
  ],"
        images="\"image-name-specializations\" : [
            { \"value\" : \"$2.png\" },
            { \"appearance\" : \"dark\", \"value\" : \"$3.png\" }
          ],"
    else
        fills="\"fill\" : { \"solid\" : \"$(fill "$2")\" },"
        images="\"image-name\" : \"$2.png\","
    fi
    cat >"$1" <<EOF
{
  $fills
  "groups" : [
    {
      "layers" : [
        {
          $images
          "name" : "foreground",
          "glass" : false
        }
      ],
      "shadow" : { "kind" : "none", "opacity" : 0 },
      "translucency" : { "enabled" : false, "value" : 0 }
    }
  ],
  "supported-platforms" : { "squares" : [ "macOS" ] }
}
EOF
}

# Compiles the .icon package $1 into the directory $2 (AppIcon.icns,
# Assets.car). actool compiles only the icon named by --app-icon, so every
# package is called AppIcon.icon.
compile() {
    mkdir -p "$2"
    xcrun actool --compile "$2" --platform macosx --minimum-deployment-target 13.0 \
        --app-icon AppIcon --output-partial-info-plist "$2/partial.plist" \
        --errors --warnings "$1" >"$2/actool.log" 2>&1 \
        || { cat "$2/actool.log" >&2; exit 1; }
    [ -s "$2/AppIcon.icns" ] && [ -s "$2/Assets.car" ] \
        || { cat "$2/actool.log" >&2; echo "build-icons.sh: actool wrote no icon for $1" >&2; exit 1; }
}

# 2. The app icon: Match Row, light and dark.
icon="$work/AppIcon.icon"
mkdir -p "$icon/Assets"
cp "$work/pages/$DEFAULT_ICON-light.png" "$work/pages/$DEFAULT_ICON-dark.png" "$icon/Assets/"
icon_json "$icon/icon.json" "$DEFAULT_ICON-light" "$DEFAULT_ICON-dark"
compile "$icon" "$work/bundle"

# 3. Every design and appearance as macOS renders it: compile it as the only
# artwork of an icon and take the 256 px image from the .icns.
mkdir -p "$work/icons"
for design in $DESIGNS; do
    for look in light dark; do
        dir="$work/runtime/$design-$look"
        mkdir -p "$dir/AppIcon.icon/Assets"
        cp "$work/pages/$design-$look.png" "$dir/AppIcon.icon/Assets/"
        icon_json "$dir/AppIcon.icon/icon.json" "$design-$look"
        compile "$dir/AppIcon.icon" "$dir/out"
        iconutil -c iconset -o "$dir/AppIcon.iconset" "$dir/out/AppIcon.icns"
        cp "$dir/AppIcon.iconset/icon_128x128@2x.png" "$work/icons/$design-$look.png"
    done
done

# 4. Copy the results into the crate.
mkdir -p "$ASSETS/AppIcon.icon/Assets" "$ASSETS/bundle" "$ASSETS/icons"
cp "$icon/icon.json" "$ASSETS/AppIcon.icon/icon.json"
cp "$icon/Assets/"*.png "$ASSETS/AppIcon.icon/Assets/"
cp "$work/bundle/AppIcon.icns" "$work/bundle/Assets.car" "$ASSETS/bundle/"
cp "$work/icons/"*.png "$ASSETS/icons/"
echo "wrote $ASSETS/AppIcon.icon, $ASSETS/bundle and $ASSETS/icons"
