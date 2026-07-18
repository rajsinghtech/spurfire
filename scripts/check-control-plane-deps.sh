#!/usr/bin/env bash
set -euo pipefail

# The HTTP control plane owns metadata/provider APIs only. It must never acquire
# a gameplay node, relay, observer, or Godot runtime through a transitive edge.
tree="$(cargo tree --locked -p spurfire-server --prefix none)"
forbidden='^(rustscale[^ ]*|spurfire-net[^ ]*|spurfire-gdext|godot[^ ]*|tsnet[^ ]*|tailscale-node[^ ]*|tailscale-client[^ ]*) v'
if printf '%s\n' "$tree" | grep -E "$forbidden" >/dev/null; then
  echo "error: spurfire-server gained a forbidden data-plane/runtime dependency" >&2
  printf '%s\n' "$tree" | grep -E "$forbidden" >&2
  exit 1
fi

# The service may own its HTTP listener, but it must not open a gameplay UDP
# socket. Keep this source guard additive to the transitive package check.
if grep -R -n -E 'UdpSocket|SOCK_DGRAM' crates/spurfire-server/src >/dev/null; then
  echo "error: spurfire-server gained a UDP/gameplay listener primitive" >&2
  grep -R -n -E 'UdpSocket|SOCK_DGRAM' crates/spurfire-server/src >&2
  exit 1
fi
