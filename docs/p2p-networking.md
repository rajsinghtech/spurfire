# Peer gameplay networking

Spurfire's native gameplay data plane uses application UDP through embedded RustScale. The integration is pinned to RustScale revision `8511e0b78074bf07b59d53cf1a2eb349cd0d2407` plus the focused vendored netstack wakeup patch documented in `vendor/rustscale-netstack/PROVENANCE.md`; do not update either without rerunning the live lifecycle probe.

## Components

- `crates/spurfire-net` defines transport-independent, 1,200-byte-bounded envelopes.
- Envelopes carry heartbeats, rider input/snapshots, shot commands/results, authority announcements, and migration snapshots.
- `SessionState` rejects replayed/out-of-order sequences, wrong-lobby traffic, and stale authority epochs. Silent authority loss elects the lowest connected `PlayerId` and advances the epoch.
- `spurfire_net::rustscale::RustScalePeer` enrolls an ephemeral node with a one-use auth key, clears the key after `up()`, binds `Server::listen_packet`, and sends/receives through `UdpListener`.
- `SnapshotBuffer` interpolates authoritative states two ticks behind, follows the shortest yaw arc, caps velocity extrapolation at fifteen ticks (250 ms at 60 Hz), and classifies prediction corrections as smoothable or snap-sized.
- Godot's native `PeerSession` node owns RustScale on a background Tokio runtime and moves packets to the main thread through signals. OAuth credentials remain control-plane-only; this class accepts only a narrow join auth key.
- Godot's `NetworkRider` presents buffered remote snapshots and exposes reconciliation results. `multiplayer_replication.gd` sends authority snapshots or rider inputs from the fixed physics loop and feeds accepted snapshots into the remote rider.

The C ABI still has no gameplay UDP API. Spurfire does not use it: the Rust GDExtension links the native Rust `tsnet` API directly.

RustScale revision `8511e0b` did not wake its smoltcp poll loop after `UdpListener::send_to` enqueued application data, allowing packets to batch behind a one-second idle fallback. Spurfire's pinned patch wakes after enqueue and includes a sub-500-ms idle-send regression test. This is tracked upstream as [rustscale#75](https://github.com/rajsinghtech/rustscale/issues/75). A managed 1,600-snapshot-per-peer soak measured steady maximum packet gaps around 41–54 ms; control-map reconnects caused brief 125–287 ms gaps but no rejection, disconnect, accumulated delay, or traffic loss.

## Live proof

Run:

```bash
just p2p-live
```

The script requires the gitignored `.env` OAuth settings. It:

1. Mints an organization token.
2. Creates an API-only child tailnet.
3. Immediately stores its one-time child credentials in a mode-0600 temporary file.
4. Installs a disposable `tag:spurfire` allow policy.
5. Mints five non-reusable, ephemeral, preauthorized 15-minute keys.
6. Enrolls two independent embedded RustScale servers.
7. Exchanges a bounded Spurfire Hello and rider-input frame in both directions over application UDP and reports the route class.
8. Starts three additional peers as separate OS processes, establishes the mesh, and forcibly kills authority process A without a Leave packet.
9. Verifies surviving processes B and C elect B at epoch 2, exchange the authority announcement and a new rider-input frame, and continue play.
10. Closes survivors and exactly deletes the child tailnet under an exit trap.

On 2026-07-17, the complete probe printed `SPURFIRE_P2P_UDP_OK`, `SPURFIRE_MIGRATION_OK authority=a successor=b epoch=2 continued_play=true`, and `SPURFIRE_P2P_LIFECYCLE_OK`. The child tailnet `tail83569b.ts.net` was deleted. Earlier direct-UDP development tailnets were also deleted. Those runs exposed RustScale's retryable macOS port-mapper shutdown uncertainty, which the smoke retries and treats as a local teardown warning only after traffic succeeds.

## Security boundaries

- Never pass organization or child OAuth credentials to Godot.
- Never print auth keys. The live script writes them only to its private temporary directory and deletes that directory on every exit path.
- Datagram size is checked before JSON parsing and before send.
- Application identity is the validated lobby/player ID in the envelope, not an untrusted hostname.
- Sequence rejection limits replay within a peer session; authority epochs prevent a disconnected old host from resuming authority.

## Remaining production work

Design for the first four items is settled in `docs/decisions.md` D6/D7 and
`docs/prototype-plan.md` §M6; they build after the M5 fun verdict.

- Unify authority migration on one rule (D6): survivors recompute `election_v1` over the
  match-start matrix restricted to the survivor set; `SessionState.expire_and_migrate`'s
  lowest-ID rule becomes the degraded fallback inside the same scoring function.
- Replace the `MigrationSnapshot` hash stub with real `MatchState` handoff: 2 Hz keyframes +
  20 Hz deltas, 10 s peer ring buffers, successor restores and announces the restored-state
  hash.
- Add lag compensation (D7): authority-side rewind over ~250 ms position + stance history,
  capped at 150 ms; stance must be in snapshots from M2 onward.
- Drive `PeerSession` from the lobby join HTTP flow and distribute peer endpoints via the
  roster (additive measurements field), replacing file-based demo discovery.
- Add authenticated session-level packet tags if tailnet membership alone is not sufficient for the final threat model.
- Apply authority rider inputs to separately simulated remote horse entities and add input replay after reconciliation; the current vertical slice sends fixed-tick inputs and presents authority snapshots.
- Exercise forced DERP, route transitions, roaming, packet loss, and 16-peer churn.
- RustScale currently may report `portmapper cleanup remains uncertain` repeatedly on macOS close even though process exit releases local resources. Track this upstream.
