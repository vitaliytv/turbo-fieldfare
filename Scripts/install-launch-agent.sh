#!/usr/bin/env bash
# Install (or remove) a per-user LaunchAgent that starts TurboFieldfareServer at
# login. Everything machine-specific is derived or passed in: paths come from
# this script's own location, and every server option has a flag.
#
# See docs/AUTOSTART.md for what each launchd key buys and why.

set -euo pipefail

script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
repository_directory="$(cd -- "$script_directory/.." && pwd)"

label="com.turbo-fieldfare.server"
model=""
max_context="16384"
port="8080"
log_file="$HOME/Library/Logs/TurboFieldfare/server.log"
run_at_load="true"
uninstall="false"

usage() {
  cat <<USAGE
usage: Scripts/install-launch-agent.sh [options]

  --model <dir>          Model directory, absolute or relative to the
                         repository. Default: the single *.gturbo directory
                         under scratch/, when exactly one exists.
  --max-context <tokens> 4096, 8192, 16384, 32768, or 65536 (default $max_context).
  --port <1...65535>     Loopback port (default $port).
  --label <name>         launchd label (default $label).
  --log <file>           Combined stdout/stderr log (default under
                         ~/Library/Logs/TurboFieldfare).
  --manual               Do not start at login; run only on demand.
  --uninstall            Remove the agent and its plist, then exit.
  --help                 Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --model) model="${2:?--model needs a directory}"; shift 2 ;;
    --max-context) max_context="${2:?--max-context needs a value}"; shift 2 ;;
    --port) port="${2:?--port needs a value}"; shift 2 ;;
    --label) label="${2:?--label needs a name}"; shift 2 ;;
    --log) log_file="${2:?--log needs a file}"; shift 2 ;;
    --manual) run_at_load="false"; shift ;;
    --uninstall) uninstall="true"; shift ;;
    --help) usage; exit 0 ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
done

plist="$HOME/Library/LaunchAgents/$label.plist"
domain="gui/$(id -u)"

# Booting out an agent that is not loaded is not an error here: the point is to
# reach "not running", and both paths arrive there.
#
# The wait is not optional. `bootout` returns before launchd has finished tearing
# the service down, and bootstrapping a label that is still leaving the domain
# fails with "Bootstrap failed: 5: Input/output error" — which reads like a
# permissions problem and is really a race. Re-running this script to change a
# setting hits it every time.
unload_if_loaded() {
  launchctl bootout "$domain/$label" 2>/dev/null || true
  for _ in $(seq 1 100); do
    launchctl print "$domain/$label" > /dev/null 2>&1 || return 0
    sleep 0.2
  done
  echo "$label is still shutting down after 20s" >&2
  return 1
}

if [[ "$uninstall" == "true" ]]; then
  unload_if_loaded
  rm -f "$plist"
  echo "removed $label"
  exit 0
fi

binary="$repository_directory/.build/release/TurboFieldfareServer"
if [[ ! -x "$binary" ]]; then
  echo "no release binary at $binary" >&2
  echo "build it first: swift build -c release" >&2
  exit 1
fi

# The server accepts a fixed set of context sizes. Rejecting a wrong value here
# turns a launchd restart loop — which reports nothing useful — into one line.
case "$max_context" in
  4096|8192|16384|32768|65536) ;;
  *) echo "--max-context must be 4096, 8192, 16384, 32768, or 65536" >&2; exit 2 ;;
esac

if [[ -z "$model" ]]; then
  # Only auto-detect when the answer is unambiguous; guessing between several
  # checkpoints would silently serve the wrong one.
  shopt -s nullglob
  candidates=("$repository_directory"/scratch/*.gturbo)
  shopt -u nullglob
  if [[ ${#candidates[@]} -eq 1 ]]; then
    model="$(basename "${candidates[0]}")"
    model="scratch/$model"
  else
    echo "pass --model: found ${#candidates[@]} *.gturbo directories under scratch/" >&2
    exit 2
  fi
fi

# Resolve for the existence check only. The agent keeps the path as given,
# together with a WorkingDirectory, so a relative model stays relative.
model_absolute="$model"
[[ "$model" != /* ]] && model_absolute="$repository_directory/$model"
if [[ ! -d "$model_absolute" ]]; then
  echo "model directory not found: $model_absolute" >&2
  exit 1
fi

mkdir -p "$(dirname "$log_file")" "$HOME/Library/LaunchAgents"

cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$label</string>

    <key>ProgramArguments</key>
    <array>
        <string>$binary</string>
        <string>--model</string>
        <string>$model</string>
        <string>--max-context</string>
        <string>$max_context</string>
        <string>--port</string>
        <string>$port</string>
    </array>

    <!-- A relative --model resolves against this, not against /. -->
    <key>WorkingDirectory</key>
    <string>$repository_directory</string>

    <key>EnvironmentVariables</key>
    <dict>
        <key>TFF_LOG_CACHE</key>
        <string>1</string>
    </dict>

    <key>RunAtLoad</key>
    <$run_at_load/>

    <!-- Restart after a crash only. A deliberate launchctl stop stays stopped. -->
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>

    <key>ThrottleInterval</key>
    <integer>30</integer>

    <!-- launchd otherwise runs agents at background QoS and throttles their
         CPU and GPU, which reads as a slow model rather than a capped one. -->
    <key>ProcessType</key>
    <string>Interactive</string>

    <key>StandardOutPath</key>
    <string>$log_file</string>
    <key>StandardErrorPath</key>
    <string>$log_file</string>
</dict>
</plist>
PLIST

plutil -lint "$plist" > /dev/null

unload_if_loaded
launchctl bootstrap "$domain" "$plist"

echo "installed $label"
echo "  model        $model"
echo "  max context  $max_context"
echo "  port         $port"
echo "  log          $log_file"

if [[ "$run_at_load" != "true" ]]; then
  echo "installed for on-demand use; start it with:"
  echo "  launchctl kickstart $domain/$label"
  exit 0
fi

# Report readiness rather than assuming it: loading an agent only means launchd
# accepted it, and weights take a while to map.
printf 'waiting for the server'
for _ in $(seq 1 60); do
  if curl -fsS -m 2 "http://127.0.0.1:$port/v1/models" > /dev/null 2>&1; then
    printf '\nserving on http://127.0.0.1:%s\n' "$port"
    exit 0
  fi
  printf '.'
  sleep 2
done

printf '\nno response yet on port %s; check the log:\n  %s\n' "$port" "$log_file" >&2
exit 1
