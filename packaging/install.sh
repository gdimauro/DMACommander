#!/usr/bin/env bash
# Put dmac on the PATH, and DMACommander in the Dock.
#
#   ./packaging/install.sh              # build, install, Dock, Desktop
#   ./packaging/install.sh --no-build   # skip cargo, just (re)install the launchers
#   ./packaging/install.sh --no-dock --no-desktop
#   ./packaging/install.sh --uninstall
#
# The launchers point at the release build inside this source tree rather than
# copying it, so `cargo build --release` is all it takes for the command, the
# Dock icon and the Desktop alias to pick up a new build.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
bin_dir="${DMAC_BIN_DIR:-$HOME/.local/bin}"
app_dir="${DMAC_APP_DIR:-$HOME/Applications}"
app="$app_dir/DMACommander.app"
desktop_link="$HOME/Desktop/DMACommander.app"
binary="$repo/target/release/dmac"

build=1 dock=1 desktop=1 uninstall=0
for arg in "$@"; do
  case "$arg" in
    --no-build)   build=0 ;;
    --no-dock)    dock=0 ;;
    --no-desktop) desktop=0 ;;
    --uninstall)  uninstall=1 ;;
    -h|--help)    sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

# --- Removing it again, because anything that edits your Dock owes you that. --
if [ "$uninstall" = 1 ]; then
  rm -f "$bin_dir/dmac" "$desktop_link"
  rm -rf "$app"
  if command -v python3 >/dev/null; then
    python3 - <<'PY' && killall Dock 2>/dev/null || true
import plistlib, subprocess
raw = subprocess.run(["defaults", "export", "com.apple.dock", "-"],
                     capture_output=True, check=True).stdout
d = plistlib.loads(raw)
apps = d.get("persistent-apps", [])
kept = [a for a in apps
        if "DMACommander.app" not in
        a.get("tile-data", {}).get("file-data", {}).get("_CFURLString", "")]
if len(kept) != len(apps):
    d["persistent-apps"] = kept
    subprocess.run(["defaults", "import", "com.apple.dock", "-"],
                   input=plistlib.dumps(d), check=True)
    print("removed from the Dock")
PY
  fi
  echo "removed. The source tree is untouched."
  exit 0
fi

[ "$build" = 1 ] && (cd "$repo" && cargo build --release -F gpu)
[ -x "$binary" ] || { echo "no release build at $binary — drop --no-build" >&2; exit 1; }

# --- The command. -----------------------------------------------------------
mkdir -p "$bin_dir"
cat > "$bin_dir/dmac" <<EOF
#!/bin/sh
# Installed by $repo/packaging/install.sh — edit that, not this.
binary="$binary"
if [ ! -x "\$binary" ]; then
  echo "dmac: not built. Run: (cd $repo && cargo build --release -F gpu)" >&2
  exit 127
fi
exec "\$binary" "\$@"
EOF
chmod +x "$bin_dir/dmac"
echo "installed $bin_dir/dmac"

case ":$PATH:" in
  *":$bin_dir:"*) ;;
  *) echo "note: $bin_dir is not on your PATH. Add it in ~/.zshrc:"
     echo "      export PATH=\"$bin_dir:\$PATH\"" ;;
esac

# --- The app bundle. --------------------------------------------------------
# A TUI has no window of its own, so the bundle's job is to open a terminal and
# run dmac inside it. Which terminal is a real choice: the ones listed first
# speak the kitty keyboard protocol, which is what makes Ctrl-Shift-C and the
# rest of the modern bindings arrive at all.
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$repo/packaging/icon.icns" "$app/Contents/Resources/dmac.icns"

