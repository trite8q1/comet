# Two workspaces on one VPS (Nico and Marc)

One `zeron headless` process is one signed-in workspace. Nico and Marc cannot
share a single engine: the data-dir lock, `device-id`, and `session.json` all
belong to one account.

They can share a **machine**. Each person runs their own engine. The VPS then
shows up as a device in Nico's workspace and as a different device in Marc's.
They do not see each other's chats, spaces, or sessions.

Two Unix users is the cleaner split. This note is the same-user setup: one
Linux account, two isolated engines.

`zeron daemon install` is **not** used here. It writes a single user unit
(`zeron.service` / `sh.zeron.app`) and would overwrite the other person.

## Layout

| | Nico | Marc |
|---|---|---|
| Data dir | `~/.zeron-nico` | `~/.zeron-marc` |
| IPC | `27654` | `28654` |
| OAuth callback | `27641` | `28641` |
| Worktrees | `~/.zeron-nico/worktrees` | `~/.zeron-marc/worktrees` |
| Device name | `vps-nico` | `vps-marc` |
| Claude config | `~/.claude-nico` | `~/.claude-marc` |
| Codex home | `~/.codex-nico` | `~/.codex-marc` |
| systemd unit | `zeron-nico.service` | `zeron-marc.service` |

`ZERON_DATA_DIR` isolates the engine lock, device id, WorkOS session, registry
snapshots, journals, uploads, and `{data_dir}/agent-accounts` slots.

Claude and Codex still default to `~/.claude` and `~/.codex`. Without
`CLAUDE_CONFIG_DIR` / `CODEX_HOME`, an account swap in one engine would hit
the other. Point those at the matching data dir as in the table.

The disk is still shared. Use separate clone paths. Do not point both spaces
at the same working tree.

## Env files

```bash
# ~/.config/zeron/nico.env
export ZERON_DATA_DIR=$HOME/.zeron-nico
export ZERON_IPC_PORT=27654
export ZERON_CALLBACK_PORT=27641
export ZERON_WORKTREES_DIR=$HOME/.zeron-nico/worktrees
export ZERON_DEVICE_NAME=vps-nico
export CLAUDE_CONFIG_DIR=$HOME/.claude-nico
export CODEX_HOME=$HOME/.codex-nico
```

```bash
# ~/.config/zeron/marc.env
export ZERON_DATA_DIR=$HOME/.zeron-marc
export ZERON_IPC_PORT=28654
export ZERON_CALLBACK_PORT=28641
export ZERON_WORKTREES_DIR=$HOME/.zeron-marc/worktrees
export ZERON_DEVICE_NAME=vps-marc
export CLAUDE_CONFIG_DIR=$HOME/.claude-marc
export CODEX_HOME=$HOME/.codex-marc
```

`zeron login`, `logout`, `status`, and `headless` must all be run with the
same env file as the daemon. Otherwise they touch `~/.zeron` and fight the
default lock.

```bash
set -a && source ~/.config/zeron/nico.env && set +a
zeron login          # Nico's WorkOS account
# then the same for marc.env / Marc's account
```

Stop the matching unit before `login` / `logout`. Those commands refuse to
change `session.json` while an engine holds the data dir.

## systemd user units

`~/.config/systemd/user/zeron-nico.service`:

```ini
[Unit]
Description=Zeron engine (Nico)
After=network-online.target

[Service]
Type=simple
EnvironmentFile=%h/.config/zeron/nico.env
ExecStart=/usr/local/bin/zeron headless
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

Copy to `zeron-marc.service` with `marc.env`. Point `ExecStart` at the real
binary (`command -v zeron`).

```bash
systemctl --user daemon-reload
systemctl --user enable --now zeron-nico.service zeron-marc.service
loginctl enable-linger $USER
```

Logs: `journalctl --user -u zeron-nico.service -f` (same for marc).

## After it is up

On a laptop, Nico signs into his workspace; Marc into his. Each creates a
space on their VPS device (`vps-nico` / `vps-marc`) pointing at their own
folder, then starts chats there as usual.

Phone and desktop are viewports. They do not need a second engine on the VPS.
