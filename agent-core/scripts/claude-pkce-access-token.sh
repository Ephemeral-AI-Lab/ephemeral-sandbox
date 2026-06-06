#!/usr/bin/env bash
set -euo pipefail

CLIENT_ID="9d1c250a-e61b-44d9-88ed-5944d1962f5e"
AUTHORIZE_URL="https://claude.ai/oauth/authorize"
TOKEN_URL="https://api.anthropic.com/v1/oauth/token"
REDIRECT_URI="https://console.anthropic.com/oauth/code/callback"
SCOPES="org:create_api_key user:profile user:inference"
BASE_URL="https://api.anthropic.com"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
AGENT_CORE_DIR="$(cd -- "$SCRIPT_DIR/.." && pwd)"
LOCAL_YML="$AGENT_CORE_DIR/config/local.yml"

OPEN_BROWSER=1
PRINT_TOKEN=0
WRITE_LOCAL=1
AUTH_CODE_STATE=""
CODE_PROVIDED=0

usage() {
  cat <<'EOF'
Usage:
  agent-core/scripts/claude-pkce-access-token.sh [options]

Generates a Claude PKCE authorization URL, exchanges the returned code#state for
a short-lived OAuth access token, and updates agent-core/config/local.yml by
default.

Options:
  --code <code#state>       Exchange an already copied authorization code.
  --no-open                 Print the URL but do not open the browser.
  --no-write-local          Do not update config/local.yml.
  --local-yml <path>        Override the local.yml path to update.
  --print-token             Print the access token to stdout.
  -h, --help                Show this help.

Default behavior:
  - opens the browser for the generated URL
  - prompts for code#state
  - writes providers.active=claude_coding_plan
  - writes providers.claude_coding_plan.access_token
  - does not print the token
EOF
}

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --code)
      AUTH_CODE_STATE="${2:-}"
      CODE_PROVIDED=1
      shift 2
      ;;
    --no-open)
      OPEN_BROWSER=0
      shift
      ;;
    --no-write-local)
      WRITE_LOCAL=0
      shift
      ;;
    --local-yml)
      LOCAL_YML="${2:-}"
      shift 2
      ;;
    --print-token)
      PRINT_TOKEN=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

need curl
need jq
need openssl
need base64

urlencode() {
  jq -rn --arg value "$1" '$value | @uri'
}

base64url() {
  base64 | tr '+/' '-_' | tr -d '=\n'
}

generate_verifier() {
  openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n'
}

challenge_for() {
  printf '%s' "$1" | openssl dgst -sha256 -binary | base64url
}

write_local_yml() {
  local token="$1"
  local target="$2"
  local dir tmp line stripped in_providers saw_providers saw_active inserted skip_claude
  dir="$(dirname -- "$target")"
  mkdir -p "$dir"
  tmp="$(mktemp "${target}.tmp.XXXXXX")"
  chmod 600 "$tmp"

  if [[ ! -f "$target" ]]; then
    {
      printf 'providers:\n'
      printf '  active: claude_coding_plan\n'
      printf '  claude_coding_plan:\n'
      printf '    base_url: %s\n' "$BASE_URL"
      printf '    access_token: %s\n' "$token"
    } >"$tmp"
    mv "$tmp" "$target"
    chmod 600 "$target"
    return
  fi

  in_providers=0
  saw_providers=0
  saw_active=0
  inserted=0
  skip_claude=0
  while IFS= read -r line || [[ -n "$line" ]]; do
    stripped="${line//[[:space:]]/}"
    if [[ -n "$stripped" && "$line" != [[:space:]]* ]]; then
      if [[ "$in_providers" -eq 1 && "$inserted" -eq 0 ]]; then
        if [[ "$saw_active" -eq 0 ]]; then
          printf '  active: claude_coding_plan\n' >>"$tmp"
          saw_active=1
        fi
        printf '  claude_coding_plan:\n' >>"$tmp"
        printf '    base_url: %s\n' "$BASE_URL" >>"$tmp"
        printf '    access_token: %s\n' "$token" >>"$tmp"
        inserted=1
      fi
      if [[ "$line" == "providers:" ]]; then
        in_providers=1
        saw_providers=1
        saw_active=0
      else
        in_providers=0
      fi
      skip_claude=0
    fi

    if [[ "$in_providers" -eq 1 && "$line" == "  active:"* ]]; then
      printf '  active: claude_coding_plan\n' >>"$tmp"
      saw_active=1
      continue
    fi

    if [[ "$in_providers" -eq 1 && "$line" == "  claude_coding_plan:" ]]; then
      printf '  claude_coding_plan:\n' >>"$tmp"
      printf '    base_url: %s\n' "$BASE_URL" >>"$tmp"
      printf '    access_token: %s\n' "$token" >>"$tmp"
      inserted=1
      skip_claude=1
      continue
    fi

    if [[ "$skip_claude" -eq 1 ]]; then
      if [[ -z "$stripped" || "$line" == "    "* ]]; then
        continue
      fi
      skip_claude=0
    fi

    printf '%s\n' "$line" >>"$tmp"
  done <"$target"

  if [[ "$saw_providers" -eq 0 ]]; then
    {
      printf '\nproviders:\n'
      printf '  active: claude_coding_plan\n'
      printf '  claude_coding_plan:\n'
      printf '    base_url: %s\n' "$BASE_URL"
      printf '    access_token: %s\n' "$token"
    } >>"$tmp"
  elif [[ "$in_providers" -eq 1 && "$inserted" -eq 0 ]]; then
    if [[ "$saw_active" -eq 0 ]]; then
      printf '  active: claude_coding_plan\n' >>"$tmp"
    fi
    printf '  claude_coding_plan:\n' >>"$tmp"
    printf '    base_url: %s\n' "$BASE_URL" >>"$tmp"
    printf '    access_token: %s\n' "$token" >>"$tmp"
  fi

  mv "$tmp" "$target"
  chmod 600 "$target"
}

