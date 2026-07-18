#!/usr/bin/env bash
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
dist="$root/dist"
mkdir -p "$dist"

command -v zip >/dev/null || { echo "zip is required" >&2; exit 1; }
rm -f "$dist/riddle-vscode.vsix" "$dist/riddle-helix.zip" "$dist/riddle-zed.zip"

(
    cd "$root/vscode"
    npm ci
    npm run check
    npx --yes @vscode/vsce package --out "$dist/riddle-vscode.vsix"
)

(
    cd "$root/helix"
    zip -qr "$dist/riddle-helix.zip" languages.toml runtime
)

(
    cd "$root/zed"
    zip -qr "$dist/riddle-zed.zip" Cargo.lock Cargo.toml extension.toml languages src
)

ls -lh "$dist/riddle-vscode.vsix" "$dist/riddle-helix.zip" "$dist/riddle-zed.zip"
