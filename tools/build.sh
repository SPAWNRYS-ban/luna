#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
MANIFEST="$ROOT/luna-manifest.json"
DEVICE_ID="${LUNA_DEVICE:-}"
DIST="${LUNA_DIST:-$ROOT/dist}"

usage() {
  cat <<'USAGE'
Build one Luna station binary without deploying it.

Usage:
  tools/build.sh
  tools/build.sh --device DEVICE_ID
  tools/build.sh --device DEVICE_ID --output PATH

Options:
  --device ID       Station entry from luna-manifest.json
  --output PATH     Output directory (default: ./dist)
USAGE
}

arrow_select() {
  local title=$1
  shift
  local items=("$@")
  local selected=0
  local tty=/dev/tty
  local saved
  saved=$(stty -g < "$tty")

  restore_terminal() {
    stty "$saved" < "$tty"
  }
  trap restore_terminal EXIT INT TERM
  stty -icanon -echo min 1 time 0 < "$tty"

  while :; do
    printf '\033[2J\033[H' > "$tty"
    printf '%s\n\n' "$title" > "$tty"
    local index
    for index in "${!items[@]}"; do
      if ((index == selected)); then
        printf '  \033[36m> %s\033[0m\n' "${items[index]}" > "$tty"
      else
        printf '    %s\n' "${items[index]}" > "$tty"
      fi
    done
    printf '\n↑/↓ выбрать, Enter подтвердить, q отмена\n' > "$tty"

    local key sequence
    IFS= read -r -n 1 key < "$tty"
    case "$key" in
      $'\x1b')
        IFS= read -r -n 2 sequence < "$tty"
        case "$sequence" in
          '[A') ((selected = (selected - 1 + ${#items[@]}) % ${#items[@]})) ;;
          '[B') ((selected = (selected + 1) % ${#items[@]})) ;;
        esac
        ;;
      '')
        printf '\033[2J\033[H' > "$tty"
        printf -v REPLY '%s' "${items[selected]}"
        trap - EXIT INT TERM
        stty "$saved" < "$tty"
        return 0
        ;;
      q|Q)
        trap - EXIT INT TERM
        stty "$saved" < "$tty"
        return 1
        ;;
    esac
  done
}

while (($#)); do
  case "$1" in
    --device)
      (($# >= 2)) || { echo "--device requires an ID" >&2; exit 2; }
      DEVICE_ID=$2
      shift 2
      ;;
    --output)
      (($# >= 2)) || { echo "--output requires a path" >&2; exit 2; }
      DIST=$2
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
    *)
      [[ -z "$DEVICE_ID" ]] || { echo "unexpected argument: $1" >&2; exit 2; }
      DEVICE_ID=$1
      shift
      ;;
  esac
done

command -v cargo >/dev/null 2>&1 || { echo "cargo is required" >&2; exit 1; }
command -v python3 >/dev/null 2>&1 || { echo "python3 is required" >&2; exit 1; }

if [[ -z "$DEVICE_ID" ]]; then
  device_items=()
  while IFS= read -r entry; do
    [[ -n "$entry" ]] && device_items+=("$entry")
  done < <(python3 - "$MANIFEST" <<'PY'
import json, sys
for entry in json.load(open(sys.argv[1], encoding="utf-8"))["devices"]:
    print(f"{entry['device']} — {', '.join(entry.get('models', []))}")
PY
  )
  ((${#device_items[@]} > 0)) || { echo "manifest has no stations" >&2; exit 1; }
  arrow_select "Выберите станцию для сборки:" "${device_items[@]}" || exit 1
  DEVICE_ID=${REPLY%% — *}
fi

[[ "$DEVICE_ID" =~ ^[A-Za-z0-9_.-]+$ ]] || {
  echo "invalid device ID" >&2
  exit 2
}

python3 - "$MANIFEST" "$DEVICE_ID" <<'PY'
import json, sys
manifest, wanted = sys.argv[1:]
entries = [x for x in json.load(open(manifest, encoding="utf-8")).get("devices", [])
           if x.get("device") == wanted]
if len(entries) != 1:
    raise SystemExit(f"unsupported or ambiguous station: {wanted}")
PY

mkdir -p "$DIST"
echo "Building $DEVICE_ID..."
"$ROOT/tools/build-stations.sh" "$MANIFEST" "$DIST" "$DEVICE_ID"

asset=$(python3 - "$DIST/manifest.json" <<'PY'
import json, sys
entries = json.load(open(sys.argv[1], encoding="utf-8"))["devices"]
if len(entries) != 1:
    raise SystemExit("build did not produce exactly one station asset")
print(entries[0]["asset"])
PY
)

echo "Built: $DIST/assets/$asset"
echo "SHA256: $DIST/SHA256SUMS"
