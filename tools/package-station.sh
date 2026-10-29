#!/bin/sh
# Pack one already-built station binary with the official YAM packer.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
model=${1:?Usage: package-station.sh MODEL OUTPUT.yampkg}
output=${2:?Usage: package-station.sh MODEL OUTPUT.yampkg}
case "$model" in
    yandexmini) triple=armv7-unknown-linux-musleabihf ;;
    yandexmicro|yandexmini_2|yandexmini3|yandexmidi|yandexplum|yandexstation_2)
        triple=aarch64-unknown-linux-musl ;;
    *) echo 'Unknown station model' >&2; exit 2 ;;
esac
binary=$repo/target/$triple/release/luna-$model
[ -f "$binary" ] || { echo "Build $binary first" >&2; exit 1; }
version=$(awk '$1 == "version:" { print $2; exit }' "$repo/yam.yaml")
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT HUP INT TERM
cp "$binary" "$stage/luna"
cp "$repo/tools/luna-service" "$stage/luna-service"
cat >"$stage/yam.yaml" <<EOF
name: yam/luna
version: $version
license: MIT
description: Luna integration build with YAM binary providers
targets: [$model]
files:
  bin/luna: luna
  bin/luna-service: luna-service
services:
  luna:
    exec: [bin/luna-service, run]
    restart: always
    after: [network]
EOF
set -- --platform "$model"
if [ -n "${YAM_PROFILES:-}" ]; then set -- "$@" --profiles "$YAM_PROFILES"; fi
"${YAM_BIN:-yam}" "$@" pack "$stage" --output "$output"
