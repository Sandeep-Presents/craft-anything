#!/usr/bin/env bash
# Craft Anything "anywhere" bootstrap (macOS / Linux).
# Downloads the official release CLI (original, unmodified), connects it to detected
# agent hosts, enables Deep mode + local Ghidra, and copies the skill to all host dirs.
# Does not edit any upstream source.
set -euo pipefail
VERSION="${1:-0.5.0}"

json_escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }

# verify <file> <asset-name>: check release SHA256SUMS before extracting.
verify() {
  local sums expected actual
  sums="$(mktemp)"
  curl -fsSL "https://github.com/storytold/craft-anything/releases/download/v$VERSION/SHA256SUMS.txt" -o "$sums"
  expected="$(grep -F "$2" "$sums" | head -1 | awk '{print $1}')"
  if [ -z "$expected" ]; then echo "no checksum entry for $2" >&2; return 1; fi
  if command -v sha256sum >/dev/null 2>&1; then actual="$(sha256sum "$1" | awk '{print $1}')";
  else actual="$(shasum -a 256 "$1" | awk '{print $1}')"; fi
  if [ "$actual" != "$expected" ]; then echo "checksum mismatch for $2" >&2; return 1; fi
}

find_ghidra() {
  for r in "$HOME/Tools" "$HOME/Downloads" "$HOME/Desktop" "$HOME" /opt /usr/share /Applications "$HOME/Applications"; do
    [ -d "$r" ] || continue
    for d in "$r"/ghidra_*; do
      [ -f "$d/support/analyzeHeadless" ] && { echo "$d"; return; }
    done
  done
}
find_java() {
  if [ -n "${JAVA_HOME:-}" ]; then echo "$JAVA_HOME"; return; fi
  for d in "$HOME"/Tools/jdk*; do [ -d "$d" ] && { echo "$d"; return; }; done
}

# 1. Locate or download the CLI.
CLI="$(command -v craft-anything-cli || true)"
if [ -z "$CLI" ]; then
  for p in "$HOME/.local/bin/craft-anything-cli" "$HOME/Tools/craft-anything/craft-anything-cli"; do
    [ -x "$p" ] && { CLI="$p"; break; }
  done
fi
if [ -z "$CLI" ]; then
  os="$(uname -s)"; arch="$(uname -m)"
  mkdir -p "$HOME/.local/bin" "$HOME/Tools/craft-anything"
  tmp="$(mktemp -d)"
  if [ "$os" = "Darwin" ]; then
    asset="craft-anything-cli-$VERSION-macos-universal.zip"
    curl -fL "https://github.com/storytold/craft-anything/releases/download/v$VERSION/$asset" -o "$tmp/ca.zip"
    verify "$tmp/ca.zip" "$asset"
    unzip -q "$tmp/ca.zip" -d "$tmp/x"
    find "$tmp/x" -name craft-anything-cli -type f -exec cp {} "$HOME/.local/bin/craft-anything-cli" \;
    chmod +x "$HOME/.local/bin/craft-anything-cli"; CLI="$HOME/.local/bin/craft-anything-cli"
  else
    case "$arch" in x86_64) a=x86_64;; aarch64|arm64) a=aarch64;; *) a="$arch";; esac
    asset="craft-anything-$VERSION-linux-$a.tar.gz"
    curl -fL "https://github.com/storytold/craft-anything/releases/download/v$VERSION/$asset" -o "$tmp/ca.tgz"
    verify "$tmp/ca.tgz" "$asset"
    mkdir -p "$tmp/x"; tar -xzf "$tmp/ca.tgz" -C "$tmp/x"
    find "$tmp/x" -name craft-anything-cli -type f -exec cp {} "$HOME/.local/bin/craft-anything-cli" \;
    chmod +x "$HOME/.local/bin/craft-anything-cli"; CLI="$HOME/.local/bin/craft-anything-cli"
  fi
fi
echo "CLI: $CLI"

# 2. Connect to detected hosts + install bundled skill.
"$CLI" setup --all-detected --yes

# 3. Deep mode + local Ghidra/Java (paths JSON-encoded so quotes/backslashes are safe).
"$CLI" settings_set --key deep_mode --value true >/dev/null
G="$(find_ghidra || true)"
if [ -n "$G" ]; then "$CLI" settings_set --key engines.ghidra.install_dir --value "\"$(json_escape "$G")\"" >/dev/null; echo "Ghidra: $G"; fi
J="$(find_java || true)"
if [ -n "$J" ]; then "$CLI" settings_set --key engines.ghidra.java_home --value "\"$(json_escape "$J")\"" >/dev/null; fi

# 4. Copy bundled skill into every known host skill dir.
SRC=""
for p in "$HOME/.agents/skills/craft-anything" "$HOME/.qoder/skills/craft-anything"; do
  [ -d "$p" ] && { SRC="$p"; break; }
done
if [ -n "$SRC" ]; then
  for h in .qoder .claude .codex .cursor .agents; do
    mkdir -p "$HOME/$h/skills"; cp -R "$SRC" "$HOME/$h/skills/craft-anything"
  done
  echo "Skill copied to host dirs."
fi

# 5. Summary.
"$CLI" doctor
echo "Done. Restart/reconnect your AI host so its MCP tool list refreshes."