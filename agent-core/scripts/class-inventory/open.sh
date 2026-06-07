#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
AGENT_CORE_DIR="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
OUTPUT_DIR="$AGENT_CORE_DIR/docs/class-inventory"

BIND="${CLASS_INVENTORY_BIND:-127.0.0.1}"
PORT="${CLASS_INVENTORY_PORT:-8787}"
BROWSER_APP="${CLASS_INVENTORY_BROWSER:-Google Chrome}"
PAGE="${1:-${CLASS_INVENTORY_PAGE:-index.html}}"
BASE_URL="http://${BIND}:${PORT}"
LOG_FILE="${CLASS_INVENTORY_LOG:-${TMPDIR:-/tmp}/agent-core-class-inventory-${PORT}.log}"

usage() {
  cat <<'EOF'
Usage:
  agent-core/scripts/class-inventory/open.sh [page]

Starts the class-inventory refresh server if needed and opens it in Chrome.

Arguments:
  page    Optional page under agent-core/docs/class-inventory.
          Defaults to index.html.

Examples:
  agent-core/scripts/class-inventory/open.sh
  agent-core/scripts/class-inventory/open.sh crates/eos-engine.html

Environment:
  CLASS_INVENTORY_BIND       Bind address, default 127.0.0.1.
  CLASS_INVENTORY_PORT       Port, default 8787.
  CLASS_INVENTORY_BROWSER    macOS browser app, default Google Chrome.
  CLASS_INVENTORY_LOG        Server log path.
EOF
}

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

case "$PAGE" in
  -h | --help)
    usage
    exit 0
    ;;
  "$OUTPUT_DIR"/*)
    PAGE="${PAGE#"$OUTPUT_DIR"/}"
    ;;
  file://"$OUTPUT_DIR"/*)
    PAGE="${PAGE#"file://$OUTPUT_DIR/"}"
    ;;
  /*)
    echo "page must be relative to $OUTPUT_DIR: $PAGE" >&2
    exit 1
    ;;
esac

need curl
need python3

if [[ ! -f "$OUTPUT_DIR/$PAGE" ]]; then
  echo "inventory page not found: $OUTPUT_DIR/$PAGE" >&2
  exit 1
fi

url="${BASE_URL}/${PAGE}"

server_ready() {
  curl -fsS "${BASE_URL}/index.html" 2>/dev/null | grep -q "Generated Rust source inventory"
}

start_server() {
  mkdir -p "$(dirname -- "$LOG_FILE")"
  nohup python3 "$SCRIPT_DIR/serve.py" --bind "$BIND" --port "$PORT" >"$LOG_FILE" 2>&1 &
  local pid="$!"
  for _ in {1..40}; do
    if server_ready; then
      echo "started class-inventory server pid=$pid log=$LOG_FILE"
      return
    fi
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "class-inventory server failed to start; log follows:" >&2
      tail -n 40 "$LOG_FILE" >&2 || true
      exit 1
    fi
    sleep 0.25
  done
  echo "class-inventory server did not become ready; log follows:" >&2
  tail -n 40 "$LOG_FILE" >&2 || true
  exit 1
}

if server_ready; then
  echo "reusing class-inventory server at $BASE_URL"
else
  start_server
fi

if command -v open >/dev/null 2>&1; then
  if ! open -a "$BROWSER_APP" "$url" 2>/dev/null; then
    open "$url"
  fi
elif command -v xdg-open >/dev/null 2>&1; then
  xdg-open "$url" >/dev/null 2>&1 &
else
  echo "open this URL: $url"
  exit 0
fi

echo "opened $url"
