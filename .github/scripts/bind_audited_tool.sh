#!/usr/bin/env bash
# Bind a tool installed by a pinned taiki-e/install-action step to the exact
# checksum-verified file that step wrote in this run, and assert its version
# (#1998).
#
#   bind_audited_tool.sh --mark <binary>
#       run as the step right before the install step
#   bind_audited_tool.sh <ENV_VAR> <binary> <subcommand> <version>
#       run after it, e.g.
#       bind_audited_tool.sh AUDITED_NEXTEST cargo-nextest nextest 0.9.140
#
# Why a bound path and not `cargo <subcommand>`: Cargo resolves an external
# subcommand through $CARGO_HOME/bin and PATH, so which file runs depends on
# the host. The acceptance Mac keeps the owner's own ~/.cargo/bin/cargo-nextest
# (0.9.138) and ~/.cargo/bin/cargo-audit (0.22.2, the audited version); they
# lose to the install-action copy today only because the runner service PATH
# happens to list ~/.cargo/bin after the directories GITHUB_PATH prepends.
# When PATH does not list it, Cargo searches $CARGO_HOME/bin first
# (rust-lang/cargo tag rust-1.97.0, src/bin/cargo/main.rs,
# search_directories: `path_dirs.insert(0, home_bin)`). The owner's tools
# stay; the workflow wins on its own by never going through that lookup.
#
# Where the audited file is: the pinned install-action (main.sh) always
# downloads and extracts a rust-crate tool (there is no already-installed
# skip) into $CARGO_HOME/bin when the `cargo` on PATH lives there, and into
# ~/.install-action/bin otherwise (main.sh lines 641-663 at c295c25 and
# 9983c65). Both are candidates here; which one this run wrote is proven by
# freshness, not assumed.
#
# Freshness proof: `--mark` writes ${RUNNER_TEMP}/audited-install-<binary>.marker
# and then waits one second, so anything written by the install step has a
# strictly later timestamp on a filesystem with 1-second or finer timestamp
# resolution, provided wall time moves forward (no clock step back). A
# candidate is fresh when its status-change time (ctime) is later than the
# marker's modification time (`find -newercm`, BSD and GNU). Not mtime:
# install-action extracts with `tar x` and moves with `mv`, both of which keep
# the archive member's mtime (the tool's release date), so the audited file
# is always "older" than the marker by mtime. Creating or renaming the file
# sets its ctime to now, and ctime cannot be set back by a user.
#
# Contract (all failures exit 6 with a ::error:: line; nothing is exported):
#   1. the marker exists as a regular file;
#   2. of ~/.install-action/bin/<binary> and ${CARGO_HOME:-~/.cargo}/bin/<binary>,
#      exactly one is fresh. None fresh (a leftover from an earlier job, or the
#      installer wrote somewhere else) or both fresh (cannot tell which one the
#      installer wrote) fails. Refusing two fresh candidates only helps while
#      the genuine install is still visible as a separate fresh candidate (see
#      the limits below);
#   3. the fresh one is a regular, executable, non-symlink file;
#   4. `<that file> <subcommand> --version` prints, on its first line,
#      "<binary> <version>" or "<binary>-<subcommand> <version>", optionally
#      followed by a space and build details;
#   5. its physical path is printed and exported as <ENV_VAR> via GITHUB_ENV.
# Later steps run "${<ENV_VAR>:?}" <subcommand> ... by absolute path.
# Stale candidates and the PATH lookup are reported by path, never executed.
#
# Out of scope, under the owner-only / accidental-shadow threat model
# (#1980, #1987):
#   - replacing the bound file after this check (TOCTOU): later steps open the
#     path again, and nothing pins the inode or its digest between check and
#     use;
#   - a binary forging its version string: the version is self-reported, and
#     the fresh file already runs during the check;
#   - freshness is not provenance. A ctime refresh of a stale copy during the
#     run (chmod, chown, xattr change, a restore) fails closed only while the
#     genuine install is visible as a second fresh candidate. If a restore
#     replaces the installed pathname itself, or a leftover's ctime is
#     refreshed while the genuine install is absent from both locations, there
#     is exactly one fresh candidate and it is accepted as long as it reports
#     the pinned version;
#   - a wall clock stepped back (between --mark and the install, or after a
#     leftover was written), or a filesystem with timestamps coarser than
#     1 second, can make the genuine install look stale (fails closed: none
#     fresh) or a pre-existing file look fresh.
#
# Bash-3.2 compatible: the macOS acceptance host resolves /bin/bash.

set -euo pipefail

fail() {
  echo "::error::bind_audited_tool: $*" >&2
  exit 6
}

