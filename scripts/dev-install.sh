#!/usr/bin/env bash
# Build and install the local development tachi binary with release sentinels.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DIR="${HOME}/bin"
DESTINATION="${INSTALL_DIR}/tachi"
TARGET_DIR="${CARGO_TARGET_DIR:-${HOME}/.cache/sigil-shared-target}"
RELEASE_BINARY="${TARGET_DIR}/release/tachi"
SENTINELS=(
  "sticky_leave (leave a read-once ephemeral note"
  "tachi_memory(action='sticky_check')"
  "FROM session_claims WHERE claim_id = ?1"
  "dispatch_outcomes row "
)

fail() {
  printf 'dev-install: %s\n' "$*" >&2
  exit 1
}

next_backup_path() {
  local prefix candidate suffix
  prefix="${DESTINATION}.backup.$(date +%Y%m%d%H%M%S)"
  candidate="${prefix}"
  suffix=1
  while [[ -e "${candidate}" || -L "${candidate}" ]]; do
    candidate="${prefix}.${suffix}"
    ((suffix += 1))
  done
  printf '%s\n' "${candidate}"
}

cd "${ROOT}"
CARGO_TARGET_DIR="${TARGET_DIR}" cargo build --locked --release --bin tachi
[[ -f "${RELEASE_BINARY}" ]] || fail "release build did not produce ${RELEASE_BINARY}"

literal_dump="$(mktemp)"
trap 'rm -f "${literal_dump}"' EXIT
strings "${RELEASE_BINARY}" > "${literal_dump}"
for sentinel in "${SENTINELS[@]}"; do
  # Each sentinel includes literal syntax or whitespace, so a private Rust
  # identifier such as `handle_sticky_leave` cannot satisfy this release gate.
  if ! grep -Fq "${sentinel}" "${literal_dump}"; then
    fail "release literal gate failed: missing ${sentinel}"
  fi
done
rm -f "${literal_dump}"
trap - EXIT
printf 'dev-install: release literal gate passed for %s\n' "${RELEASE_BINARY}"

mkdir -p "${INSTALL_DIR}"
staged="$(mktemp "${INSTALL_DIR}/.tachi.new.XXXXXX")"
cleanup_stage() {
  rm -f "${staged}"
}
trap cleanup_stage EXIT
install -m 755 "${RELEASE_BINARY}" "${staged}"

backup=""
if [[ -L "${DESTINATION}" ]]; then
  printf 'dev-install: replacing symlink %s -> %s with a real file\n' \
    "${DESTINATION}" "$(readlink "${DESTINATION}")"
fi
if [[ -e "${DESTINATION}" || -L "${DESTINATION}" ]]; then
  backup="$(next_backup_path)"
  mv "${DESTINATION}" "${backup}"
  printf 'dev-install: backed up %s to %s\n' "${DESTINATION}" "${backup}"
fi

if ! mv "${staged}" "${DESTINATION}"; then
  if [[ -n "${backup}" ]]; then
    mv "${backup}" "${DESTINATION}"
  fi
  fail "could not install ${DESTINATION}"
fi
trap - EXIT

if ! "${DESTINATION}" --version; then
  rm -f "${DESTINATION}"
  if [[ -n "${backup}" ]]; then
    mv "${backup}" "${DESTINATION}"
  fi
  fail "installed binary failed its version check"
fi

printf 'dev-install: installed %s\n' "${DESTINATION}"
if [[ -n "${backup}" ]]; then
  printf 'dev-install: rollback: mv %q %q\n' "${backup}" "${DESTINATION}"
fi
