#!/usr/bin/env bash
#
# Installer for the resident Tachi HTTP service.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v2.1.0/scripts/install.sh | bash
#   bash install.sh --version latest
#
set -euo pipefail

VERSION="${TACHI_VERSION:-latest}"
SKIP_BREW=0
DAEMON_SERVICE="${TACHI_DAEMON_SERVICE:-1}"
DAEMON_PORT="${TACHI_DAEMON_PORT:-6919}"

print_help() {
  echo "Usage: install.sh [options]"
  echo "  --version latest    Install the current Homebrew formula (only supported value)"
  echo "  --dir <path>        Retired OpenClaw plugin option (refused)"
  echo "  --skip-brew         Skip installing/updating the Tachi Homebrew package"
  echo "  --skip-plugin       Accepted for compatibility; plugin installation is retired"
  echo "  --daemon-service    Install/restart the user launchd daemon service (default on macOS)"
  echo "  --skip-daemon-service"
  echo "                       Do not install/restart the daemon service"
  echo "  -h, --help          Show this help"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      if [[ $# -lt 2 || "$2" == --* ]]; then
        echo "--version requires a value; this Homebrew installer supports only latest." >&2
        exit 1
      fi
      VERSION="$2"; shift 2
      ;;
    --dir) echo "OpenClaw MCP plugin installation is retired; use a native Tachi HTTP client. No installation or configuration was changed." >&2; exit 1 ;;
    --skip-brew) SKIP_BREW=1; shift ;;
    --skip-plugin) shift ;;
    --daemon-service) DAEMON_SERVICE=1; shift ;;
    --skip-daemon-service) DAEMON_SERVICE=0; shift ;;
    -h|--help)
      print_help
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      print_help
      exit 1
      ;;
  esac
done

if [ "$VERSION" != "latest" ]; then
  echo "This Homebrew installer supports only --version latest (the current tap formula)." >&2
  echo "Requested version: $VERSION. Specific release pins are unsupported; no installation or configuration was changed." >&2
  exit 1
fi