check_word() {
  case "$1" in
    ""|*[![:alnum:]._-]*) fail "binary, subcommand and version must be non-empty [[:alnum:]._-] words (got '$1')" ;;
  esac
}

marker_path() {
  [ -n "${RUNNER_TEMP:-}" ] || fail "RUNNER_TEMP is not set; this script only runs inside an Actions step"
  [ -d "${RUNNER_TEMP}" ] || fail "RUNNER_TEMP '${RUNNER_TEMP}' is not a directory"
  printf '%s/audited-install-%s.marker' "${RUNNER_TEMP}" "$1"
}

if [ "$#" -eq 2 ] && [ "$1" = "--mark" ]; then
  binary="$2"
  check_word "${binary}"
  marker="$(marker_path "${binary}")"
  rm -f -- "${marker}"
  : >"${marker}"
  # Strict ordering for 1-second or finer timestamp resolution, with
  # forward-moving wall time; see "Freshness proof".
  sleep 1
  echo "${binary} install marker: ${marker}"
  exit 0
fi

[ "$#" -eq 4 ] || fail "usage: bind_audited_tool.sh --mark <binary> | <ENV_VAR> <binary> <subcommand> <version>"
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
  check_word "${word}"
done
[ -n "${GITHUB_ENV:-}" ] || fail "GITHUB_ENV is not set; this script only runs inside an Actions step"
[ -n "${HOME:-}" ] || fail "HOME is not set"

# (1) The marker the step before the install wrote.
marker="$(marker_path "${binary}")"
if [ -L "${marker}" ] || [ ! -f "${marker}" ]; then
  fail "no install marker ${marker}; run 'bind_audited_tool.sh --mark ${binary}' as the step right before the install step"
fi

# (2) Exactly one candidate written after the marker.
fresh=""
fresh_count=0
stale=""
seen_dirs=""
for dir in "${HOME}/.install-action/bin" "${CARGO_HOME:-${HOME}/.cargo}/bin"; do
  [ -d "${dir}" ] || continue
  real_dir="$(cd "${dir}" && pwd -P)" || fail "cannot resolve ${dir}"
  case "|${seen_dirs}|" in
    *"|${real_dir}|"*) continue ;;
  esac
  seen_dirs="${seen_dirs}|${real_dir}"
  candidate="${real_dir}/${binary}"
  if [ ! -e "${candidate}" ] && [ ! -L "${candidate}" ]; then
    continue
  fi
  newer="$(find "${candidate}" -prune -newercm "${marker}")" || fail "cannot compare ${candidate} with ${marker}"
  if [ -n "${newer}" ]; then
    fresh="${candidate}"
    fresh_count=$((fresh_count + 1))
  else
    # Reported by path only: an unaudited binary is never executed here.
    echo "${binary} stale copy, not written by this run (bypassed): ${candidate}"
    stale="${stale:+${stale}, }${candidate}"
  fi
done
if [ "${fresh_count}" -eq 0 ]; then
  fail "no ${binary} was written after ${marker} in ~/.install-action/bin or \$CARGO_HOME/bin; the install step did not produce it in this run (stale copies: ${stale:-none})"
fi
if [ "${fresh_count}" -gt 1 ]; then
  fail "${binary} was written after ${marker} in both ~/.install-action/bin and \$CARGO_HOME/bin; cannot tell which one the audited install wrote"
fi

# (3) The extracted file itself, not something reached through a link.
if [ -L "${fresh}" ] || [ ! -f "${fresh}" ] || [ ! -x "${fresh}" ]; then
  fail "${fresh} is not a regular executable file"
fi
bound="${fresh}"
echo "${binary} bound path: ${bound}"

# Shadow report: what an unbound invocation could have picked instead.
path_hit="$(type -P "${binary}" || true)"
echo "${binary} PATH lookup (bypassed): ${path_hit:-none}"

# (4) That exact file reports the audited version.
version_out="$("${bound}" "${subcommand}" --version)" || fail "${bound} ${subcommand} --version exited non-zero"
IFS= read -r first_line <<<"${version_out}" || true
echo "${binary} bound version: ${first_line}"
case "${first_line}" in
  "${binary} ${version}"|"${binary} ${version} "*) ;;
  "${binary}-${subcommand} ${version}"|"${binary}-${subcommand} ${version} "*) ;;
  *) fail "${bound} reports '${first_line}', not the audited ${binary} ${version}" ;;
esac

# (5) Bind: later steps run this absolute path.
printf '%s=%s\n' "${env_var}" "${bound}" >>"${GITHUB_ENV}"
echo "${env_var}=${bound}"
