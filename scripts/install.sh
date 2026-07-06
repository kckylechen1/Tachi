#!/usr/bin/env bash
#
# One-command installer for Tachi + OpenClaw plugin.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/v1.6.4/scripts/install.sh | bash
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

sha256_file() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    echo "❌ Required command not found: sha256sum or shasum" >&2
    return 1
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

daemon_status_for_scope() {
  local tachi_bin="$1"
  local global_db="$2"
  TACHI_DISABLE_AUTO_DAEMON=1 TACHI_DISABLE_STDIO_PROXY=1 \
    "$tachi_bin" --global-db "$global_db" --no-project-db daemon status --json 2>/dev/null || true
}

daemon_status_is_running() {
  grep -q '"state": "running"'
}

stop_existing_daemon_for_scope() {
  local tachi_bin="$1"
  local global_db="$2"
  local status

  status=$(daemon_status_for_scope "$tachi_bin" "$global_db")
  if printf '%s\n' "$status" | daemon_status_is_running; then
    echo ">> Stopping existing Tachi daemon for this DB scope..."
    TACHI_DISABLE_AUTO_DAEMON=1 TACHI_DISABLE_STDIO_PROXY=1 \
      "$tachi_bin" --global-db "$global_db" --no-project-db daemon kill >/dev/null || true
  fi

  for _ in $(seq 1 50); do
    status=$(daemon_status_for_scope "$tachi_bin" "$global_db")
    if ! printf '%s\n' "$status" | daemon_status_is_running; then
      return 0
    fi
    sleep 0.2
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
      TACHI_DISABLE_AUTO_DAEMON=1 TACHI_DISABLE_STDIO_PROXY=1 \
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
  <key>StandardOutPath</key>
  <string>$(xml_escape "$logs_dir/launchd-daemon.out.log")</string>
  <key>StandardErrorPath</key>
  <string>$(xml_escape "$logs_dir/launchd-daemon.err.log")</string>
</dict>
</plist>
PLIST

  launchctl_bootout_plist "$plist" "$label"
  stop_existing_daemon_for_scope "$tachi_bin" "$global_db"
  launchctl_bootstrap_plist "$plist" "$label"

  if ! wait_for_daemon_for_scope "$tachi_bin" "$global_db"; then
    echo "   Inspect: $logs_dir/launchd-daemon.err.log"
    exit 1
  fi
}

download_plugin_tarball() {
  local tarball="$1"
  local primary_asset="tachi-openclaw-v${VERSION}.tar.gz"
  local legacy_asset="memory-hybrid-bridge-v${VERSION}.tar.gz"
  local primary_url="https://github.com/$REPO/releases/download/v${VERSION}/${primary_asset}"
  local legacy_url="https://github.com/$REPO/releases/download/v${VERSION}/${legacy_asset}"

  echo ">> Downloading OpenClaw plugin release asset..."
  if curl -fSL -o "$tarball" "$primary_url"; then
    echo "   Downloaded: $primary_url"
    DOWNLOADED_PLUGIN_ASSET="$primary_asset"
    return 0
  fi

  echo "   Primary asset not found, falling back to legacy asset name..."
  if curl -fSL -o "$tarball" "$legacy_url"; then
    echo "   Downloaded: $legacy_url"
    DOWNLOADED_PLUGIN_ASSET="$legacy_asset"
    return 0
  fi

  echo "❌ Download failed. Checked:"
  echo "   $primary_url"
  echo "   $legacy_url"
  return 1
}

download_plugin_checksum() {
  local checksum_file="$1"
  local asset_name="$2"
  local checksum_url="https://github.com/$REPO/releases/download/v${VERSION}/${asset_name}.sha256"

  echo ">> Downloading OpenClaw plugin checksum..."
  if curl -fSL -o "$checksum_file" "$checksum_url"; then
    echo "   Downloaded: $checksum_url"
    return 0
  fi

  echo "❌ Checksum download failed. Refusing to install an unverifiable plugin archive."
  echo "   Expected checksum asset: $checksum_url"
  echo "   Use --skip-plugin to install only the Tachi binary/daemon."
  return 1
}

verify_plugin_tarball() {
  local tarball="$1"
  local asset_name="$2"
  local checksum_file="$3"
  local expected
  local actual

  expected=$(awk 'NF {print $1; exit}' "$checksum_file" | tr '[:upper:]' '[:lower:]')
  if ! printf '%s' "$expected" | grep -Eq '^[0-9a-f]{64}$'; then
    echo "❌ Invalid checksum file for $asset_name"
    return 1
  fi

  actual=$(sha256_file "$tarball" | tr '[:upper:]' '[:lower:]')
  if [ "$actual" != "$expected" ]; then
    echo "❌ Checksum mismatch for $asset_name"
    echo "   expected: $expected"
    echo "   actual:   $actual"
    return 1
  fi

  echo "   ✅ Checksum verified: $expected"
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
  local checksum_file="$tmpdir/plugin.tar.gz.sha256"
  DOWNLOADED_PLUGIN_ASSET=""

  download_plugin_tarball "$tarball"
  download_plugin_checksum "$checksum_file" "$DOWNLOADED_PLUGIN_ASSET"
  verify_plugin_tarball "$tarball" "$DOWNLOADED_PLUGIN_ASSET" "$checksum_file"

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
