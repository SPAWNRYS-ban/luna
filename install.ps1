[CmdletBinding()]
param(
    [string]$Version = $(if ($env:LUNA_VERSION) { $env:LUNA_VERSION } else { "latest" }),
    [string]$Serial = $(if ($env:ADB_SERIAL) { $env:ADB_SERIAL } else { "" }),
    [string]$Repository = $(if ($env:LUNA_REPOSITORY) { $env:LUNA_REPOSITORY } else { "https://github.com/nyakokitsu/luna" }),
    [string]$RemoteRoot = $(if ($env:LUNA_REMOTE_ROOT) { $env:LUNA_REMOTE_ROOT } else { "/data/quazar/luna" }),
    [switch]$Uninstall
)
$ErrorActionPreference = "Stop"
function Require-Command([string]$Name) {
    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) { throw "$Name is required. Install Android Platform Tools and add adb to PATH." }
}
function Invoke-AdbShell([string]$Command) {
    $result = & adb -s $Serial shell sh -c $Command 2>&1
    if ($LASTEXITCODE -ne 0) { throw "adb shell failed: $result" }
    return ($result -join [Environment]::NewLine)
}
function Compact([string]$Value) {
    if ([string]::IsNullOrWhiteSpace($Value)) { return "" }
    return (($Value.ToLowerInvariant() -replace '[-_][a-z0-9]{4}$', '') -replace '[^a-z0-9]', '')
}
Require-Command "adb"
if ($RemoteRoot -notmatch '^[A-Za-z0-9_./-]+$') { throw "Invalid remote root" }
$devices = @((& adb devices) | Where-Object { $_ -match '^([^\s]+)\s+device$' } | ForEach-Object { $Matches[1] })
if (-not $Serial) {
    if ($devices.Count -eq 0) { throw "No ADB station found." }
    if ($devices.Count -eq 1) { $Serial = $devices[0] }
    else {
        for ($i = 0; $i -lt $devices.Count; $i++) { Write-Host ("{0}) {1}" -f ($i + 1), $devices[$i]) }
        $choice = [int](Read-Host "Station number")
        if ($choice -lt 1 -or $choice -gt $devices.Count) { throw "Invalid station number" }
        $Serial = $devices[$choice - 1]
    }
}
if ((& adb -s $Serial get-state 2>$null) -ne "device") { throw "ADB station is offline: $Serial" }
$url = if ($Version -eq "latest") { "$Repository/releases/latest/download" } else {
    if ($Version -notmatch '^v') { $Version = "v$Version" }
    "$Repository/releases/download/$Version"
}
$temp = Join-Path ([IO.Path]::GetTempPath()) ("luna-install-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temp | Out-Null
try {
    if ($Uninstall) {
        $remoteText = @'
set -eu
root="$LUNA_ROOT"; init=/etc/init.d/S99luna
[ -x "$init" ] && "$init" stop || true
rm -f "$init" "$root/bin/luna" "$root/bin/luna.previous" "$root/luna.pid"
echo "Luna removed"
'@
        Set-Content -Path "$temp/remote.sh" -Value $remoteText -Encoding ascii
        Get-Content "$temp/remote.sh" -Raw | & adb -s $Serial shell "LUNA_ROOT='$RemoteRoot' sh -s"
        if ($LASTEXITCODE -ne 0) { throw "Remote uninstall failed" }
        Write-Host "Luna removed from $Serial"
        return
    }
    Invoke-WebRequest "$url/manifest.json" -OutFile "$temp/manifest.json" -UseBasicParsing
    Invoke-WebRequest "$url/SHA256SUMS" -OutFile "$temp/SHA256SUMS" -UseBasicParsing
    $manifest = Get-Content "$temp/manifest.json" -Raw | ConvertFrom-Json
    $info = Invoke-AdbShell 'printf "__LUNA_HOSTNAME__"; uname -n; printf "__LUNA_ARCH__"; uname -m; printf "__LUNA_ABI__"; (getprop ro.product.cpu.abi 2>/dev/null || true); printf "__LUNA_MODEL__"; (getprop ro.product.model 2>/dev/null || true); printf "__LUNA_DT__"; (cat /proc/device-tree/model 2>/dev/null || true); printf "__LUNA_COMPAT__"; (cat /proc/device-tree/compatible 2>/dev/null | tr "\000" "\n" || true)'
    $readMarker = { param([string]$Name) [regex]::Match($info, "__${Name}__(.*?)(?=__LUNA_|$)", [Text.RegularExpressions.RegexOptions]::Singleline).Groups[1].Value.Trim([char]13, [char]10, [char]0, [char]32) }
    $hostname = & $readMarker 'HOSTNAME'
    $arch = & $readMarker 'ARCH'
    $abi = & $readMarker 'ABI'
    $model = & $readMarker 'MODEL'
    $dt = & $readMarker 'DT'
    $compatible = & $readMarker 'COMPAT'
    if ($arch -eq 'arm64-v8a') { $arch = 'aarch64' }
    if ($arch -eq 'armeabi-v7a') { $arch = 'armv7' }
    if (-not $arch -and $abi -eq 'arm64-v8a') { $arch = 'aarch64' }
    if (-not $arch -and $abi -eq 'armeabi-v7a') { $arch = 'armv7' }
    $values = @($hostname, $model, $dt, $compatible) | Where-Object { $_ }
    $ids = @($values | ForEach-Object { Compact $_ })
    $found = @($manifest.devices | Where-Object {
        $entry = $_
        $aliases = @($entry.device, $entry.codename, $entry.models) | Where-Object { $_ } | ForEach-Object { Compact $_ }
        $nameMatch = $false
        foreach ($alias in $aliases) { foreach ($id in $ids) { if ($alias -eq $id -or $alias.Contains($id) -or $id.Contains($alias)) { $nameMatch = $true } } }
        $archMatch = (($entry.target -like 'aarch64-*' -and $arch -in @('aarch64', 'arm64')) -or ($entry.target -like 'armv7-*' -and $arch -in @('armv7', 'armv7l', 'arm')))
        $nameMatch -and $archMatch
    })
    if ($found.Count -ne 1) { throw "Unsupported or ambiguous station. Detected: $($values -join ', ') (arch=$arch, abi=$abi). Raw adb info: $info" }
    $entry = $found[0]
    $asset = $entry.asset
    Write-Host "Detected $($entry.device) ($($entry.codename)) [$arch]"
    Invoke-WebRequest "$url/$asset" -OutFile "$temp/$asset" -UseBasicParsing
    $sumLine = Get-Content "$temp/SHA256SUMS" | Where-Object { $_ -match "\s$([regex]::Escape($asset))$" } | Select-Object -First 1
    if (-not $sumLine) { throw "No checksum found for $asset" }
    $expected = ($sumLine -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash "$temp/$asset" -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($expected -ne $actual) { throw "Checksum mismatch for $asset" }
    $init = @'
#!/bin/sh
# Managed by Luna
set -eu
root=/data/quazar/luna; binary=$root/bin/luna; pidfile=$root/luna.pid; logfile=$root/luna.log
case "$1" in
start)
  [ -x "$binary" ] || exit 0; mkdir -p "$root/data"
  if [ -f "$pidfile" ] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then exit 0; fi
  rm -f "$pidfile"
  if command -v start-stop-daemon >/dev/null 2>&1; then
    start-stop-daemon --start --background --make-pidfile --pidfile "$pidfile" --startas "$binary" -- --listen 0.0.0.0:8099 --data-dir "$root/data"
  else
    nohup "$binary" --listen 0.0.0.0:8099 --data-dir "$root/data" >"$logfile" 2>&1 </dev/null & echo $! >"$pidfile"
  fi ;;
stop) [ -f "$pidfile" ] && kill "$(cat "$pidfile")" 2>/dev/null || true; rm -f "$pidfile" ;;
restart) "$0" stop; "$0" start ;;
*) exit 2 ;;
esac
'@
    $remoteText = @'
