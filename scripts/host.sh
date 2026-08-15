#!/bin/sh
# Runs a punktfunk host in a bare ubuntu container (see `task docker:host`):
# installs the packages as root, then re-execs as `pf` to run the daemons.
set -eu

REPO="https://git.unom.io/api/packages/unom/debian"
KEY=/etc/apt/keyrings/punktfunk.asc

CFG="$HOME/.config/punktfunk"
PAIRED="$CFG/punktfunk1-paired.json"
MGMT_PORT=47990
CONSOLE_PORT=47992

pkg() { apt-get -y -qq --no-install-recommends "$@" >/dev/null; }

install_as_root() {
  export DEBIAN_FRONTEND=noninteractive
  rm -f /etc/apt/apt.conf.d/docker-clean  # else the apt cache volume stays empty

  pkg update
  pkg install ca-certificates curl
  curl -fsSL "$REPO/repository.key" -o "$KEY"
  echo "deb [signed-by=$KEY] $REPO stable main" >/etc/apt/sources.list.d/punktfunk.list
  pkg update  # not scoped to that list: apt would prune the Ubuntu indexes as orphaned
  pkg install punktfunk-host punktfunk-web

  # Host identity lives in $HOME, and the host declines to run as root.
  id -u pf >/dev/null 2>&1 || useradd -m pf
  chown -R pf:pf /home/pf  # the config volume mounts in root-owned
}

# API/library + the console's TLS pair. `serve` snapshots $PAIRED at startup, so a
# client that pairs later (every `docker:deploy` run has a fresh, ephemeral identity)
# is written by punktfunk1-host and never seen here — /api/v1/library then 401s and
# the app shows an empty library. Restart on any change to the pairing list.
serve() {
  fingerprint() { cksum "$PAIRED" 2>/dev/null || true; }
  while :; do
    punktfunk-host serve --native-port 9778 --no-mdns &
    pid=$! seen=$(fingerprint)
    while [ "$(fingerprint)" = "$seen" ]; do sleep 2; done
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true  # let it release the port before the restart
  done
}

console() {
  export PUNKTFUNK_MGMT_URL="https://127.0.0.1:$MGMT_PORT" HOST=0.0.0.0 PORT="$CONSOLE_PORT" \
    PUNKTFUNK_UI_SECURE=1 PUNKTFUNK_UI_TLS_CERT="$CFG/cert.pem" PUNKTFUNK_UI_TLS_KEY="$CFG/key.pem"
  punktfunk-web-server
}

if [ "$(id -u)" = 0 ]; then
  install_as_root
  exec su - pf -c "$0"
fi

export PUNKTFUNK_MGMT_TOKEN=0000000000000000 PUNKTFUNK_UI_PASSWORD=0000
export PUNKTFUNK_ENCODER=software  # openh264; `auto` never picks it on Linux, and there's no GPU

serve &
until [ -s "$CFG/cert.pem" ]; do sleep 0.1; done  # serve writes the console's TLS pair
console &

echo "console: https://localhost:$CONSOLE_PORT  password: $PUNKTFUNK_UI_PASSWORD"

# `serve` can't stream in a container (no DRM/compositor path); punktfunk1-host takes
# the stream traffic with synthetic CPU frames, so pairing and playback still work.
# No --allow-tofu: it admits the client without persisting a pairing, so `serve`'s mTLS
# library API still 401s ("not paired"). --allow-pairing keeps the PIN ceremony armed for
# good — the console's Pairing page arms only `serve`'s plane (9778), so a knock on this
# process's 9777 never shows up there and the startup arming just times out.
exec punktfunk-host punktfunk1-host \
  --source synthetic \
  --allow-pairing \
  --pairing-pin 0000 \
  --data-port 47999
