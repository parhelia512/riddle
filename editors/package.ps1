$ErrorActionPreference = "Stop"

$dist = Join-Path $PSScriptRoot "dist"
$vscode = Join-Path $PSScriptRoot "vscode"
New-Item -ItemType Directory -Force $dist | Out-Null

Push-Location $vscode
try {
    npm ci
    if ($LASTEXITCODE -ne 0) { throw "npm ci failed" }

    npm run check
    if ($LASTEXITCODE -ne 0) { throw "npm run check failed" }

    npx --yes @vscode/vsce package --out (Join-Path $dist "riddle-vscode.vsix")
    if ($LASTEXITCODE -ne 0) { throw "VS Code packaging failed" }
} finally {
    Pop-Location
}

Compress-Archive -Force `
    -Path (Join-Path $PSScriptRoot "helix\*") `
    -DestinationPath (Join-Path $dist "riddle-helix.zip")

$zedFiles = "Cargo.lock", "Cargo.toml", "extension.toml", "languages", "src" |
    ForEach-Object { Join-Path (Join-Path $PSScriptRoot "zed") $_ }
Compress-Archive -Force `
    -Path $zedFiles `
    -DestinationPath (Join-Path $dist "riddle-zed.zip")

Get-ChildItem $dist -File | Select-Object Name, Length