set -eu
root="$LUNA_ROOT"; binary="$root/bin/luna"; init=/etc/init.d/S99luna
mount -o remount,rw / 2>/dev/null || true
mount | grep ' on / ' | grep -q '(rw' || { echo "root filesystem is not writable" >&2; exit 1; }
mkdir -p "$root/bin" "$root/data"; chmod 0755 "$root/bin/luna.new"
[ ! -f "$init" ] || grep -q '^# Managed by Luna$' "$init" || { echo "refusing unmanaged $init" >&2; exit 1; }
[ -x "$init" ] && "$init" stop || true
[ -f "$binary" ] && cp "$binary" "$root/bin/luna.previous"
mv "$root/bin/luna.new" "$binary"
cat >"$init" <<'INIT'
__INIT__
INIT
chmod 0755 "$init"; "$init" start
echo "Luna installed"
'@
    $remoteText = $remoteText.Replace("__INIT__", $init)
    Set-Content -Path "$temp/remote.sh" -Value $remoteText -Encoding ascii
    & adb -s $Serial push "$temp/$asset" "$RemoteRoot/bin/luna.new" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "adb push failed" }
    Get-Content "$temp/remote.sh" -Raw | & adb -s $Serial shell "LUNA_ROOT='$RemoteRoot' sh -s"
    if ($LASTEXITCODE -ne 0) { throw "Remote installation failed" }
    Write-Host "Luna installed on $Serial ($($entry.device)); API port 8099"
} finally {
    Remove-Item $temp -Recurse -Force -ErrorAction SilentlyContinue
}

