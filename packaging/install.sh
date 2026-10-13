#!/usr/bin/env bash
# The demu user install script.
#
# Installs the demu daemon and its configuration for the current user:
#
#   1. The binary at ~/.cargo/bin/demu, built with cargo when it is
#      missing (cargo install --path crates/demu).
#   2. The machine configs of configs/ into the XDG config path
#      ${XDG_CONFIG_HOME:-$HOME/.config}/demu/configs, so demu
#      resolves bare triple names from there beside its bundled copies.
#   3. The documentation of docs/ into the XDG config path
#      ${XDG_CONFIG_HOME:-$HOME/.config}/demu/docs, which the
#      demu-rom-dev skill references as the installed reference path.
#   4. The OpenCode integration: the mcp.servers.demu entry of
#      docs/examples/opencode.jsonc merged into the global OpenCode
#      configuration ~/.config/opencode/opencode.json(c), and every
#      skill directory of .opencode/skills copied into
#      ~/.config/opencode/skills. Merging replaces only the demu MCP
#      entry and leaves every other setting of the local file alone.
#   5. The user unit packaging/systemd/demu.service into
#      ${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user, reloaded
#      systemd, and the service enabled for the next login.
#
# Environment:
#   DEMU_MCP_URL   the MCP endpoint URL to record in the OpenCode
#                  configuration (default: http://127.0.0.1:6502/mcp,
#                  which matches the unit's --http bind)
#
# Options:
#   --keep   keep machine configs already present in the config
#            directory instead of refreshing them from configs/; run
#            this to preserve local edits of the installed copies
#
# Run from a checkout of the demu repository:
#   packaging/install.sh
#
# Without systemd the script skips the service steps and installs the
# rest. To keep the daemon after logout: loginctl enable-linger.

set -euo pipefail

: "${HOME:?the install needs HOME}"

readonly REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly DEMU_BIN="${HOME}/.cargo/bin/demu"
readonly DEMU_CONFIG_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/demu/configs"
readonly DEMU_DOCS_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/demu/docs"
readonly SYSTEMD_USER_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user"
readonly OPENCODE_DIR="${XDG_CONFIG_HOME:-${HOME}/.config}/opencode"
readonly DEMU_MCP_URL="${DEMU_MCP_URL:-http://127.0.0.1:6502/mcp}"

KEEP_CONFIGS=0

