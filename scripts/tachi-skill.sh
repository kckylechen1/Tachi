#!/usr/bin/env bash
# tachi-skill — Centralized skill management for Tachi Hub
#
# Central store: ~/.tachi/skills/
# Hub registry:  hub_capabilities table (via `tachi hub register`)
# Client dirs:   ~/.claude/skills/ (symlinks for "listed" skills)
#
# Usage:
#   tachi-skill scan              Scan central store, register all into Hub
#   tachi-skill list              List all skills with status
#   tachi-skill enable  <name>    Symlink to ~/.claude/skills/, mark listed
#   tachi-skill disable <name>    Remove symlink, mark discoverable
#   tachi-skill install <path>    Copy to central store + register + enable
#   tachi-skill status  <name>    Show detailed status of a single skill

set -euo pipefail

TACHI_HOME="${TACHI_HOME:-$HOME/.tachi}"
SKILLS_DIR="$TACHI_HOME/skills"
CLAUDE_SKILLS_DIR="$HOME/.claude/skills"
# #1132: memory.db -> tachi-memory.db. Prefer the canonical name, but fall
# back to the pre-#1132 name for a global DB not yet touched by an `open()`
# call since the rename shipped (the rename-on-open seam only fires inside
# the Rust daemon/CLI, not this standalone script).
DB_PATH="$TACHI_HOME/global/tachi-memory.db"
if [ ! -e "$DB_PATH" ] && [ -e "$TACHI_HOME/global/memory.db" ]; then
    DB_PATH="$TACHI_HOME/global/memory.db"
fi

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
DIM='\033[2m'
BOLD='\033[1m'
NC='\033[0m'

die() { echo -e "${RED}error:${NC} $*" >&2; exit 1; }

ensure_dirs() {
    mkdir -p "$SKILLS_DIR" "$CLAUDE_SKILLS_DIR"
}

# ─── Scan ──────────────────────────────────────────────────────────────────────

cmd_scan() {
    ensure_dirs
    local count=0
    local failed=0
    local updated=0

    for skill_dir in "$SKILLS_DIR"/*/; do
        [ -d "$skill_dir" ] || continue
        local name
        name=$(basename "$skill_dir")

        # Find SKILL.md (the skill definition file)
        local skill_file=""
        if [ -f "$skill_dir/SKILL.md" ]; then
            skill_file="$skill_dir/SKILL.md"
        else
            echo -e "${DIM}  skip $name (no SKILL.md)${NC}"
            continue
        fi

        # Extract description from YAML frontmatter
        local description=""
        description=$(awk '/^---$/{n++; next} n==1 && /^description:/{sub(/^description:[[:space:]]*/, ""); desc=$0; next} n==1 && desc && /^[[:space:]]/{gsub(/^[[:space:]]+/, ""); desc=desc " " $0; next} n==1 && desc{print desc; exit} n>=2 && desc{print desc; exit}' "$skill_file" 2>/dev/null)
        if [ -z "$description" ]; then
            # Simpler extraction: single-line description
            description=$(grep -m1 '^description:' "$skill_file" 2>/dev/null | sed 's/^description:[[:space:]]*//' | head -c 200)
        fi
        [ -z "$description" ] && description="Skill: $name"

        # Determine current visibility
        local current_vis="discoverable"
        if [ -L "$CLAUDE_SKILLS_DIR/$name" ] || [ -d "$CLAUDE_SKILLS_DIR/$name" ]; then
            current_vis="listed"
        fi

        local skill_id="skill:$name"
        local now
        now=$(date -u +"%Y-%m-%dT%H:%M:%S.000Z")

        # Upsert skill metadata + definition in one parameterized transaction.
        # All values are passed via sys.argv — prevents SQL/shell injection
        # from skill names or descriptions containing quotes or semicolons.
        if python3 -c '
import sqlite3, json, sys

db_path, skill_id, skill_name, description, skill_file, visibility, now = sys.argv[1:8]

db = sqlite3.connect(db_path)
try:
    content = open(skill_file).read()
    defn = json.dumps({
        "format": "claude-code-skill-markdown",
        "source_path": skill_file,
        "content": content,
        "policy": {"visibility": visibility, "scope": "pack-shared"},
    })
    desc = (description or "")[:200]
    db.execute(
        """INSERT INTO hub_capabilities
               (id, type, name, version, description, definition, enabled, created_at, updated_at)
           VALUES (?, "skill", ?, 1, ?, ?, 1, ?, ?)
           ON CONFLICT(id) DO UPDATE SET
               description = excluded.description,
               definition = excluded.definition,
               updated_at = excluded.updated_at""",
        (skill_id, skill_name, desc, defn, now, now),
    )
    db.commit()
