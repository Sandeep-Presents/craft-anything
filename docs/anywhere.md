# Using Craft Anything from any agent host

Craft Anything already ships a CLI, an MCP server and a `setup` that connects detected
coding-agent apps. This page describes a small cross-host bootstrap that makes the same
engine usable from **any** agent host (Qoder, Claude Code, OpenCode, Codex, Cursor, ...),
including hosts that are not yet installed, and turns on Deep mode with a local Ghidra.

The bootstrap scripts live in [`packaging/anywhere/`](../packaging/anywhere/):

- `install.ps1` — Windows
- `install.sh` — macOS / Linux

They only **install and connect** the existing engine; they do not modify engine code.

## What the bootstrap does

1. Reuse `craft-anything-cli` on PATH, or download the official release CLI for the OS/arch.
2. Run `craft-anything-cli setup --all-detected --yes` to register the MCP server and install
   the bundled skill into detected hosts.
3. Enable Deep mode: `craft-anything-cli settings_set --key deep_mode --value true`.
4. Point Deep mode at a local Ghidra when one is found (searches common unzip locations):
   `settings_set --key engines.ghidra.install_dir --value '"<dir>"'`, plus
   `engines.ghidra.java_home` when a JDK is found. Built-in engines still work without Ghidra;
   Ghidra only adds richer decompilation.
5. Copy the bundled skill into every known host skill directory so it is ready even for hosts
   that are not installed yet (see matrix below).
6. Print `craft-anything-cli doctor` so the person can see what connected.

After running it, restart or reconnect the host so its MCP tool list refreshes.

## Host matrix

| Host | Skill dir | MCP registration (via `setup`) |
|---|---|---|
| Qoder | `~/.qoder/skills/craft-anything` | `~/.qoder/settings.json` |
| Claude Code | `~/.claude/skills/craft-anything` | `~/.claude.json` |
| OpenCode | `~/.agents/skills/craft-anything` | `~/.config/opencode/opencode.json` |
| Codex | `~/.codex/skills/craft-anything` | `~/.codex/config.toml` |
| Cursor | `~/.cursor/skills/craft-anything` | `~/.cursor/mcp.json` |

`setup --client <id>` handles detected hosts; the bootstrap additionally copies the skill into
undetected hosts' directories.

## Deep mode and consent

Deep mode lets tools read a target's code, files and page source; Clean room (the default)
refuses inside-the-target tools with reason `clean_room`. Enable Deep only for software you own
or are permitted to examine. Tools that run target code or change a device still require the
person's explicit consent (`consent_required`); the bootstrap does not bypass that.

## Manual equivalent

```sh
craft-anything-cli setup --all-detected --yes
craft-anything-cli settings_set --key deep_mode --value true
craft-anything-cli settings_set --key engines.ghidra.install_dir --value '"<ghidra dir>"'
craft-anything-cli doctor
```