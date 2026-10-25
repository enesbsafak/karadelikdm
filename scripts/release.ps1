# Karadelik Download Manager sürüm paketi: kurulum programını (NSIS) derler, dist\ altında setup, SHA256SUMS.txt
# ve latest.json üretir. latest.json uygulama içi güncelleyicinin okuduğu dosyadır; GitHub sürümüne setup ile
# birlikte yüklenmelidir.
#
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1            # derle + paketle
#   powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -NoBuild   # mevcut derlemeyi paketle
#
# Sürüm notları: docs\releases\<sürüm>.md (GitHub sürüm metni).
param([switch]$NoBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = ([regex]::Match((Get-Content Cargo.toml -Raw), '(?m)^version\s*=\s*"([^"]+)"')).Groups[1].Value
$pkg = (Get-Content package.json -Raw | ConvertFrom-Json).version
$conf = (Get-Content src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
if ($version -ne $pkg -or $version -ne $conf) { throw "Sürüm uyuşmuyor: Cargo.toml=$version package.json=$pkg tauri.conf.json=$conf" }
$notesFile = "docs\releases\$version.md"
if (-not (Test-Path $notesFile)) { throw "Sürüm notu yok: $notesFile" }

if (-not $NoBuild) {
    # Uygulama, paketlenmemiş uzantıyı kaynak olarak taşır (Tercihler > Tarayıcı > Uzantıyı kur); derleme bunu ister
    node extension/build.mjs
    if ($LASTEXITCODE -ne 0) { throw 'Uzantı derlenemedi' }
    cargo test --workspace --quiet
    if ($LASTEXITCODE -ne 0) { throw 'Testler başarısız' }
    # Native messaging host, kurulumda ana exe'nin yanına konur (tauri.conf.json > bundle.externalBin)
    cargo build --release -p kdm-nmh
    if ($LASTEXITCODE -ne 0) { throw 'karadelikdm-nmh derlenemedi' }
    New-Item -ItemType Directory -Force src-tauri\binaries | Out-Null
    Copy-Item target\release\karadelikdm-nmh.exe src-tauri\binaries\karadelikdm-nmh-x86_64-pc-windows-msvc.exe -Force
    npm run build
    if ($LASTEXITCODE -ne 0) { throw 'Derleme başarısız' }
}

$built = Get-ChildItem "target\release\bundle\nsis\*_${version}_x64-setup.exe" | Select-Object -First 1
if (-not $built) { throw "Kurulum programı yok: target\release\bundle\nsis\*_${version}_x64-setup.exe" }
$name = "KaradelikDM-$version-setup.exe"
$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
Get-ChildItem $dist -File | Remove-Item -Force
Copy-Item $built.FullName "$dist\$name"

$setupHash = (Get-FileHash "$dist\$name" -Algorithm SHA256).Hash.ToLower()
$exeHash = (Get-FileHash target\release\karadelikdm.exe -Algorithm SHA256).Hash.ToLower()
$utf8 = New-Object System.Text.UTF8Encoding $false
[IO.File]::WriteAllText("$dist\SHA256SUMS.txt", "$setupHash $name`n$exeHash karadelikdm.exe`n", $utf8)

$latest = [ordered]@{
    version   = $version
    platforms = [ordered]@{
        'windows-x86_64' = [ordered]@{
            url    = "https://github.com/enesbsafak/karadelikdm/releases/download/v$version/$name"
            sha256 = $setupHash
        }
    }
}
[IO.File]::WriteAllText("$dist\latest.json", ($latest | ConvertTo-Json -Depth 4), $utf8)
Copy-Item $notesFile "$dist\RELEASE_NOTES.md"

Write-Host "Hazır: dist\$name, dist\SHA256SUMS.txt, dist\latest.json"
Write-Host "Önce main'i push edin, sonra: gh release create v$version dist\$name dist\SHA256SUMS.txt dist\latest.json --title `"Karadelik Download Manager $version`" --notes-file $notesFile"