VERIFIER="$(generate_verifier)"
CHALLENGE="$(challenge_for "$VERIFIER")"
AUTH_URL="${AUTHORIZE_URL}?code=true&client_id=$(urlencode "$CLIENT_ID")&response_type=code&redirect_uri=$(urlencode "$REDIRECT_URI")&scope=$(urlencode "$SCOPES")&code_challenge=$(urlencode "$CHALLENGE")&code_challenge_method=S256&state=$(urlencode "$VERIFIER")"

if [[ -z "$AUTH_CODE_STATE" ]]; then
  echo "Open this URL and authorize Claude:"
  echo "$AUTH_URL"
  if [[ "$OPEN_BROWSER" -eq 1 ]] && command -v open >/dev/null 2>&1; then
    open "$AUTH_URL" >/dev/null 2>&1 || true
  fi
  printf 'Paste code#state: ' >&2
  IFS= read -r AUTH_CODE_STATE
fi

if [[ "$AUTH_CODE_STATE" != *"#"* ]]; then
  echo "authorization value must be code#state" >&2
  exit 1
fi

CODE="${AUTH_CODE_STATE%%#*}"
STATE="${AUTH_CODE_STATE#*#}"
if [[ "$CODE_PROVIDED" -eq 1 ]]; then
  VERIFIER="$STATE"
elif [[ "$STATE" != "$VERIFIER" ]]; then
  echo "state mismatch; the pasted code does not belong to this PKCE run" >&2
  exit 1
fi

PAYLOAD="$(
  jq -n \
    --arg grant_type "authorization_code" \
    --arg client_id "$CLIENT_ID" \
    --arg code "$CODE" \
    --arg state "$STATE" \
    --arg redirect_uri "$REDIRECT_URI" \
    --arg code_verifier "$VERIFIER" \
    '{
      grant_type: $grant_type,
      client_id: $client_id,
      code: $code,
      state: $state,
      redirect_uri: $redirect_uri,
      code_verifier: $code_verifier
    }'
)"

RESPONSE_FILE="$(mktemp)"
trap 'rm -f "$RESPONSE_FILE"' EXIT
HTTP_CODE="$(
  printf '%s' "$PAYLOAD" | curl -sS -o "$RESPONSE_FILE" -w '%{http_code}' \
    -H 'content-type: application/json' \
    -H 'accept: application/json' \
    --data @- \
    "$TOKEN_URL"
)"

if [[ "$HTTP_CODE" != 2* ]]; then
  echo "token exchange failed with HTTP $HTTP_CODE" >&2
  jq -r '.error_description // .error.message // .error // .' "$RESPONSE_FILE" >&2 || cat "$RESPONSE_FILE" >&2
  exit 1
fi

ACCESS_TOKEN="$(jq -r '.access_token // empty' "$RESPONSE_FILE")"
REFRESH_TOKEN="$(jq -r '.refresh_token // empty' "$RESPONSE_FILE")"
EXPIRES_IN="$(jq -r '.expires_in // empty' "$RESPONSE_FILE")"
if [[ -z "$ACCESS_TOKEN" ]]; then
  echo "token response did not include access_token" >&2
  exit 1
fi

if [[ "$WRITE_LOCAL" -eq 1 ]]; then
  write_local_yml "$ACCESS_TOKEN" "$LOCAL_YML"
  echo "updated $LOCAL_YML with providers.active=claude_coding_plan"
fi

if [[ "$PRINT_TOKEN" -eq 1 ]]; then
  printf '%s\n' "$ACCESS_TOKEN"
else
  echo "access_token acquired (redacted)"
fi

if [[ -n "$REFRESH_TOKEN" ]]; then
  echo "refresh_token received (redacted; not stored)"
fi
if [[ -n "$EXPIRES_IN" ]]; then
  echo "expires_in: $EXPIRES_IN seconds"
fi
