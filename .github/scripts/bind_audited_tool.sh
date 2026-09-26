#!/usr/bin/env bash
# Bind a tool installed by the pinned taiki-e/install-action to the exact
# checksum-verified file it extracted, and assert its version (#1998).
#
#   bind_audited_tool.sh <ENV_VAR> <binary> <subcommand> <version>
#   e.g. bind_audited_tool.sh AUDITED_NEXTEST cargo-nextest nextest 0.9.140
#
# Why a bound path and not `cargo <subcommand>`: Cargo resolves an external
# subcommand through $CARGO_HOME/bin and PATH, so which file runs depends on
# the host. The acceptance Mac keeps the owner's own ~/.cargo/bin/cargo-nextest
# (0.9.138) and ~/.cargo/bin/cargo-audit; they lose to the install-action copy
# today only because the runner service PATH happens to list ~/.cargo/bin
# after the directories GITHUB_PATH prepends. When PATH does not list it,
# Cargo searches $CARGO_HOME/bin first (rust-lang/cargo tag rust-1.97.0,
# src/bin/cargo/main.rs, search_directories: `path_dirs.insert(0, home_bin)`).
# The owner's tools stay; the workflow wins on its own by never going through
# that lookup.
#
# Where the audited file is: the pinned install-action (main.sh) extracts a
# rust-crate tool into $CARGO_HOME/bin only when the `cargo` on PATH lives
# there; after ./.github/actions/setup-rust, `cargo` is the toolchain binary
# (the runner log prints "cargo is located at ~/.rustup/toolchains/..."), so
# the tool lands in ~/.install-action/bin. Any other layout fails closed here
# rather than guessing.
#
# Contract (all failures exit 6 with a ::error:: line):
#   1. ~/.install-action/bin/<binary> is a regular, executable, non-symlink file;
#   2. `<that file> <subcommand> --version` prints, on its first line,
#      "<binary> <version>" or "<binary>-<subcommand> <version>", optionally
#      followed by a space and build details;
#   3. the physical path is printed and exported as <ENV_VAR> via GITHUB_ENV.
# Later steps run "${<ENV_VAR>:?}" <subcommand> ... by absolute path.
# Shadow candidates (PATH lookup, $CARGO_HOME/bin) are reported, never used.
#
# Bash-3.2 compatible: the macOS acceptance host resolves /bin/bash.

set -euo pipefail

fail() {
  echo "::error::bind_audited_tool: $*" >&2
  exit 6
}

[ "$#" -eq 4 ] || fail "usage: bind_audited_tool.sh <ENV_VAR> <binary> <subcommand> <version>"
env_var="$1"
binary="$2"
subcommand="$3"
version="$4"

# Character classes, not ranges: [A-Z] can match lowercase in some locales.
case "${env_var}" in
  AUDITED_?*) ;;
  *) fail "env var '${env_var}' must be named AUDITED_*" ;;
esac
case "${env_var}" in
  *[![:upper:][:digit:]_]*) fail "env var '${env_var}' must be uppercase letters, digits and _" ;;
esac
for word in "${binary}" "${subcommand}" "${version}"; do
  case "${word}" in
    ""|*[![:alnum:]._-]*) fail "binary, subcommand and version must be non-empty [[:alnum:]._-] words (got '${word}')" ;;
  esac
done
[ -n "${GITHUB_ENV:-}" ] || fail "GITHUB_ENV is not set; this script only runs inside an Actions step"
[ -n "${HOME:-}" ] || fail "HOME is not set"

prebuilt_dir="${HOME}/.install-action/bin"
prebuilt="${prebuilt_dir}/${binary}"

# (1) The extracted file itself, not something reached through a link.
if [ -L "${prebuilt}" ] || [ ! -f "${prebuilt}" ] || [ ! -x "${prebuilt}" ]; then
  fail "${prebuilt} is not a regular executable file; the pinned install-action did not extract ${binary} where this binding expects it"
fi
bound_dir="$(cd "${prebuilt_dir}" && pwd -P)" || fail "cannot resolve ${prebuilt_dir}"
bound="${bound_dir}/${binary}"
echo "${binary} bound path: ${bound}"

# Shadow report: what an unbound invocation could have picked instead.
path_hit="$(type -P "${binary}" || true)"
echo "${binary} PATH lookup (bypassed): ${path_hit:-none}"
home_bin_hit="${CARGO_HOME:-${HOME}/.cargo}/bin/${binary}"
if [ -e "${home_bin_hit}" ]; then
  # Reported by path only: an unaudited binary is never executed here.
  echo "${binary} CARGO_HOME/bin copy (bypassed): ${home_bin_hit}"
fi

# (2) That exact file reports the audited version.
version_out="$("${bound}" "${subcommand}" --version)" || fail "${bound} ${subcommand} --version exited non-zero"
IFS= read -r first_line <<<"${version_out}" || true
echo "${binary} bound version: ${first_line}"
case "${first_line}" in
  "${binary} ${version}"|"${binary} ${version} "*) ;;
  "${binary}-${subcommand} ${version}"|"${binary}-${subcommand} ${version} "*) ;;
  *) fail "${bound} reports '${first_line}', not the audited ${binary} ${version}" ;;
esac

# (3) Bind: later steps run this absolute path.
printf '%s=%s\n' "${env_var}" "${bound}" >>"${GITHUB_ENV}"
echo "${env_var}=${bound}"
