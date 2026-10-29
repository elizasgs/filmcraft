#!/usr/bin/env bash
# Build a local, ad-hoc signed "FilmCraft AI.app" from target/release, with filmcraft-cli inside so
# the Assistant panel can start its MCP bridge. It copies the Info.plist, icon and resources of an
# installed FilmCraft.app, and leaves that app alone.
#
#   cargo build --release -p filmcraft -p filmcraft-cli
#   packaging/macos/dev-ai-app.sh [DEST_DIR]        (default ~/Applications)
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SRC_APP="${SRC_APP:-/Applications/FilmCraft.app}"
DEST="${1:-$HOME/Applications}"
APP="$DEST/FilmCraft AI.app"
for f in "$ROOT/target/release/filmcraft" "$ROOT/target/release/filmcraft-cli" "$SRC_APP/Contents/Info.plist"; do
  [[ -e "$f" ]] || { echo "error: missing $f" >&2; exit 1; }
done
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources" 
cp "$SRC_APP/Contents/Info.plist" "$SRC_APP/Contents/PkgInfo" "$APP/Contents/"
cp -R "$SRC_APP/Contents/Resources/." "$APP/Contents/Resources/"
cp "$ROOT/target/release/filmcraft" "$APP/Contents/MacOS/FilmCraft"
cp "$ROOT/target/release/filmcraft-cli" "$APP/Contents/MacOS/filmcraft-cli"
/usr/libexec/PlistBuddy -c "Set :CFBundleDisplayName 'FilmCraft AI'" -c "Set :CFBundleName 'FilmCraft AI'" "$APP/Contents/Info.plist"
codesign --force --deep --sign - "$APP"
echo "built $APP"
