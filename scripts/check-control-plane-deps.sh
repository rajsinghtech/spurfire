#!/usr/bin/env bash
set -euo pipefail

# The HTTP control plane owns metadata/provider APIs only. It must never acquire
# a gameplay node, relay, observer, or Godot runtime through a transitive edge.
tree="$(cargo tree --locked -p spurfire-server --prefix none)"
if printf '%s\n' "$tree" | grep -E '^(rustscale|spurfire-net|spurfire-gdext|godot|tsnet|tailscale-node|tailscale-client) v' >/dev/null; then
  echo "error: spurfire-server gained a forbidden data-plane/runtime dependency" >&2
  printf '%s\n' "$tree" | grep -E '^(rustscale|spurfire-net|spurfire-gdext|godot|tsnet|tailscale-node|tailscale-client) v' >&2
  exit 1
fi