if [[ ! "$DAEMON_PORT" =~ ^[0-9]+$ || ${#DAEMON_PORT} -gt 5 ]]; then
  echo "Invalid TACHI_DAEMON_PORT: expected a fixed port from 1 to 65535 (default: 6919)." >&2
  exit 1
fi
DAEMON_PORT=$((10#$DAEMON_PORT))
if (( DAEMON_PORT < 1 || DAEMON_PORT > 65535 )); then
  echo "Invalid TACHI_DAEMON_PORT: expected a fixed port from 1 to 65535 (default: 6919)." >&2
  exit 1
fi

DAEMON_SERVICE_NORMALIZED=$(printf '%s' "$DAEMON_SERVICE" | tr '[:upper:]' '[:lower:]')
case "$DAEMON_SERVICE_NORMALIZED" in
  1|true|yes|on) DAEMON_SERVICE=1 ;;
  0|false|no|off) DAEMON_SERVICE=0 ;;
  *)
    echo "Invalid TACHI_DAEMON_SERVICE value: $DAEMON_SERVICE (expected true/false)"
    exit 1
    ;;
esac

echo "========================================================="
echo "🧠 Installing Tachi HTTP Service"
echo "========================================================="
echo ""

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "❌ Required command not found: $1"
    exit 1
  fi
}

if [ "$SKIP_BREW" -eq 0 ]; then
  require_cmd brew
fi

install_tachi_brew() {
  echo ""
  echo ">> Installing/updating Tachi via Homebrew..."
  brew tap kckylechen1/tachi >/dev/null

  if brew list tachi >/dev/null 2>&1; then
    if brew upgrade tachi; then
      :
    else
      echo "   brew upgrade returned non-zero, attempting reinstall..."
      brew reinstall tachi
    fi
  else
    brew install tachi
  fi

  echo "   Installed binary: $(command -v tachi || echo 'not on PATH yet')"
  if command -v tachi >/dev/null 2>&1; then
    echo "   Tachi version: $(tachi --version || true)"
  fi
}

xml_escape() {
  local value="$1"
  value=${value//&/&amp;}
  value=${value//</&lt;}
  value=${value//>/&gt;}
  value=${value//\"/&quot;}
  value=${value//\'/&apos;}
  printf '%s' "$value"
}

launchctl_bootout_plist() {
  local plist="$1"
  local label="$2"
  local uid
  uid=$(id -u)
  launchctl bootout "gui/$uid" "$plist" >/dev/null 2>&1 || true
  launchctl remove "$label" >/dev/null 2>&1 || true
  launchctl unload "$plist" >/dev/null 2>&1 || true
}

launchctl_bootstrap_plist() {
  local plist="$1"
  local label="$2"
  local uid
  uid=$(id -u)
  if ! launchctl bootstrap "gui/$uid" "$plist" >/dev/null 2>&1; then
    launchctl load "$plist" >/dev/null
  fi
}

# The top-level fields of the status of whatever holds this DB scope, one
# `name<TAB>type<TAB>value` line each, or nothing. Nothing means the binary
# did not answer: it failed, or what it printed is not one JSON object.
daemon_status_for_scope() {
  local tachi_bin="$1"
  local global_db="$2"

  # The output reaches the decoder as the binary wrote it. Kept in a shell
  # variable first, it would lose a NUL byte, and a broken object could become
  # a whole one. Here a NUL becomes a byte the decoder refuses.
  TACHI_DISABLE_AUTO_DAEMON=1 \
    "$tachi_bin" --global-db "$global_db" --no-project-db daemon status --json 2>/dev/null |
    tr '\000' '\001' | json_object_fields
}

# One `name<TAB>type<TAB>value` line for each top-level field of the input,
# and nothing else unless the whole input is exactly one JSON object. Nothing
# else may read the status: matched as text, a nested field passes for the
# top-level one and a value of the wrong type for the right one.
#
# A `\u` escape is refused, and so is a name with any escape in it. Two names
# that differ only in an escape would then be one name written twice. A value
# is printed as it stands between its quotes, and a control character cannot
# occur there, so a line holds one field. The status reads a value only as
# one of its plain words, and no short escape can spell one of them, so the
# value as written decides exactly as the decoded one would.
#
# Both awk runs are pinned to the C locale. In a UTF-8 locale awk compares
# strings by their collation there: a name with a soft hyphen in it, `st­ate`,
# compared equal to `state` and stood in for it. The C locale compares bytes.
# It does not check that the text is valid UTF-8. Nothing is decided on a byte
# that is not compared exactly, so such a byte can only keep a value from
# matching.
json_object_fields() {
  LC_ALL=C awk '
    function ws(c) {
      while (i <= n) {
        c = substr(s, i, 1)
        if (c == " " || c == "\t" || c == "\n" || c == "\r") i++
        else break
      }
    }
    # A string opens at i. Leaves i just after its closing quote.
    function string(c, e) {
      i++
      while (i <= n) {
        c = substr(s, i, 1)
        if (c == "\"") { i++; return }
        if (c == "\\") {
          e = substr(s, i + 1, 1)
          if (index("\"\\/bfnrt", e) == 0) { bad = 1; return }
          i += 2
          continue
        }
        if (c < " ") { bad = 1; return }
        i++
      }
      bad = 1
    }
    # A value opens at i, whitespace already skipped. Returns its type, or
    # sets bad.
    function value(depth, c) {
      if (bad || depth > 32) { bad = 1; return "" }
      c = substr(s, i, 1)
      if (c == "{") return object(depth + 1, 0)
      if (c == "[") return array(depth + 1)
      if (c == "\"") { string(); return bad ? "" : "string" }
      if (substr(s, i, 4) == "true") { i += 4; return "true" }
      if (substr(s, i, 5) == "false") { i += 5; return "false" }
      if (substr(s, i, 4) == "null") { i += 4; return "null" }
      if (match(substr(s, i), /^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?/)) {
        i += RLENGTH
        return "number"
      }
      bad = 1
      return ""
    }
    function object(depth, top,   key, f, t, c) {
      i++
      ws()
      if (substr(s, i, 1) == "}") { i++; return "object" }
      for (;;) {
        ws()
        if (substr(s, i, 1) != "\"") { bad = 1; return "" }
        f = i
        string()
        if (bad) return ""
        key = substr(s, f + 1, i - f - 2)
        if (index(key, "\\") > 0) { bad = 1; return "" }
        ws()
        if (substr(s, i, 1) != ":") { bad = 1; return "" }
        i++
        ws()
        f = i
        t = value(depth)
        if (bad) return ""
        if (top) {
          if (key in seen) { bad = 1; return "" }
          seen[key] = 1
          out = out key "\t" t "\t" shown(f, t) "\n"
        }
        ws()
        c = substr(s, i, 1)
        if (c == ",") { i++; continue }
        if (c == "}") { i++; return "object" }
        bad = 1
        return ""
      }
    }
    function array(depth, c) {
      i++
      ws()
      if (substr(s, i, 1) == "]") { i++; return "array" }
      for (;;) {
        ws()
        value(depth)
        if (bad) return ""
        ws()
        c = substr(s, i, 1)
        if (c == ",") { i++; continue }
        if (c == "]") { i++; return "array" }
        bad = 1
        return ""
      }
    }
    # What to print for a value that ran from f up to i.
    function shown(f, t) {
      if (t == "string") return substr(s, f + 1, i - f - 2)
      if (t == "number") return substr(s, f, i - f)
      return ""
    }
    {
      s = s (NR > 1 ? "\n" : "") $0
    }
    END {
      n = length(s)
      i = 1
      ws()
      if (substr(s, i, 1) != "{") exit 1
      object(1, 1)
      ws()
      if (bad || i <= n) exit 1
      printf "%s", out
    }
  '
}

# The value of a top-level field of decoded status fields, when that field is
# of the given type.
daemon_status_field() {
  LC_ALL=C awk -F '\t' -v name="$2" -v type="$3" '$1 == name && $2 == type { print $3; exit }' <<<"$1"
}

# What may be done about whatever holds this DB scope, from one read-only
# look: `free`, `stop` (this scope's daemon, stopped through the CLI) or
# `stop-previous <pid>` (the previous version's daemon, see below). Anything
# else is refused with the status that was read, and returns 1.
daemon_scope_verdict() {
  local tachi_bin="$1"
  local global_db="$2"
  local fields state reason pid

  fields=$(daemon_status_for_scope "$tachi_bin" "$global_db") || fields=""
  state=$(daemon_status_field "$fields" state string)
  case "$state" in
    none|stale_pid) echo free ;;
    running) echo stop ;;
    foreign)
      # After an upgrade the previous version's daemon is reported as
      # foreign, and `daemon kill` refuses it. It holds the lock and the port
      # of this scope until it exits, so the upgrade has to stop it the way
      # launchd does. Any other foreign daemon is left alone and reported.
      reason=$(daemon_status_field "$fields" reason string)
      pid=$(daemon_status_field "$fields" pid number)
      if [[ "$reason" == "daemon version "* && "$pid" =~ ^[0-9]+$ ]]; then
        echo "stop-previous $pid"
      else
        echo "❌ a foreign Tachi daemon holds this DB scope and was not stopped"
        printf '%s\n' "$fields"
        return 1
      fi
      ;;
    *)
      # `unavailable` is a live pid whose state could not be read; an empty
      # state means the binary did not answer. Neither says the scope is free.
      echo "❌ cannot tell what holds this DB scope (daemon status: ${state:-unreadable}); not installing over it"
      printf '%s\n' "$fields"
      return 1
      ;;
  esac
}

stop_existing_daemon_for_scope() {
  local tachi_bin="$1"
  local global_db="$2"
  local verdict fields state pid

  verdict=$(daemon_scope_verdict "$tachi_bin" "$global_db") || {
    printf '%s\n' "$verdict"
    return 1
  }
  case "$verdict" in
    free) ;;
    stop)
      echo ">> Stopping existing Tachi daemon for this DB scope..."
      TACHI_DISABLE_AUTO_DAEMON=1 \
        "$tachi_bin" --global-db "$global_db" --no-project-db daemon kill >/dev/null || true
      ;;
    "stop-previous "*)
      pid=${verdict#stop-previous }
      echo ">> Stopping the previous version's Tachi daemon (pid $pid)..."
      kill -TERM "$pid" 2>/dev/null || true
      ;;
  esac

  for _ in $(seq 1 50); do
    fields=$(daemon_status_for_scope "$tachi_bin" "$global_db") || fields=""
    state=$(daemon_status_field "$fields" state string)
    case "$state" in
      running|foreign) sleep 0.2 ;;
      none|stale_pid) return 0 ;;
      *)
        echo "❌ daemon status became unreadable while waiting for the previous daemon to stop"
        printf '%s\n' "$fields"
        return 1
        ;;
    esac
  done

  echo "❌ previous daemon for this DB scope did not stop"
  printf '%s\n' "$fields"
  return 1
}

