# Autostart the server

`Scripts/install-launch-agent.sh` installs a per-user LaunchAgent that starts
`TurboFieldfareServer` at login and restarts it after a crash. Nothing about the
machine is baked in: paths are derived from the repository, and every server
option is a flag.

## Install

```sh
swift build -c release
Scripts/install-launch-agent.sh --max-context 65536
```

The script picks the model automatically when `scratch/` holds exactly one
`*.gturbo` directory, and refuses to guess otherwise. It waits for the server to
answer on `/v1/models` before reporting success, so a green line means serving,
not merely loaded.

| Flag | Default | Notes |
| --- | --- | --- |
| `--model <dir>` | the single `*.gturbo` under `scratch/` | Absolute, or relative to the repository. |
| `--max-context <tokens>` | `16384` | One of 4096, 8192, 16384, 32768, 65536. |
| `--port <1...65535>` | `8080` | Loopback only. |
| `--label <name>` | `com.turbo-fieldfare.server` | Use distinct labels to run two models side by side. |
| `--log <file>` | `~/Library/Logs/TurboFieldfare/server.log` | stdout and stderr combined. |
| `--manual` | off | Install without starting at login. |
| `--uninstall` | — | Remove the agent and its plist. |

Re-running the script replaces the agent in place, so it doubles as the way to
change context size, port, or model.

## Operate

```sh
launchctl kickstart -k gui/$(id -u)/com.turbo-fieldfare.server
```

Restart. Needed after every `swift build -c release`: a running process keeps
the old binary, so a rebuild alone changes nothing.

```sh
launchctl print gui/$(id -u)/com.turbo-fieldfare.server
```

State, pid, and last exit code.

```sh
launchctl bootout gui/$(id -u)/com.turbo-fieldfare.server
```

Stop until the next login. It stays stopped — see `KeepAlive` below.

## Why these launchd keys

Each of these prevents a specific failure that is hard to diagnose after the
fact.

**`WorkingDirectory`** — a relative `--model` resolves against the agent's
working directory, which is `/` unless set. Without it the server starts, fails
to find the weights, and launchd retries forever.

**`ProcessType: Interactive`** — launchd otherwise runs agents at background QoS
and throttles their CPU and GPU. The server still answers, just slower, so the
symptom reads as a slow model rather than a capped process.

**`KeepAlive` with `SuccessfulExit=false`** — restart after a crash, but let a
deliberate `launchctl bootout` stay stopped. Plain `KeepAlive: true` would
restart the server the moment you stop it.

**`ThrottleInterval: 30`** — a busy port or an unreadable checkpoint fails
immediately and identically on every attempt. The pause turns a restart loop
into a slow retry you can read in the log.

**`TFF_LOG_CACHE=1`** — prompt-cache misses are otherwise invisible and
expensive; see [Runtime controls](RUNTIME_CONTROLS.md). The reasons name
conditions and counts, never message content, so it is safe to leave on. Drop
the key from the plist to silence it.

## Notes

The agent runs the binary at `.build/release/`, which is whatever the checked
out branch last built. Switching branches and rebuilding changes what the
service serves at its next restart.

The log has no rotation. It grows slowly — one line per request — but it does
grow.

At login the model is mapped into memory whether or not you need it that day.
Install with `--manual` to keep the agent available without that cost.