usage() {
    sed -n '/^# The demu user install/,/^#   loginctl enable-linger./p' \
        "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --keep)
            KEEP_CONFIGS=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

# ---------------------------------------------------------------------------
# 1. The binary: build and install through cargo when it is missing.
# ---------------------------------------------------------------------------
if [ ! -x "${DEMU_BIN}" ]; then
    if command -v cargo >/dev/null 2>&1; then
        echo "installing the demu binary with cargo (this builds the workspace)"
        (cd "${REPO_ROOT}" && cargo install --path crates/demu)
    else
        echo "error: ${DEMU_BIN} is missing and cargo is not on PATH." >&2
        echo "error: install the Rust toolchain, then run from the repository:" >&2
        echo "         cargo install --path crates/demu" >&2
        exit 1
    fi
fi
echo "binary: ${DEMU_BIN}"

# ---------------------------------------------------------------------------
# 2. The machine configs into the demu XDG config path.
# ---------------------------------------------------------------------------
mkdir -p "${DEMU_CONFIG_DIR}"
installed_configs=0
kept_configs=0
for config in "${REPO_ROOT}"/configs/*.toml; do
    name="$(basename "${config}")"
    if [ -e "${DEMU_CONFIG_DIR}/${name}" ] && [ "${KEEP_CONFIGS}" = 1 ]; then
        kept_configs=$((kept_configs + 1))
        continue
    fi
    install -m 644 "${config}" "${DEMU_CONFIG_DIR}/${name}"
    installed_configs=$((installed_configs + 1))
done
echo "machine configs: ${DEMU_CONFIG_DIR} (${installed_configs} installed, ${kept_configs} kept)"

# ---------------------------------------------------------------------------
# 3. The documentation into the demu XDG config path.
# ---------------------------------------------------------------------------
mkdir -p "${DEMU_DOCS_DIR}"
cp -R "${REPO_ROOT}/docs/." "${DEMU_DOCS_DIR}/"
echo "documentation: ${DEMU_DOCS_DIR} (refreshed)"

# ---------------------------------------------------------------------------
# 4. The OpenCode integration: the merged MCP entry and the skills.
# ---------------------------------------------------------------------------
mkdir -p "${OPENCODE_DIR}/skills"
python3 - "${REPO_ROOT}/docs/examples/opencode.jsonc" "${OPENCODE_DIR}" "${DEMU_MCP_URL}" <<'PY'
"""Merge the mcp.servers.demu entry of the example into the global OpenCode config."""

import json
import re
import shutil
import sys
from pathlib import Path

EXAMPLE, OPENCODE_DIR, MCP_URL = (Path(sys.argv[1]), Path(sys.argv[2]), sys.argv[3])


def load_jsonc(text: str) -> dict:
    """Parse JSONC: strip comments and trailing commas, then JSON-decode."""
    out, i, in_string = [], 0, False
    while i < len(text):
        char = text[i]
        if in_string:
            if char == "\\":
                out.append(text[i : i + 2])
                i += 2
                continue
            if char == '"':
                in_string = False
            out.append(char)
        elif char == '"':
            in_string = True
            out.append(char)
        elif char == "/" and text[i : i + 2] == "//":
            i = text.find("\n", i)
            if i < 0:
                break
        elif char == "/" and text[i : i + 2] == "/*":
            i = text.find("*/", i) + 2
            if i < 2:
                break
        else:
            out.append(char)
        i += 1
    without_commas = re.sub(r",\s*([}\]])", r"\1", "".join(out))
    return json.loads(without_commas)


def merge(target: Path) -> None:
    """Write the target config with the demu MCP entry merged in."""
    entry = load_jsonc(EXAMPLE.read_text())["mcp"]["servers"]["demu"]
    entry["url"] = MCP_URL
    target.parent.mkdir(parents=True, exist_ok=True)
    was_new = not target.exists()
    backup = target.with_suffix(target.suffix + ".bak")
    if was_new:
        config = {}
    else:
        # One backup per target, of the first pre-merge state: later
        # runs of the merge never overwrite the pristine copy.
        if not backup.exists():
            shutil.copy2(target, backup)
        config = load_jsonc(target.read_text())
    config.setdefault("mcp", {}).setdefault("servers", {})["demu"] = entry
    target.write_text(json.dumps(config, indent=2) + "\n")


json_config = OPENCODE_DIR / "opencode.json"
jsonc_config = OPENCODE_DIR / "opencode.jsonc"
if jsonc_config.exists():
    merge(jsonc_config)
    print(f"opencode config: merged the demu MCP entry into {jsonc_config} (backup .bak)")
if json_config.exists():
    merge(json_config)
    print(f"opencode config: merged the demu MCP entry into {json_config} (backup .bak)")
if not json_config.exists() and not jsonc_config.exists():
    merge(jsonc_config)
    print(f"opencode config: created {jsonc_config} with the demu MCP entry")
PY

for skill in "${REPO_ROOT}"/.opencode/skills/*/; do
    [ -d "${skill}" ] || continue
    cp -R "${skill}" "${OPENCODE_DIR}/skills/"
    echo "opencode skill: merged $(basename "${skill}") into ${OPENCODE_DIR}/skills"
done

# ---------------------------------------------------------------------------
# 5. The systemd user unit: install, reload, enable.
# ---------------------------------------------------------------------------
mkdir -p "${SYSTEMD_USER_DIR}"
install -m 644 "${REPO_ROOT}/packaging/systemd/demu.service" \
    "${SYSTEMD_USER_DIR}/demu.service"
echo "user unit: ${SYSTEMD_USER_DIR}/demu.service"

if command -v systemctl >/dev/null 2>&1 && systemctl --user >/dev/null 2>&1; then
    systemctl --user daemon-reload
    if systemctl --user enable --now demu; then
        echo "service: enabled and started (systemctl --user status demu)"
        echo "to keep the daemon after logout: loginctl enable-linger"
    else
        echo "warning: the enable failed; start manually with:" >&2
        echo "         systemctl --user enable --now demu" >&2
    fi
else
    echo "warning: systemd is unavailable; the unit is installed but not enabled" >&2
    echo "warning: enable later with: systemctl --user daemon-reload && systemctl --user enable --now demu" >&2
fi

echo "install complete"