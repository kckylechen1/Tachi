#!/usr/bin/env bash
#
# One-command installer for Tachi + OpenClaw plugin.
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/kckylechen1/tachi/main/scripts/install.sh | bash
#   bash install.sh --version 1.1.2
#
set -euo pipefail

VERSION="${TACHI_VERSION:-latest}"
PLUGIN_DIR="${TACHI_OPENCLAW_PLUGIN_DIR:-${SIGIL_PLUGIN_DIR:-$HOME/.openclaw/extensions/tachi}}"
REPO="kckylechen1/tachi"
SKIP_BREW=0
SKIP_PLUGIN=0

print_help() {
  echo "Usage: install.sh [options]"
  echo "  --version <ver>     Release version to install (default: latest)"
  echo "  --dir <path>        OpenClaw plugin install dir (default: ~/.openclaw/extensions/tachi)"
  echo "  --skip-brew         Skip installing/updating the Tachi Homebrew package"
  echo "  --skip-plugin       Skip installing/updating the OpenClaw plugin"
  echo "  -h, --help          Show this help"
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --dir) PLUGIN_DIR="$2"; shift 2 ;;
    --skip-brew) SKIP_BREW=1; shift ;;
    --skip-plugin) SKIP_PLUGIN=1; shift ;;
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

  echo ">> Verifying native module..."
  if (
    cd "$PLUGIN_DIR"
    node --input-type=module -e "import('@chaoxlabs/tachi-node').then(m => { if (!m.JsMemoryStore) throw new Error('missing export'); console.log('   ✅ Native module loaded OK') })"
  ) 2>/dev/null; then
    :
  else
    echo "   ⚠ Native module not available — OpenClaw will run in MCP-first mode."
  fi

  configure_openclaw_json

  rm -rf "$tmpdir"
}

if [ "$SKIP_BREW" -eq 0 ]; then
  install_tachi_brew
else
  echo ">> Skipping Homebrew install (--skip-brew)"
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
echo "  3. Verify Tachi with: tachi --help"