wait_for_daemon_for_scope() {
  local tachi_bin="$1"
  local global_db="$2"
  local fields

  for _ in $(seq 1 50); do
    fields=$(daemon_status_for_scope "$tachi_bin" "$global_db") || fields=""
    if [[ "$(daemon_status_field "$fields" state string)" == running ]]; then
      TACHI_DISABLE_AUTO_DAEMON=1 \
        "$tachi_bin" --global-db "$global_db" --no-project-db daemon status || true
      return 0
    fi
    sleep 0.2
  done

  echo "❌ daemon service did not report state=running"
  printf '%s\n' "$fields"
  return 1
}

install_daemon_service() {
  if [ "$(uname -s)" != "Darwin" ]; then
    echo ""
    echo ">> Skipping daemon service: launchd service management is macOS-only"
    return 0
  fi

  require_cmd launchctl
  if ! command -v tachi >/dev/null 2>&1; then
    echo "❌ tachi is not on PATH; install the binary before enabling the daemon service"
    exit 1
  fi

  local tachi_bin
  local label="com.kckylechen.tachi.daemon"
  local launch_agents="$HOME/Library/LaunchAgents"
  local app_home="${TACHI_HOME:-$HOME/.tachi}"
  local logs_dir="$app_home/logs"
  local global_db="${TACHI_DAEMON_GLOBAL_DB:-$app_home/global/tachi-memory.db}"
  local profile="${TACHI_PROFILE:-}"
  local profile_environment=""
  local path_env="${TACHI_LAUNCHD_PATH:-$HOME/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.cargo/bin}"
  local plist="$launch_agents/$label.plist"

  # Ordinary installs use the default Agent surface. Preserve an explicitly
  # configured legacy restriction; daemon startup validates its selector.
  if [ -n "$profile" ]; then
    profile_environment="    <key>TACHI_PROFILE</key>
    <string>$(xml_escape "$profile")</string>"
  fi

  tachi_bin=$(command -v tachi)
  mkdir -p "$launch_agents" "$logs_dir" "$(dirname "$global_db")"

  echo ""
  echo ">> Installing Tachi daemon LaunchAgent..."
  echo "   Label: $label"
  echo "   Binary: $tachi_bin"
  echo "   Global DB: $global_db"

  # Asked before anything is replaced or unloaded, so a refusal leaves the
  # installed plist and the loaded service as they are.
  local verdict
  verdict=$(daemon_scope_verdict "$tachi_bin" "$global_db") || {
    printf '%s\n' "$verdict"
    exit 1
  }

  # Written beside the target and renamed over it: launchd and the next run
  # of this script never see a half-written plist, and a failed write leaves
  # the previous one in place.
  local plist_tmp
  plist_tmp=$(mktemp "$launch_agents/$label.plist.XXXXXX")
  if ! cat >"$plist_tmp" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$(xml_escape "$label")</string>
  <key>ProgramArguments</key>
  <array>
    <string>$(xml_escape "$tachi_bin")</string>
    <string>--daemon</string>
    <string>--port</string>
    <string>$DAEMON_PORT</string>
    <string>--global-db</string>
    <string>$(xml_escape "$global_db")</string>
    <string>--no-project-db</string>
  </array>
  <key>WorkingDirectory</key>
  <string>$(xml_escape "$HOME")</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>$(xml_escape "$path_env")</string>
    <key>TACHI_HOME</key>
    <string>$(xml_escape "$app_home")</string>
$profile_environment
    <key>TACHI_DAEMON_IDLE_TIMEOUT_SECS</key>
    <string>0</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>$(xml_escape "$logs_dir/launchd-daemon.out.log")</string>
  <key>StandardErrorPath</key>
  <string>$(xml_escape "$logs_dir/launchd-daemon.err.log")</string>
</dict>
</plist>
PLIST
  then
    rm -f "$plist_tmp"
    echo "❌ could not write $plist_tmp"
    exit 1
  fi
  mv -f "$plist_tmp" "$plist"

  launchctl_bootout_plist "$plist" "$label"
  stop_existing_daemon_for_scope "$tachi_bin" "$global_db"
  launchctl_bootstrap_plist "$plist" "$label"

  if ! wait_for_daemon_for_scope "$tachi_bin" "$global_db"; then
    echo "   Inspect: $logs_dir/launchd-daemon.err.log"
    exit 1
  fi
}