except Exception as exc:
    sys.stderr.write("error: hub upsert failed for %s: %s\n" % (skill_name, exc))
    sys.exit(1)
finally:
    db.close()
' "$DB_PATH" "$skill_id" "$name" "$description" "$skill_file" "$current_vis" "$now"; then
            count=$((count + 1))
            echo -e "  ${GREEN}✓${NC} $name ${DIM}($current_vis)${NC}"
        else
            failed=$((failed + 1))
            echo -e "  ${RED}✗${NC} $name ${DIM}(hub upsert failed)${NC}"
        fi
    done

    echo -e "\n${BOLD}Scanned: $count skills registered in Hub${NC}"
    if [ "$failed" -gt 0 ]; then
        echo -e "${RED}${failed} skill(s) failed to register — see errors above${NC}" >&2
        return 1
    fi
}

# ─── List ──────────────────────────────────────────────────────────────────────

cmd_list() {
    ensure_dirs
    local listed=0
    local discoverable=0
    local total=0

    printf "${BOLD}%-30s %-14s %-8s %s${NC}\n" "SKILL" "STATUS" "USES" "DESCRIPTION"
    printf "%-30s %-14s %-8s %s\n" "─────" "──────" "────" "───────────"

    for skill_dir in "$SKILLS_DIR"/*/; do
        [ -d "$skill_dir" ] || continue
        local name
        name=$(basename "$skill_dir")
        total=$((total + 1))

        # Check if symlinked (= listed/enabled for Claude)
        local status
        if [ -L "$CLAUDE_SKILLS_DIR/$name" ]; then
            status="${GREEN}● listed${NC}"
            listed=$((listed + 1))
        else
            status="${DIM}○ discoverable${NC}"
            discoverable=$((discoverable + 1))
        fi

        # Query uses count + substr(description, ...) in one parameterized call
        local hub_data
        hub_data=$(python3 -c '
import sqlite3, sys
db_path, skill_name = sys.argv[1], sys.argv[2]
db = sqlite3.connect(db_path)
row = db.execute(
    "SELECT COALESCE(uses, 0), COALESCE(substr(description, 1, 50), \"\") "
    "FROM hub_capabilities WHERE id=?",
    ("skill:" + skill_name,),
).fetchone()
db.close()
if row:
    print("%s|%s" % (row[0], row[1] or ""))
else:
    print("0|")
' "$DB_PATH" "$name" 2>/dev/null || echo "0|")
        local uses="${hub_data%%|*}"
        local desc="${hub_data#*|}"
        [ -z "$uses" ] && uses="—"

        printf "%-30s %-25b %-8s %s\n" "$name" "$status" "$uses" "$desc"
    done

    echo ""
    echo -e "${BOLD}Total: $total${NC}  ${GREEN}Listed: $listed${NC}  ${DIM}Discoverable: $discoverable${NC}"
    echo -e "${DIM}Listed = in ~/.claude/skills/ (system prompt)  |  Discoverable = Hub only (run_skill)${NC}"
}

# ─── Enable ────────────────────────────────────────────────────────────────────

cmd_enable() {
    local name="$1"
    local skill_path="$SKILLS_DIR/$name"

    [ -d "$skill_path" ] || die "Skill '$name' not found in $SKILLS_DIR"

    ensure_dirs

    # Create symlink
    if [ -L "$CLAUDE_SKILLS_DIR/$name" ]; then
        echo -e "${DIM}Already enabled: $name${NC}"
    else
        # Remove any non-symlink directory that might exist
        [ -d "$CLAUDE_SKILLS_DIR/$name" ] && rm -rf "$CLAUDE_SKILLS_DIR/$name"
        ln -s "$skill_path" "$CLAUDE_SKILLS_DIR/$name"
        echo -e "${GREEN}✓${NC} Enabled: $name → ~/.claude/skills/$name"
    fi

    # Update Hub visibility to listed.
    # Values passed via sys.argv to prevent injection; errors logged, not swallowed.
    python3 -c '
import sqlite3, json, sys
db_path, skill_name = sys.argv[1], sys.argv[2]
skill_id = "skill:" + skill_name
db = sqlite3.connect(db_path)
try:
    row = db.execute("SELECT definition FROM hub_capabilities WHERE id=?", (skill_id,)).fetchone()
    if row:
        defn = json.loads(row[0])
        defn.setdefault("policy", {})["visibility"] = "listed"
        db.execute("UPDATE hub_capabilities SET definition=? WHERE id=?", (json.dumps(defn), skill_id))
        db.commit()
except (json.JSONDecodeError, sqlite3.Error) as exc:
    sys.stderr.write("warning: hub visibility update skipped for %s: %s\n" % (skill_name, exc))
finally:
    db.close()
' "$DB_PATH" "$name"

    echo -e "${DIM}Hub visibility → listed (appears in system prompt next session)${NC}"
}

# ─── Disable ───────────────────────────────────────────────────────────────────

cmd_disable() {
    local name="$1"

    # Remove symlink
    if [ -L "$CLAUDE_SKILLS_DIR/$name" ]; then
        rm "$CLAUDE_SKILLS_DIR/$name"
        echo -e "${GREEN}✓${NC} Disabled: $name (symlink removed)"
    elif [ -d "$CLAUDE_SKILLS_DIR/$name" ]; then
        echo -e "${YELLOW}Warning: $name is a real directory, not a symlink. Moving to central store.${NC}"
        # If it's a real dir, move it to central store if not already there
        if [ ! -d "$SKILLS_DIR/$name" ]; then
            mv "$CLAUDE_SKILLS_DIR/$name" "$SKILLS_DIR/$name"
        else
            rm -rf "$CLAUDE_SKILLS_DIR/$name"
        fi
        echo -e "${GREEN}✓${NC} Disabled: $name"
    else
        echo -e "${DIM}Already disabled: $name${NC}"
    fi

    # Update Hub visibility to discoverable.
    # Values passed via sys.argv to prevent injection; errors logged, not swallowed.
    python3 -c '
import sqlite3, json, sys
db_path, skill_name = sys.argv[1], sys.argv[2]
skill_id = "skill:" + skill_name
db = sqlite3.connect(db_path)
try:
    row = db.execute("SELECT definition FROM hub_capabilities WHERE id=?", (skill_id,)).fetchone()
    if row:
        defn = json.loads(row[0])
        defn.setdefault("policy", {})["visibility"] = "discoverable"
        db.execute("UPDATE hub_capabilities SET definition=? WHERE id=?", (json.dumps(defn), skill_id))
        db.commit()
except (json.JSONDecodeError, sqlite3.Error) as exc:
    sys.stderr.write("warning: hub visibility update skipped for %s: %s\n" % (skill_name, exc))
finally:
    db.close()
' "$DB_PATH" "$name"

    echo -e "${DIM}Hub visibility → discoverable (still accessible via run_skill)${NC}"
}

# ─── Install ───────────────────────────────────────────────────────────────────

cmd_install() {
    local source="$1"
    [ -d "$source" ] || die "Source directory '$source' not found"

    local name
    name=$(basename "$source")

    ensure_dirs

    # Copy to central store
    if [ -d "$SKILLS_DIR/$name" ]; then
        echo -e "${YELLOW}Updating existing skill: $name${NC}"
        rm -rf "$SKILLS_DIR/$name"
    fi
    cp -r "$source" "$SKILLS_DIR/$name"
    echo -e "${GREEN}✓${NC} Installed to ~/.tachi/skills/$name"

    # Register in Hub
    cmd_scan_single "$name"

    # Enable by default
    cmd_enable "$name"
}

cmd_scan_single() {
    local name="$1"
    local skill_file="$SKILLS_DIR/$name/SKILL.md"
    [ -f "$skill_file" ] || return

    local description
    description=$(grep -m1 '^description:' "$skill_file" 2>/dev/null | sed 's/^description:[[:space:]]*//' | head -c 200)
    [ -z "$description" ] && description="Skill: $name"

    local now
    now=$(date -u +"%Y-%m-%dT%H:%M:%S.000Z")

    python3 -c '
import sqlite3, json, sys
db_path, skill_name, skill_file, description, now = sys.argv[1:6]
skill_id = "skill:" + skill_name
db = sqlite3.connect(db_path)
try:
    content = open(skill_file).read()
    defn = json.dumps({
        "format": "claude-code-skill-markdown",
        "source_path": skill_file,
        "content": content,
        "policy": {"visibility": "listed", "scope": "pack-shared"},
    })
    desc = (description or "")[:200]
    db.execute(
        """INSERT INTO hub_capabilities
               (id, type, name, version, description, definition, enabled, created_at, updated_at)
           VALUES (?, "skill", ?, 1, ?, ?, 1, ?, ?)
           ON CONFLICT(id) DO UPDATE SET
               description = excluded.description,
               definition = excluded.definition,
               updated_at = excluded.updated_at""",
        (skill_id, skill_name, desc, defn, now, now),
    )
    db.commit()
except Exception as exc:
    sys.stderr.write("warning: hub registration failed for %s: %s\n" % (skill_name, exc))
finally:
    db.close()
' "$DB_PATH" "$name" "$SKILLS_DIR/$name/SKILL.md" "$description" "$now"
}

