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

# The status of whatever holds this DB scope, or nothing. Nothing means the
# binary did not answer: it failed, or its answer was cut short.
daemon_status_for_scope() {
  local tachi_bin="$1"
  local global_db="$2"
  local status

  status=$(TACHI_DISABLE_AUTO_DAEMON=1 \
    "$tachi_bin" --global-db "$global_db" --no-project-db daemon status --json 2>/dev/null) || status=""
  printf '%s\n' "$status" | json_complete_object
}

# Print the first JSON object of the input, only when it is complete: every
# brace and bracket closed, no string left open, nothing after it. A missing
# closing brace is all that tells a cut-short `{"state": "none"` from a real
# one, and the first says nothing about the scope.
json_complete_object() {
  awk '
    {
      if (failed) next
      line = $0
      n = length(line)
      for (i = 1; i <= n; i++) {
        c = substr(line, i, 1)
        if (!started) {
          if (c == "{") { started = 1; depth = 1; text = "{" }
          continue
        }
        if (done) {
          if (index(" \t\r", c) == 0) failed = 1
          continue
        }
        text = text c
        if (in_string) {
          if (escaped) { escaped = 0; continue }
          if (c == "\\") { escaped = 1; continue }
          if (c == "\"") in_string = 0
          continue
        }
        if (c == "\"") in_string = 1
        else if (c == "{" || c == "[") depth++
        else if (c == "}" || c == "]") {
          depth--
          if (depth < 0) failed = 1
          else if (depth == 0) done = 1
        }
      }
      if (started && !done) text = text "\n"
    }
    END {
      if (started && done && !failed) print text
    }
  '
}

daemon_status_is_running() {
  grep -q '"state": "running"'
}

# The value of a quoted field of the status JSON, or nothing.
daemon_status_text() {
  sed -n "s/.*\"$1\": \"\([^\"]*\)\".*/\1/p" | head -n 1
}

# The value of a numeric field of the status JSON, or nothing.
daemon_status_number() {
  sed -n "s/.*\"$1\": \([0-9][0-9]*\).*/\1/p" | head -n 1
}

# What may be done about whatever holds this DB scope, from one read-only
# look: `free`, `stop` (this scope's daemon, stopped through the CLI) or
# `stop-previous <pid>` (the previous version's daemon, see below). Anything
# else is refused with the status that was read, and returns 1.
daemon_scope_verdict() {
  local tachi_bin="$1"
  local global_db="$2"
  local status state reason pid

  status=$(daemon_status_for_scope "$tachi_bin" "$global_db")
  state=$(printf '%s\n' "$status" | daemon_status_text state)
  case "$state" in
    none|stale_pid) echo free ;;
    running) echo stop ;;
    foreign)
      # After an upgrade the previous version's daemon is reported as
      # foreign, and `daemon kill` refuses it. It holds the lock and the port
      # of this scope until it exits, so the upgrade has to stop it the way
      # launchd does. Any other foreign daemon is left alone and reported.
      reason=$(printf '%s\n' "$status" | daemon_status_text reason)
      pid=$(printf '%s\n' "$status" | daemon_status_number pid)
      if [[ "$reason" == "daemon version "* && "$pid" =~ ^[0-9]+$ ]]; then
        echo "stop-previous $pid"
      else
        echo "❌ a foreign Tachi daemon holds this DB scope and was not stopped"
        printf '%s\n' "$status"
        return 1
      fi
      ;;
    *)
      # `unavailable` is a live pid whose state could not be read; an empty
      # state means the binary did not answer. Neither says the scope is free.
      echo "❌ cannot tell what holds this DB scope (daemon status: ${state:-unreadable}); not installing over it"
      printf '%s\n' "$status"
      return 1
      ;;
  esac
}

stop_existing_daemon_for_scope() {
  local tachi_bin="$1"
  local global_db="$2"
  local verdict status state pid

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
    status=$(daemon_status_for_scope "$tachi_bin" "$global_db")
    state=$(printf '%s\n' "$status" | daemon_status_text state)
    case "$state" in
      running|foreign) sleep 0.2 ;;
      none|stale_pid) return 0 ;;
      *)
        echo "❌ daemon status became unreadable while waiting for the previous daemon to stop"
        printf '%s\n' "$status"
        return 1
        ;;
    esac
  done

  echo "❌ previous daemon for this DB scope did not stop"
  printf '%s\n' "$status"
  return 1
}

wait_for_daemon_for_scope() {
  local tachi_bin="$1"
  local global_db="$2"
  local status

  for _ in $(seq 1 50); do
    status=$(daemon_status_for_scope "$tachi_bin" "$global_db")
    if printf '%s\n' "$status" | daemon_status_is_running; then
      TACHI_DISABLE_AUTO_DAEMON=1 \
        "$tachi_bin" --global-db "$global_db" --no-project-db daemon status || true
      return 0
    fi
    sleep 0.2
  done

  echo "❌ daemon service did not report state=running"
  printf '%s\n' "$status"
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
