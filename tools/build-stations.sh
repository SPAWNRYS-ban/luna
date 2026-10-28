#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
manifest=${1:-"$root/luna-manifest.json"}
dist=${2:-"$root/dist"}
device=${3:-}

command -v cargo >/dev/null 2>&1 || { echo "cargo is required" >&2; exit 2; }
command -v python3 >/dev/null 2>&1 || { echo "python3 is required" >&2; exit 2; }

python3 - "$root" "$manifest" "$dist" "$device" <<'PY'
import hashlib, json, os, shutil, subprocess, sys
from pathlib import Path

root = Path(sys.argv[1])
manifest_path = Path(sys.argv[2])
dist = Path(sys.argv[3])
requested = sys.argv[4] or None
data = json.loads(manifest_path.read_text())
if data.get("schema_version") != 1 or not data.get("devices"):
    raise SystemExit("invalid luna manifest")

target_dir = Path(os.environ.get("CARGO_TARGET_DIR", root / "target"))
if not target_dir.is_absolute():
    target_dir = root / target_dir
assets = dist / "assets"
assets.mkdir(parents=True, exist_ok=True)
release = []
matched = False

for entry in data["devices"]:
    if requested and entry["device"] != requested:
        continue
    matched = True
    target, binary, asset = entry["target"], entry["binary"], entry["asset"]
    cargo_manifest = (root / entry["manifest_path"]).resolve()
    if not cargo_manifest.is_file() or root not in cargo_manifest.parents:
        raise SystemExit(f"invalid station manifest: {cargo_manifest}")
    if Path(asset).name != asset:
        raise SystemExit(f"asset must be a filename: {asset}")
    env = os.environ.copy()
    env["CARGO_PROFILE_RELEASE_STRIP"] = "symbols"
    env.setdefault("CARGO_TARGET_DIR", str(root / "target"))
    linker_names = {
        "aarch64-unknown-linux-musl": "aarch64-linux-musl-gcc",
        "armv7-unknown-linux-musleabihf": "arm-linux-musleabihf-gcc",
    }
    linker = shutil.which(linker_names.get(target, ""))
    if linker:
        env["CARGO_TARGET_" + target.upper().replace("-", "_") + "_LINKER"] = linker
    command = ["cargo", "build", "--release", "--locked", "--target", target,
               "--manifest-path", str(cargo_manifest), "--bin", binary]
    print(f"Building {entry['device']}: {' '.join(command)}", flush=True)
    subprocess.run(command, cwd=root, check=True, env=env)
    source = target_dir / target / "release" / binary
    if not source.is_file():
        raise SystemExit(f"missing build output: {source}")
    destination = assets / asset
    shutil.copy2(source, destination)
    destination.chmod(destination.stat().st_mode | 0o111)
    release.append({**entry, "filepath": str(destination.relative_to(dist)),
                    "sha256sum": hashlib.sha256(destination.read_bytes()).hexdigest()})

if requested and not matched:
    raise SystemExit(f"device not found: {requested}")
(dist / "manifest.json").write_text(json.dumps({"schema_version": 1, "devices": release}, indent=2) + "\n")
(dist / "SHA256SUMS").write_text("".join(
    f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
    for path in sorted(assets.iterdir()) if path.is_file()))
PY