# ─── Status ────────────────────────────────────────────────────────────────────

cmd_status() {
    local name="$1"
    echo -e "${BOLD}Skill: $name${NC}"
    echo ""

    # Central store
    if [ -d "$SKILLS_DIR/$name" ]; then
        echo -e "  Central store: ${GREEN}✓${NC} $SKILLS_DIR/$name"
        if [ -f "$SKILLS_DIR/$name/SKILL.md" ]; then
            local lines
            lines=$(wc -l < "$SKILLS_DIR/$name/SKILL.md")
            echo -e "  SKILL.md:      $lines lines"
        fi
    else
        echo -e "  Central store: ${RED}✗${NC} not found"
    fi

    # Symlink
    if [ -L "$CLAUDE_SKILLS_DIR/$name" ]; then
        local target
        target=$(readlink "$CLAUDE_SKILLS_DIR/$name")
        echo -e "  Claude link:   ${GREEN}● listed${NC} → $target"
    elif [ -d "$CLAUDE_SKILLS_DIR/$name" ]; then
        echo -e "  Claude link:   ${YELLOW}⚠ real directory${NC} (not managed)"
    else
        echo -e "  Claude link:   ${DIM}○ not linked${NC}"
    fi

    # Hub
    local hub_info
    hub_info=$(python3 -c '
import sqlite3, sys
db_path, skill_name = sys.argv[1], sys.argv[2]
db = sqlite3.connect(db_path)
row = db.execute(
    "SELECT uses, successes, failures, avg_rating, "
    "json_extract(definition, \"$.policy.visibility\") "
    "FROM hub_capabilities WHERE id=?",
    ("skill:" + skill_name,),
).fetchone()
db.close()
if row:
    print("|".join(str(v) if v is not None else "" for v in row))
' "$DB_PATH" "$name" 2>/dev/null || true)

    if [ -n "$hub_info" ]; then
        IFS='|' read -r uses succ fail rating vis <<< "$hub_info"
        echo -e "  Hub status:    ${GREEN}✓${NC} registered"
        echo -e "  Visibility:    $vis"
        echo -e "  Uses:          $uses (success: $succ, fail: $fail)"
        [ "$rating" != "0.0" ] && echo -e "  Rating:        $rating / 5.0"
    else
        echo -e "  Hub status:    ${RED}✗${NC} not registered (run: tachi-skill scan)"
    fi
}

# ─── Main ──────────────────────────────────────────────────────────────────────

case "${1:-help}" in
    scan)
        cmd_scan
        ;;
    list|ls)
        cmd_list
        ;;
    enable)
        [ -z "${2:-}" ] && die "Usage: tachi-skill enable <name>"
        cmd_enable "$2"
        ;;
    disable)
        [ -z "${2:-}" ] && die "Usage: tachi-skill disable <name>"
        cmd_disable "$2"
        ;;
    install)
        [ -z "${2:-}" ] && die "Usage: tachi-skill install <path>"
        cmd_install "$2"
        ;;
    status)
        [ -z "${2:-}" ] && die "Usage: tachi-skill status <name>"
        cmd_status "$2"
        ;;
    help|--help|-h)
        echo "tachi-skill — Centralized skill management"
        echo ""
        echo "Usage:"
        echo "  tachi-skill scan              Scan ~/.tachi/skills/, register all into Hub"
        echo "  tachi-skill list              List all skills with enable/disable status"
        echo "  tachi-skill enable  <name>    Symlink to ~/.claude/skills/ (listed in prompt)"
        echo "  tachi-skill disable <name>    Remove symlink (still in Hub via run_skill)"
        echo "  tachi-skill install <path>    Copy to central store + register + enable"
        echo "  tachi-skill status  <name>    Show detailed status of one skill"
        echo ""
        echo "Central store: ~/.tachi/skills/"
        echo "Listed skills: symlinked to ~/.claude/skills/"
        echo "Discoverable:  Hub only, accessible via run_skill (zero prompt tokens)"
        ;;
    *)
        die "Unknown command: $1 (try: tachi-skill help)"
        ;;
esac
