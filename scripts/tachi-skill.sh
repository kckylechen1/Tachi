#!/usr/bin/env bash
# Retired compatibility entrypoint. Native links have one owner CLI; this script
# must not copy skills, edit Hub databases, or remove existing host directories.
set -euo pipefail

cat >&2 <<'EOF'
tachi-skill.sh is retired. Use the explicit native Skill directory commands:

  tachi skill-surface link --source /path/to/central/example-skill --target-dir /path/to/host/skills
  tachi skill-surface links --target-dir /path/to/host/skills
  tachi skill-surface unlink example-skill --target-dir /path/to/host/skills

The source and target directories must already exist. The entire source Skill
directory is linked; there is no copy, Hub registration, or script execution.
Existing unmanaged links/directories are preserved, including those installed
by this legacy script. Inspect and move them aside manually before migrating.

See docs/engineering/operations/native-skill-links.md for host paths, conflict
handling, recovery, and Windows requirements. No files were changed.
EOF
exit 2
