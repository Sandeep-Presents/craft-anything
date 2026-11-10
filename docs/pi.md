# Pi

Craft Anything can use an installed pi CLI as the builder of a Deep or Clean room build. This is
separate from the provider pi is signed in to: no API key is imported, and Craft Anything never
reads pi's settings, sign-in or sessions.

## Setup

1. Install pi. Its program is `pi`, usually on `PATH` (Homebrew installs it in `/opt/homebrew/bin`).
2. Sign in the way pi does: run `pi` once in a terminal and sign in to the provider its default
   model uses. The sign-in belongs to pi's own configuration; a build stops with pi's own message
   when the provider refuses it.
3. In Settings → AI, detect and add **Pi**, then choose it for **Build with this**. `ai detect`
   offers it as soon as `pi` is found, and `ai test "Pi"` runs one tiny request.

The equivalent backend entry in `config.toml` is:

```toml
[[ai.backends]]
name = "Pi"
kind = "agent_cli"
model = "pi"
enabled = true
```

`model` names the agent. A model pi should use for this backend goes in the backend's
`cli_model`, as with the other agent apps (`craft-anything-cli ai model "Pi" <model>`); the
thinking depth goes in through `--thinking` when one is chosen.

## What a run does

- One message per turn: `pi -p --mode json` with the message on stdin (never in argv, so a long
  brief stays clear of Windows' command-line limit). Its JSONL events are read line by line: the
  session header's id, `message_update` text deltas, both ends of every tool call, each assistant
  message's usage, and a retry that gave up as an error in plain words.
- The conversation continues with `--session-id`, which pi creates on the first turn and resumes
  afterwards. The projects' saved session (`craft.json`) is a pi session like any other agent's.
- A question about a build starts a fresh pi session with reading tools only (or none in Clean
  room), so it leaves the builder's conversation untouched.

## Sessions that travel with the project

pi keeps each conversation as `sessions/--<folder>--/<timestamp>_<id>.jsonl` under its own folder
(`PI_CODING_AGENT_DIR`, else `~/.pi/agent`). The project keeps a copy of it (`agent-session.jsonl`,
written after every turn), and a project moved to another computer puts it back in the folder pi
looks in for that project's working folder, named after the conversation's own timestamp, so the
same conversation carries on (#451). A folder reached through a link — `/tmp` is `/private/tmp` on
macOS — is resolved first, because pi names the real folder; without that the copy lands beside it
and pi starts a second conversation under the same id. When the copy is of another session, or
nothing can be put back, the id is dropped and the next turn starts from the brief, what is in
`app/`, and the newest exchanges kept in the copy (`handoff`).

## The tool server

pi has no `--mcp-config` flag the way Claude Code does. Servers come from `mcp.json` — which a
project only gets to define once it is trusted — or from an extension, so ours arrives as a
one-function extension the run loads with `-e`:

```js
export default function (pi) {
  pi.registerMcpServer("craft-anything-run", { command: "…/craft-anything-cli", args: ["mcp", …], exposure: "direct" });
}
```

- The file is written into a folder of the run's own under the system temporary folder,
  owner-only on Unix (`0700` folder, `0600` file), so nothing another user could prepare in a
  shared temporary folder is ever loaded through it. A folder that can't take it stops the build
  before it starts, with a fix.
- The server is named `craft-anything-run`, not `craft-anything`: pi refuses two clients with one
  name, and a `mcp.json` entry wins over an extension's, so a person's own registration is never
  replaced or reached. Its tools appear as `mcp__craft_anything_run__<tool>`, and Craft Anything
  shortens them back to the tool ids the rest of the app matches on.
- `exposure: "direct"` is what puts the tools in the model's list; pi's default exposure keeps
  them behind `codemode`, where a builder never sees their names.
- A builder's server is given `--all-tools`: this builder has no editor or shell of its own, so
  `build_file_write`, `build_file_edit` and `build_command_run` are how it changes the project.
  Clean room still refuses what it may not read — listing a tool never makes it available.

## What a builder may do

`--tools` in pi filters MCP tools as well as built-in ones when one of its entries starts with
`mcp__`, so the run is held to:

| Access | Tools |
|---|---|
| Build (Deep) | `read`, `grep`, `find`, `ls`, `edit`, `write`, `bash`, `powershell` — each change and command approved by the bridge below — and this run's `mcp__craft_anything_run__*` |
| Build (Clean room) | this run's tools only: no file tools of the agent's own |
| Question (Deep) | `read`, `grep`, `find`, `ls`; no tool server |
| Question (Clean room) | `--no-tools` |

`codemode` and `tool_search` are left out on purpose: `codemode` calls a server's tools by name
whatever the active tool set says, so leaving it in would reach the servers this run isn't meant
to use (a person's other `mcp.json` servers still connect, and only stay out of reach because
nothing here can name their tools).

## Approvals

pi has no `--permission-prompt-tool`, so the same generated extension carries the bridge itself:
its `tool_call` handler maps pi's changing tools (`bash`, `powershell`, `write`, `edit`) into
Claude Code's shape (`Bash {command}`, `Write {file_path, content}`, `Edit {file_path, …}`) and runs
`craft-anything-cli core-action-approve` with this run's project and mode. `allow` lets the call
go on; `deny` blocks it and hands the policy's own words back to the model, so it knows what to do
instead. Reads and ours tools pass straight through — ours carry their own consent.

Nothing is installed into pi's own configuration: the bridge lives in the per-run extension file
described above and is deleted with it. A run that cannot write that file stops before it starts.

## What was verified

- Unit tests: arguments (message on stdin, model and effort, resume, tool lists per access mode),
  the extension's contents and permissions, two runs never sharing a folder, JSONL parsing (text,
  both ends of a tool call, usage, a failed retry), and quoting.
- Live on macOS 15 (pi 1.5.0, Craft Anything 0.4.0, 2026-11-09): `ai detect`, `ai add "Pi"` and
  `ai test "Pi"` (370 ms); one build from a request (`--rounds 1`) and one with `--rounds 0` both
  reached Ready, with `app/index.html` and `app/styles.css` written through `build_file_write` and
  `build_file_edit`, `build_checklist` and `rebuild_check` called, and a refused website approval
  reported back as such. `craft.json` records `agent_session_by: "pi"`.
- A session that travels: a build, its project copied to another folder, every local pi session for
  it deleted, then `--continue-in <copy>` — pi continued the restored file (the follow-up prompt is
  the file's second user message) and the change reached `app/index.html`.
- The approval bridge, in a Deep build: five `bash` calls and two `write` calls ran after the policy
  allowed them (`cargo --version` reached the page's footer as its exact output), and one action the
  policy refused ("Run a scripted web session") was blocked and reported to the builder.
- Not verified: a Clean room build, a build over SSH, a model chosen in Settings, and pi versions
  other than 1.5.0.