version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$repo/Cargo.toml" | head -1)"
version="${version:-0.1.0}"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>       <string>DMACommander</string>
  <key>CFBundleIconFile</key>         <string>dmac</string>
  <key>CFBundleIdentifier</key>       <string>com.dimauro.dmacommander</string>
  <key>CFBundleName</key>             <string>DMACommander</string>
  <key>CFBundleDisplayName</key>      <string>DMACommander</string>
  <key>CFBundlePackageType</key>      <string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key>          <string>$version</string>
  <key>LSMinimumSystemVersion</key>   <string>11.0</string>
  <key>NSHighResolutionCapable</key>  <true/>
</dict>
</plist>
EOF

cat > "$app/Contents/MacOS/DMACommander" <<EOF
#!/bin/sh
# Open a terminal and run dmac in it. Set DMAC_TERMINAL to force one:
#   defaults write com.dimauro.dmacommander terminal iTerm
DMAC="$bin_dir/dmac"
EOF
cat >> "$app/Contents/MacOS/DMACommander" <<'EOF'

pick() {
  [ -n "${DMAC_TERMINAL:-}" ] && { echo "$DMAC_TERMINAL"; return; }
  forced=$(defaults read com.dimauro.dmacommander terminal 2>/dev/null) &&
    [ -n "$forced" ] && { echo "$forced"; return; }
  # Ordered by how completely they speak the keyboard: the first four report
  # Ctrl-Shift and the kitty protocol, the last two do not.
  for t in Ghostty kitty WezTerm Alacritty iTerm Terminal; do
    for d in /Applications "$HOME/Applications" /System/Applications/Utilities; do
      [ -d "$d/$t.app" ] && { echo "$t"; return; }
    done
  done
  echo Terminal
}

term=$(pick)
case "$term" in
  Ghostty)   exec open -na Ghostty   --args -e "$DMAC" ;;
  kitty)     exec open -na kitty     --args "$DMAC" ;;
  WezTerm)   exec open -na WezTerm   --args start -- "$DMAC" ;;
  Alacritty) exec open -na Alacritty --args -e "$DMAC" ;;
  iTerm|iTerm2)
    exec osascript \
      -e 'tell application "iTerm"' \
      -e '  activate' \
      -e '  set w to (create window with default profile)' \
      -e "  tell current session of w to write text \"exec '$DMAC'\"" \
      -e 'end tell' ;;
  *)
    exec osascript \
      -e 'tell application "Terminal"' \
      -e "  do script \"exec '$DMAC'\"" \
      -e '  activate' \
      -e 'end tell' ;;
esac
EOF
chmod +x "$app/Contents/MacOS/DMACommander"

# Make Launch Services notice the bundle, so the icon is right the first time
# rather than after a logout.
touch "$app"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
[ -x "$lsregister" ] && "$lsregister" -f "$app" >/dev/null 2>&1 || true
echo "installed $app"

# --- The Desktop, and the Dock. ---------------------------------------------
if [ "$desktop" = 1 ]; then
  ln -sfn "$app" "$desktop_link"
  echo "linked $desktop_link"
fi

if [ "$dock" = 1 ]; then
  python3 - "$app" <<'PY'
import plistlib, subprocess, sys
app = sys.argv[1]
raw = subprocess.run(["defaults", "export", "com.apple.dock", "-"],
                     capture_output=True, check=True).stdout
d = plistlib.loads(raw)
apps = d.get("persistent-apps", [])
here = lambda a: a.get("tile-data", {}).get("file-data", {}).get("_CFURLString", "")
if any("DMACommander.app" in here(a) for a in apps):
    print("already in the Dock")
else:
    apps.append({"tile-data": {"file-data": {"_CFURLString": app + "/",
                                             "_CFURLStringType": 0}},
                 "tile-type": "file-tile"})
    d["persistent-apps"] = apps
    subprocess.run(["defaults", "import", "com.apple.dock", "-"],
                   input=plistlib.dumps(d), check=True)
    subprocess.run(["killall", "Dock"])
    print("added to the Dock")
PY
fi

echo
echo "dmac is on your PATH; DMACommander is in ~/Applications, on the Desktop and in the Dock."
echo "Undo all of it with: $0 --uninstall"
