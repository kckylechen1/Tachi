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

absolute_db_path() {
  local path="$1" home="$2"
  case "$path" in
    /*) ;;
    '~//'*) return 1 ;;
    '~') path="$home" ;;
    '~/'*) [[ "$home" == /* ]] || return 1; path="${home%/}/${path:2}" ;;
    *) return 1 ;;
  esac
  [[ "$path" == /* && "$path" != *$'\n'* ]] || return 1
  printf '%s\n' "$path"
}

# Resolve symlinks even for a new DB whose file or parent does not exist yet.
canonical_db_path() {
  local path="$1" parent leaf resolved
  [[ "$path" == /* && "$path" != *$'\n'* ]] || return 1
  if resolved=$(realpath "$path" 2>/dev/null && printf '.'); then
    resolved=${resolved%$'\n.'}
    [[ "$resolved" != *$'\n'* ]] || return 1
    printf '%s\n' "$resolved"
    return
  fi
  [[ "$path" != / && ! -e "$path" && ! -L "$path" ]] || return 1
  path=${path%/}
  parent=$(canonical_db_path "${path%/*}/") || return 1
  leaf=${path##*/}
  case "$leaf" in
    ''|.) printf '%s\n' "$parent" ;;
    ..) parent=${parent%/*}; printf '%s\n' "${parent:-/}" ;;
    *) printf '%s/%s\n' "${parent%/}" "$leaf" ;;
  esac
}

dotenv_may_override_db_scope() {
  local path="$1" rc=0
  [[ -e "$path" || -L "$path" ]] || return 1
  [[ ! -L "$path" && -f "$path" && -r "$path" ]] || return 0
  LC_ALL=C grep -q 'MEMORY_DB_PATH' "$path" 2>/dev/null || rc=$?
  # Only exit 1 proves MEMORY_DB_PATH is absent without reading or printing secrets.
  [[ "$rc" -ne 1 ]]
}

check_launch_agent_db() {
  local expected="$1" source="$2" domain="$3" arg db="" absolute resolved domain_print
  local user_home="" app_home="" sigil_home="" legacy_home="" environment_db="" memory_db_set=0
  local program="" cwd=/ workspace path
  local has_daemon=0 has_no_project_db=0 only_serve_flags=1
  local -a declared=() launch_arguments=() launch_metadata=()
  local launch_environment_seen=0
  shift 3
  # The definition's environment and launch metadata precede the argv array.
  while [[ "$1" != -- ]]; do
    declared+=("$1")
    shift
  done
  shift
  [[ $# -gt 0 && -n "$1" ]] || return 1
  program="$1"
  shift

  while (($#)); do
    arg="$1"
    shift
    case "$arg" in
      --) break ;;
      --global-db)
        arg=${1:-}
        if (($#)); then shift; fi
        ;;
      --global-db=*) arg=${arg#*=} ;;
      --daemon) has_daemon=1; continue ;;
      --no-project-db) has_no_project_db=1; continue ;;
      --allow-schema-migration)
        existing_allow_schema_migration=1
        continue
        ;;
      --port)
        if (($#)) && [[ -n "$1" ]]; then shift; else only_serve_flags=0; fi
        continue
        ;;
      --port=*) continue ;;
      *) only_serve_flags=0; continue ;;
    esac
    if [[ -z "$arg" || -n "$db" ]]; then
      printf '❌ %s has an empty or repeated --global-db\n' "$source" >&2
      return 1
    fi
    db="$arg"
  done

  # An unloaded definition can inherit scope variables from its GUI domain.
  # The job's own print output already supplies that environment when loaded.
  if [[ "$db" != /* && -n "$domain" ]]; then
    if ! domain_print=$(LC_ALL=C launchctl print "$domain" 2>/dev/null) ||
       ! read_launchctl_scope "$domain_print" "$domain" || ((!launch_environment_seen)); then
      printf '❌ Cannot verify inherited LaunchAgent environment in %s for %s\n' "$domain" "$source" >&2
      return 1
    fi
  fi
  for arg in ${launch_metadata[@]+"${launch_metadata[@]}"} ${declared[@]+"${declared[@]}"}; do
    case "$arg" in
      HOME=*) user_home=${arg#*=} ;;
      TACHI_HOME=*) app_home=${arg#*=} ;;
      SIGIL_HOME=*) sigil_home=${arg#*=} ;;
      TACHI_APP_HOME=*) legacy_home=${arg#*=} ;;
      MEMORY_DB_PATH=*) environment_db=${arg#*=}; memory_db_set=1 ;;
      Program=*) program=${arg#*=} ;;
      WorkingDirectory=*) cwd=${arg#*=} ;;
    esac
  done
  case "${program##*/}" in tachi|tachi-server) ;; *) return 1 ;; esac

  if [[ -z "$db" || "$db" == '~'* ]]; then
    if [[ -z "$user_home" ]]; then
      user_home=$(dscacheutil -q user -a uid "$(id -u)" | awk '/^dir: / { print substr($0, 6) }') || return 1
    fi
    [[ "$user_home" == /* && "$user_home" != *$'\n'* ]] || return 1
  fi
  if [[ -z "$db" ]]; then
    app_home=${app_home:-${sigil_home:-$legacy_home}}
    if [[ -n "$app_home" ]]; then
      app_home=$(absolute_db_path "$app_home" "$user_home") || return 1
    else
      app_home="${user_home%/}/.tachi"
      # Without an explicit home, Tachi can select a workspace data home.
      cwd=$(canonical_db_path "$cwd") || return 1
      while [[ -n "$cwd" ]]; do
        workspace="${cwd%/}/data/tachi"
        if [[ -e "$workspace/global/tachi-memory.db" || -e "$workspace/global/memory.db" || -d "$workspace/projects" ]]; then
          printf '❌ Cannot prove %s DB scope with workspace data home %s\n' "$source" "$workspace" >&2
          return 1
        fi
        cwd=${cwd%/*}
      done
    fi
    if ((memory_db_set)); then
      arg="$environment_db"
    else
      arg="$app_home/global/tachi-memory.db"
    fi
  else
    arg="$db"
  fi

  # Tachi may change its working directory before opening the database.
  if ! absolute=$(absolute_db_path "$arg" "$user_home") || ! resolved=$(canonical_db_path "$absolute"); then
    printf '❌ Cannot resolve %s DB scope %s\n' "$source" "$arg" >&2
    return 1
  fi
  if [[ "$resolved" != "$expected" ]]; then
    printf '❌ %s targets DB scope %s (installer target %s)\n' "$source" "$resolved" "$expected" >&2
    return 1
  fi
  if [[ -z "$db" ]]; then
    if ((!only_serve_flags)); then
      printf '❌ Cannot prove %s DB scope with unrecognized daemon arguments. Use an explicit --global-db.\n' "$source" >&2
      return 1
    fi
    # Dotenv or the manifest can override implicit paths. Prove dotenv files
    # cannot set MEMORY_DB_PATH without reading secrets into shell memory, and
    # skip manifest.json only when `--daemon --no-project-db` serve flags match
    # `should_defer_manifest_startup`.
    local -a dotenv_files=(
      "$app_home/config.env"
      "$user_home/.tachi/config.env"
      "$user_home/.sigil/config.env"
      "$app_home/secrets.env"
      "$user_home/.tachi/secrets.env"
      "$user_home/.secrets/master.env"
    )
    for path in "${dotenv_files[@]}"; do
      if dotenv_may_override_db_scope "$path"; then
        printf '❌ Cannot prove %s DB scope with implicit configuration at %s. Use an explicit --global-db.\n' "$source" "$path" >&2
        return 1
      fi
    done
    if ! ((only_serve_flags && has_daemon && has_no_project_db)); then
      path="$app_home/manifest.json"
      if [[ -e "$path" || -L "$path" ]]; then
        printf '❌ Cannot prove %s DB scope with implicit configuration at %s. Use an explicit --global-db.\n' "$source" "$path" >&2
        return 1
      fi
    fi
  fi
  if [[ "$db" == /* ]]; then
    echo absolute
  else
    echo environment-dependent
  fi
}

check_existing_plist_scope() {
  local plist="$1" expected="$2" domain="$3" count arg i key
  local -a arguments=() metadata=()
  [[ -e "$plist" || -L "$plist" ]] || return 0
  plutil -lint -s "$plist" || return 1
  count=$(plutil -extract ProgramArguments raw -expect array "$plist") || return 1
  for ((i = 0; i < count; i++)); do
    arg=$(plutil -extract "ProgramArguments.$i" raw -expect string -n "$plist" && printf '.') || return 1
    arguments+=("${arg%.}")
  done
  if plutil -type EnableGlobbing "$plist" >/dev/null 2>&1; then
    arg=$(plutil -extract EnableGlobbing raw -expect bool "$plist") || return 1
    [[ "$arg" == false ]] || return 1
  fi
  if plutil -type RootDirectory "$plist" >/dev/null 2>&1; then
    arg=$(plutil -extract RootDirectory raw -expect string -n "$plist" && printf '.') || return 1
    arg="${arg%.}"
    [[ -z "$arg" || "$arg" == / ]] || return 1
  fi
  for key in Program WorkingDirectory; do
    if plutil -type "$key" "$plist" >/dev/null 2>&1; then
      arg=$(plutil -extract "$key" raw -expect string -n "$plist" && printf '.') || return 1
      metadata+=("$key=${arg%.}")
    fi
  done
  if plutil -type EnvironmentVariables "$plist" >/dev/null 2>&1; then
    plutil -extract EnvironmentVariables raw -expect dictionary "$plist" >/dev/null || return 1
    for key in HOME MEMORY_DB_PATH TACHI_HOME SIGIL_HOME TACHI_APP_HOME; do
      if plutil -type "EnvironmentVariables.$key" "$plist" >/dev/null 2>&1; then
        arg=$(plutil -extract "EnvironmentVariables.$key" raw -expect string -n "$plist" && printf '.') || return 1
        metadata+=("$key=${arg%.}")
      fi
    done
  fi
  check_launch_agent_db "$expected" "Existing LaunchAgent plist $plist" "$domain" \
    ${metadata[@]+"${metadata[@]}"} -- ${arguments[@]+"${arguments[@]}"} >/dev/null
}

# Populates the caller's launch_arguments, launch_metadata and environment flag.
read_launchctl_scope() {
  local job="$1" target="$2" line entry indent="" argument_indent section seen="|"
  local -a arguments=() metadata=() defaults=() inherited=() environment=()
  # Inspect the job's own argv and environment rather than re-reading its
  # plist. The loaded job may have been bootstrapped from a different file.
  [[ "$job" == "$target = {"$'\n'* && "$job" == *$'\n}' ]] || return 1
  while IFS= read -r line; do
    if [[ -n "$indent" ]]; then
      case "$line" in
        "$indent}") indent="" ;;
        "$argument_indent"*)
          entry=${line#"$argument_indent"}
          if [[ "$section" == arguments ]]; then
            arguments+=("$entry")
          elif [[ "$entry" =~ ^(HOME|MEMORY_DB_PATH|TACHI_HOME|SIGIL_HOME|TACHI_APP_HOME)\ =\>\ (.*)$ ]]; then
            entry="${BASH_REMATCH[1]}=${BASH_REMATCH[2]}"
            case "$section" in
              'default environment') defaults+=("$entry") ;;
              'inherited environment') inherited+=("$entry") ;;
              environment) environment+=("$entry") ;;
            esac
          elif [[ "$entry" != *' => '* ]]; then
            return 1
          fi
          ;;
        *) return 1 ;;
      esac
    elif [[ "$line" =~ ^([[:blank:]]+)(arguments|environment|inherited\ environment|default\ environment)\ =\ \{$ ]]; then
      indent=${BASH_REMATCH[1]}
      section=${BASH_REMATCH[2]}
      [[ "$seen" != *"|$section|"* ]] || return 1
      seen+="$section|"
      argument_indent="$indent$indent"
    elif [[ "$line" =~ ^[[:blank:]]+program\ =\ (.*)$ ]]; then
      metadata+=("Program=${BASH_REMATCH[1]}")
    elif [[ "$line" =~ ^[[:blank:]]+working\ directory\ =\ (.*)$ ]]; then
      metadata+=("WorkingDirectory=${BASH_REMATCH[1]}")
    fi
  done <<<"$job"
  [[ -z "$indent" ]] || return 1
  launch_arguments=(${arguments[@]+"${arguments[@]}"})
  launch_metadata=(${metadata[@]+"${metadata[@]}"} ${defaults[@]+"${defaults[@]}"} \
    ${inherited[@]+"${inherited[@]}"} ${environment[@]+"${environment[@]}"})
  if [[ "$seen" == *'|environment|'* ]]; then launch_environment_seen=1; fi
}

check_loaded_job_scope() {
  local job="$1" expected="$2" target="$3" scope_kind
  local -a launch_arguments=() launch_metadata=()
  local launch_environment_seen=0
  read_launchctl_scope "$job" "$target" || return 1
  scope_kind=$(check_launch_agent_db "$expected" "Loaded LaunchAgent $target" '' \
    ${launch_metadata[@]+"${launch_metadata[@]}"} -- \
    ${launch_arguments[@]+"${launch_arguments[@]}"}) || return 1
  if [[ "$scope_kind" != absolute ]] && ((!launch_environment_seen)); then
    printf '❌ Cannot read the loaded LaunchAgent environment for %s\n' "$target" >&2
    return 1
  fi
  local loaded_arg
  for loaded_arg in ${launch_arguments[@]+"${launch_arguments[@]}"}; do
    case "$loaded_arg" in
      --) break ;;
      --allow-schema-migration) existing_allow_schema_migration=1 ;;
    esac
  done
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
# look: `free` or the reported PID of a current/previous-version daemon.
# A PID is used only to correlate status with a verified launchd job.
daemon_scope_verdict() {
  local tachi_bin="$1"
  local global_db="$2"
  local fields state reason pid

  fields=$(daemon_status_for_scope "$tachi_bin" "$global_db") || fields=""
  state=$(daemon_status_field "$fields" state string)
  case "$state" in
    none|stale_pid) echo free ;;
    running)
      pid=$(daemon_status_field "$fields" pid number)
      if [[ ! "$pid" =~ ^[1-9][0-9]*$ ]]; then
        echo "❌ cannot identify the PID of the running Tachi daemon for this DB scope"
        return 1
      fi
      printf '%s\n' "$pid"
      ;;
    foreign)
      # A previous-version daemon can still be the verified launchd job.
      reason=$(daemon_status_field "$fields" reason string)
      pid=$(daemon_status_field "$fields" pid number)
      if [[ "$reason" == "daemon version "* && "$pid" =~ ^[1-9][0-9]*$ ]]; then
        printf '%s\n' "$pid"
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

check_daemon_for_upgrade() {
  local tachi_bin="$1"
  local global_db="$2"
  local managed_pid="${3:-}" verdict

  verdict=$(daemon_scope_verdict "$tachi_bin" "$global_db") || {
    printf '%s\n' "$verdict"
    return 1
  }
  if [[ "$verdict" != free && "$verdict" != "$managed_pid" ]]; then
    printf '❌ A daemon (reported PID %s) holds DB scope %s outside the verified LaunchAgent. Stop it through its service manager or original terminal, then rerun the installer.\n' "$verdict" "$global_db" >&2
    return 1
  fi
  # Even an absolute argument can name a symlink that changed after startup.
  if [[ "$verdict" == free && -n "$managed_pid" ]]; then
    printf '❌ Cannot confirm running LaunchAgent PID %s has DB scope %s from daemon status. Verify the service scope before retrying.\n' "$managed_pid" "$global_db" >&2
    return 1
  fi
}

wait_for_daemon_to_stop() {
  local tachi_bin="$1" global_db="$2" fields state
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
  local uid label="com.kckylechen.tachi.daemon"
  uid=$(id -u)
  # Launchd's label is per GUI UID, regardless of HOME, TACHI_HOME or TMPDIR.
  (
    install_lock_dir="/private/tmp/tachi-launchagent-$uid-$label.lock"
    install_plist_tmp=""
    if ! (umask 077 && mkdir "$install_lock_dir"); then
      printf '❌ Cannot acquire LaunchAgent install lock %s. If another installer is active, wait for it to finish. Remove a stale lock only after verifying no installer is active.\n' "$install_lock_dir" >&2
      exit 1
    fi
    trap 'if [[ -n "$install_plist_tmp" ]]; then rm -f "$install_plist_tmp"; fi; rmdir "$install_lock_dir"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    install_locked_daemon_service "$uid" "$label"
  )
}

install_locked_daemon_service() {
  local uid="$1" label="$2"
  local tachi_bin
  local launch_agents="$HOME/Library/LaunchAgents"
  local inherited_home="${TACHI_HOME:-${SIGIL_HOME:-${TACHI_APP_HOME:-}}}"
  local app_home="${TACHI_HOME:-$HOME/.tachi}"
  local -x TACHI_HOME="$app_home"
  local logs_dir="$app_home/logs"
  local global_db="${TACHI_DAEMON_GLOBAL_DB:-$app_home/global/tachi-memory.db}"
  local profile="${TACHI_PROFILE:-}"
  local profile_environment=""
  local path_env="${TACHI_LAUNCHD_PATH:-$HOME/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:$HOME/.cargo/bin}"
  local plist="$launch_agents/$label.plist"
  local expected job service_target absolute_db managed_pid="" print_error
  local existing_allow_schema_migration=0 migration_arg=""

  # Ordinary installs use the default Agent surface. Preserve an explicitly
  # configured legacy restriction; daemon startup validates its selector.
  if [ -n "$profile" ]; then
    profile_environment="    <key>TACHI_PROFILE</key>
    <string>$(xml_escape "$profile")</string>"
  fi

  tachi_bin=$(command -v tachi)
  if ! absolute_db=$(absolute_db_path "$global_db" "$HOME") || ! expected=$(canonical_db_path "$absolute_db"); then
    printf '❌ Cannot resolve installer DB scope %s\n' "$global_db" >&2
    exit 1
  fi
  global_db="$absolute_db"

  echo ""
  echo ">> Installing Tachi daemon LaunchAgent..."
  echo "   Label: $label"
  echo "   Binary: $tachi_bin"
  echo "   Global DB: $global_db"

  # Check the on-disk and loaded definitions independently before either
  # is changed. The loaded job may outlive the file it was started from.
  if ! check_existing_plist_scope "$plist" "$expected" "gui/$uid"; then
    printf '❌ Cannot verify existing LaunchAgent plist %s for DB scope %s\n' "$plist" "$expected" >&2
    exit 1
  fi
  service_target="gui/$uid/$label"
  if job=$(LC_ALL=C launchctl print "$service_target" 2>&1); then
    if ! check_loaded_job_scope "$job" "$expected" "$service_target"; then
      printf '❌ Cannot verify loaded LaunchAgent %s for DB scope %s\n' "$service_target" "$expected" >&2
      exit 1
    fi
    if [[ "$job" =~ $'\n'[[:blank:]]+pid\ =\ ([1-9][0-9]*)$'\n' ]]; then
      managed_pid=${BASH_REMATCH[1]}
    elif [[ "$job" =~ $'\n'[[:blank:]]+state\ =\ running$'\n' ]]; then
      printf '❌ Cannot identify the running LaunchAgent %s in daemon status\n' "$service_target" >&2
      exit 1
    fi
  else
    print_error=${job#$'Bad request.\n'}
    if [[ "$print_error" == *$'\n'* || ( "$print_error" != 'Could not find service' &&
          "$print_error" != 'Could not find service.' &&
          "$print_error" != "Could not find service \"$label\" in domain for "* ) ]]; then
      printf '❌ Cannot query LaunchAgent %s: %s\n' "$service_target" "${job%%$'\n'*}" >&2
      exit 1
    fi
    job=""
  fi
  check_daemon_for_upgrade "$tachi_bin" "$global_db" "$managed_pid" || exit 1
  if [[ -n "$inherited_home" && "$inherited_home" != "$app_home" ]]; then
    local inherited_verdict
    inherited_verdict=$(TACHI_HOME="$inherited_home" daemon_scope_verdict "$tachi_bin" "$global_db") || {
      printf '%s\n' "$inherited_verdict"
      exit 1
    }
    if [[ "$inherited_verdict" != free && "$inherited_verdict" != "$managed_pid" ]]; then
      printf '❌ A daemon (reported PID %s) holds DB scope %s outside the verified LaunchAgent. Stop it through its service manager or original terminal, then rerun the installer.\n' "$inherited_verdict" "$global_db" >&2
      exit 1
    fi
  fi

  # Written beside the target and renamed over it: launchd and the next run
  # of this script never see a half-written plist, and a failed write leaves
  # the previous one in place.
  if ((existing_allow_schema_migration)); then
    migration_arg=$'\n    <string>--allow-schema-migration</string>'
  fi
  mkdir -p "$launch_agents" "$logs_dir" "$(dirname "$global_db")"
  install_plist_tmp=$(mktemp "$launch_agents/$label.plist.XXXXXX")
  if ! cat >"$install_plist_tmp" <<PLIST
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
    <string>--no-project-db</string>${migration_arg}
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
    echo "❌ could not write $install_plist_tmp"
    exit 1
  fi
  if [[ -n "$job" ]] && ! launchctl bootout "$service_target"; then
    exit 1
  fi
  wait_for_daemon_to_stop "$tachi_bin" "$global_db" || exit 1
  mv -f "$install_plist_tmp" "$plist"
  install_plist_tmp=""

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