if [ "$SKIP_BREW" -eq 0 ]; then
  install_tachi_brew
else
  echo ">> Skipping Homebrew install (--skip-brew)"
fi

if [ "$DAEMON_SERVICE" = "1" ]; then
  install_daemon_service
else
  echo ">> Skipping daemon service (--skip-daemon-service)"
fi

echo ""
echo "========================================================="
echo "🎉 Tachi installation complete"
echo "========================================================="
echo ""
if [ "$SKIP_BREW" -eq 0 ]; then
  echo "Tachi CLI:"
  echo "  $(command -v tachi || echo 'tachi not on PATH yet')"
fi
if [ "$DAEMON_SERVICE" = "1" ] && [ "$(uname -s)" = "Darwin" ]; then
  echo "Tachi daemon service:"
  echo "  ~/Library/LaunchAgents/com.kckylechen.tachi.daemon.plist"
fi
echo ""
echo "Next steps:"
echo "  1. Configure API keys (VOYAGE_API_KEY, VOYAGE_RERANK_API_KEY [optional], SILICONFLOW_API_KEY, MINIMAX_API_KEY, REASONING_API_KEY)"
echo "  2. Configure a native HTTP MCP client at http://127.0.0.1:$DAEMON_PORT/mcp"
echo "  3. Verify Tachi with: tachi daemon status && tachi status"
