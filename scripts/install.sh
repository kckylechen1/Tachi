#!/usr/bin/env bash
#
# One-command installer for Tachi + OpenClaw plugin.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.6.1/scripts/install.sh | bash
#   bash install.sh --version 1.2.0
#
set -euo pipefail

VERSION="${TACHI_VERSION:-latest}"
PLUGIN_DIR="${TACHI_OPENCLAW_PLUGIN_DIR:-${SIGIL_PLUGIN_DIR:-$HOME/.openclaw/extensions/tachi}}"
REPO="kckylechen1/tachi"
SKIP_BREW=0
SKIP_PLUGIN=0
DAEMON_SERVICE="${TACHI_DAEMON_SERVICE:-1}"

print_help() {
  echo "Usage: install.sh [options]"
  echo "  --version <ver>     Release version to install (default: latest)"
  echo "  --dir <path>        OpenClaw plugin install dir (default: ~/.openclaw/extensions/tachi)"
  echo "  --skip-brew         Skip installing/updating the Tachi Homebrew package"
  echo "  --skip-plugin       Skip installing/updating the OpenClaw plugin"
  echo "  --daemon-service    Install/restart the user launchd daemon service (default on macOS)"
  echo "  --skip-daemon-service"
  echo "                       Do not install/restart the daemon service"
  echo "  -h, --help          Show this help"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --dir) PLUGIN_DIR="$2"; shift 2 ;;
    --skip-brew) SKIP_BREW=1; shift ;;
    --skip-plugin) SKIP_PLUGIN=1; shift ;;
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
echo "🧠 Installing Tachi + OpenClaw Plugin"
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
if [ "$SKIP_PLUGIN" -eq 0 ]; then
  require_cmd node
  require_cmd npm
fi
require_cmd curl

if [ "$SKIP_PLUGIN" -eq 0 ]; then
  NODE_MAJOR=$(node -e "console.log(process.versions.node.split('.')[0])")
  if [ "$NODE_MAJOR" -lt 18 ]; then
    echo "❌ Node.js >= 18 required (found $(node -v))"
    exit 1
  fi
fi

resolve_latest_version() {
  curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | node -e "
    let data = '';
    process.stdin.on('data', chunk => data += chunk);
    process.stdin.on('end', () => {
      const tag = JSON.parse(data).tag_name || '';
      process.stdout.write(tag.replace(/^v/, ''));
    });
  "
}

if [ "$VERSION" = "latest" ]; then
  echo ">> Fetching latest release..."
  VERSION=$(resolve_latest_version)
  if [ -z "$VERSION" ]; then
    echo "❌ Could not determine latest version. Use --version to specify."
    exit 1
  fi
fi
echo "   Version: $VERSION"

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
  launchctl kickstart -k "gui/$uid/$label" >/dev/null 2>&1 || true
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
  local global_db="${TACHI_DAEMON_GLOBAL_DB:-$app_home/global/memory.db}"
  local port="${TACHI_DAEMON_PORT:-0}"
  local profile="${TACHI_PROFILE:-standard}"
  local path_env="${TACHI_LAUNCHD_PATH:-$HOME/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.cargo/bin}"
  local plist="$launch_agents/$label.plist"

  tachi_bin=$(command -v tachi)
  mkdir -p "$launch_agents" "$logs_dir" "$(dirname "$global_db")"

  echo ""
  echo ">> Installing Tachi daemon LaunchAgent..."
  echo "   Label: $label"
  echo "   Binary: $tachi_bin"
  echo "   Global DB: $global_db"

  cat >"$plist" <<PLIST
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
    <string>$(xml_escape "$port")</string>
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
    <key>TACHI_PROFILE</key>
    <string>$(xml_escape "$profile")</string>
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

  launchctl_bootout_plist "$plist" "$label"
  launchctl_bootstrap_plist "$plist" "$label"

  sleep 2
  local daemon_json
  daemon_json=$(tachi daemon status --json 2>/dev/null || true)
  if printf '%s\n' "$daemon_json" | grep -q '"state": "running"'; then
    tachi daemon status || true
  else
    echo "❌ daemon service did not report state=running"
    echo "   Inspect: $logs_dir/launchd-daemon.err.log"
    printf '%s\n' "$daemon_json"
    exit 1
  fi
}

