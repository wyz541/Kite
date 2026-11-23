# Creates a small repository with one file in each Git state so the Kite
# preview's Git section can be checked by hand.
$ErrorActionPreference = 'Stop'
$root = Join-Path $env:TEMP 'kite-git-demo'
if (Test-Path $root) { Remove-Item -LiteralPath $root -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $root 'src') | Out-Null
Push-Location $root
try {
    git init -q -b main
    git config user.email demo@kite.local
    git config user.name 'Kite Demo'
    git config core.autocrlf false
    Set-Content -NoNewline src/main.rs "fn main() {`n    println!(`"hello`");`n}`n"
    Set-Content -NoNewline README.md "# Demo`n`nOriginal line.`n"
    Set-Content -NoNewline clean.txt "clean file`n"
    Set-Content -NoNewline .gitignore "target/`n"
    git add .
    git commit -qm 'Initial commit'
    # Modified in the work tree only.
    Set-Content -NoNewline src/main.rs "fn main() {`n    println!(`"hello, kite`");`n    println!(`"git preview`");`n}`n"
    # Staged change.
    Set-Content -NoNewline README.md "# Demo`n`nStaged change.`n"
    git add README.md
    # Untracked and ignored files.
    Set-Content -NoNewline src/new.rs "brand new`n"
    New-Item -ItemType Directory -Path target | Out-Null
    Set-Content -NoNewline target/build.log 'x'
    git status --short
    Write-Host "Demo repository: $root"
} finally {
    Pop-Location
}
