#!/bin/sh
set -eu

REPO="https://git.unom.io/api/packages/unom/debian"
KEY=/etc/apt/keyrings/punktfunk.asc
LIST=/etc/apt/sources.list.d/punktfunk.list

pkg() { apt-get -y -qq --no-install-recommends "$@" >/dev/null; }

if [ "$(id -u)" = 0 ]; then
  export DEBIAN_FRONTEND=noninteractive
  rm -f /etc/apt/apt.conf.d/docker-clean  # else the apt cache volume stays empty

  pkg update
  pkg install ca-certificates curl
  curl -fsSL "$REPO/repository.key" -o "$KEY"
  echo "deb [signed-by=$KEY] $REPO stable main" >"$LIST"
  pkg update  # not scoped to $LIST: apt would prune the Ubuntu indexes as orphaned
  pkg install punktfunk-host punktfunk-web

  # Host identity lives in $HOME, and the host declines to run as root.
  id -u pf >/dev/null 2>&1 || useradd -m pf
  chown -R pf:pf /home/pf  # the config volume mounts in root-owned
  exec su - pf -c "$0"
fi

CFG="$HOME/.config/punktfunk"

export PUNKTFUNK_MGMT_TOKEN=0000000000000000 PUNKTFUNK_UI_PASSWORD=0000
export PUNKTFUNK_ENCODER=software  # openh264; `auto` never picks it on Linux, and there's no GPU

# `serve` handles API/library + console on 47990 but cannot stream in a container
# (no DRM/compositor path). `punktfunk1-host` takes stream traffic using synthetic
# CPU frames, so pairing and playback work.
punktfunk-host serve --native-port 9778 --no-mdns &
until [ -s "$CFG/cert.pem" ]; do sleep 0.1; done  # serve writes the console's TLS pair

export PUNKTFUNK_MGMT_URL=https://127.0.0.1:47990 HOST=0.0.0.0 PORT=47992 \
  PUNKTFUNK_UI_SECURE=1 PUNKTFUNK_UI_TLS_CERT="$CFG/cert.pem" \
  PUNKTFUNK_UI_TLS_KEY="$CFG/key.pem"
punktfunk-web-server &

echo "console: https://localhost:47992  password: $PUNKTFUNK_UI_PASSWORD"
exec punktfunk-host punktfunk1-host \
  --source synthetic \
  --allow-tofu \
  --pairing-pin 0000 \
  --data-port 47999