download_plugin_tarball() {
  local tarball="$1"
  local primary_url="https://github.com/$REPO/releases/download/v${VERSION}/tachi-openclaw-v${VERSION}.tar.gz"
  local legacy_url="https://github.com/$REPO/releases/download/v${VERSION}/memory-hybrid-bridge-v${VERSION}.tar.gz"

  echo ">> Downloading OpenClaw plugin release asset..."
  if curl -fSL -o "$tarball" "$primary_url"; then
    echo "   Downloaded: $primary_url"
    return 0
  fi

  echo "   Primary asset not found, falling back to legacy asset name..."
  if curl -fSL -o "$tarball" "$legacy_url"; then
    echo "   Downloaded: $legacy_url"
    return 0
  fi

  echo "❌ Download failed. Checked:"
  echo "   $primary_url"
  echo "   $legacy_url"
  return 1
}

configure_openclaw_json() {
  local openclaw_json="$HOME/.openclaw/openclaw.json"
  if [ ! -f "$openclaw_json" ]; then
    echo "   openclaw.json not found at $openclaw_json; skipping auto-config."
    return 0
  fi

  echo ">> Updating openclaw.json..."
  node - "$openclaw_json" "$PLUGIN_DIR" <<'NODE'
const fs = require("fs");
const [jsonPath, pluginDir] = process.argv.slice(2);
const config = JSON.parse(fs.readFileSync(jsonPath, "utf8"));

if (!config.plugins) config.plugins = {};
if (!config.plugins.allow) config.plugins.allow = [];
config.plugins.allow = config.plugins.allow.filter((id) => id !== "memory-hybrid-bridge");
if (!config.plugins.allow.includes("tachi")) config.plugins.allow.push("tachi");

if (!config.plugins.load) config.plugins.load = {};
if (!config.plugins.load.paths) config.plugins.load.paths = [];
if (!config.plugins.load.paths.includes(pluginDir)) config.plugins.load.paths.push(pluginDir);

if (!config.plugins.slots) config.plugins.slots = {};
config.plugins.slots.memory = "tachi";

if (!config.plugins.entries) config.plugins.entries = {};
delete config.plugins.entries["memory-hybrid-bridge"];
if (!config.plugins.entries.tachi) {
  config.plugins.entries.tachi = { enabled: true, config: {} };
} else {
  config.plugins.entries.tachi.enabled = true;
}

fs.writeFileSync(jsonPath, `${JSON.stringify(config, null, 2)}\n`);
NODE
  echo "   ✅ openclaw.json updated"
}

install_openclaw_plugin() {
  local tmpdir
  tmpdir=$(mktemp -d)
  local tarball="$tmpdir/plugin.tar.gz"

  download_plugin_tarball "$tarball"

  echo ">> Installing OpenClaw plugin to $PLUGIN_DIR..."
  mkdir -p "$PLUGIN_DIR"

  if [ -d "$PLUGIN_DIR/data" ]; then
    echo "   Preserving existing data/ directory"
    mv "$PLUGIN_DIR/data" "$tmpdir/data_backup"
  fi

  find "$PLUGIN_DIR" -mindepth 1 -maxdepth 1 ! -name data -exec rm -rf {} +

  tar -xzf "$tarball" -C "$PLUGIN_DIR"

  if [ -d "$tmpdir/data_backup" ]; then
    mv "$tmpdir/data_backup" "$PLUGIN_DIR/data"
  fi
  mkdir -p "$PLUGIN_DIR/data"

  echo ">> Installing plugin dependencies..."
  (
    cd "$PLUGIN_DIR"
    npm install --omit=dev --registry https://registry.npmjs.org
  )

  echo ">> Verifying OpenClaw plugin..."
  node -e "import('node:url').then(({ pathToFileURL }) => import(pathToFileURL(process.argv[1]).href)).then(() => console.log('   ✅ Plugin load smoke test passed')).catch((err) => { console.error(err); process.exit(1); })" "$PLUGIN_DIR/index.js"

  configure_openclaw_json

  rm -rf "$tmpdir"
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

if [ "$SKIP_PLUGIN" -eq 0 ]; then
  install_openclaw_plugin
else
  echo ">> Skipping OpenClaw plugin install (--skip-plugin)"
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
if [ "$SKIP_PLUGIN" -eq 0 ]; then
  echo "OpenClaw plugin path:"
  echo "  $PLUGIN_DIR"
  echo "OpenClaw plugin id:"
  echo "  tachi"
fi
echo ""
echo "Next steps:"
echo "  1. Configure API keys (VOYAGE_API_KEY, VOYAGE_RERANK_API_KEY [optional], SILICONFLOW_API_KEY, MINIMAX_API_KEY, REASONING_API_KEY)"
echo "  2. Restart the OpenClaw gateway"
echo "  3. Verify Tachi with: tachi daemon status && tachi status"
