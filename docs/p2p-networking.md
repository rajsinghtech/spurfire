# Peer gameplay networking

Spurfire's native gameplay data plane uses application UDP through embedded RustScale. The integration is pinned to RustScale revision `8511e0b78074bf07b59d53cf1a2eb349cd0d2407`; do not update it without rerunning the live lifecycle probe.

## Components

- `crates/spurfire-net` defines transport-independent, 1,200-byte-bounded envelopes.
- Envelopes carry heartbeats, rider input/snapshots, shot commands/results, authority announcements, and migration snapshots.
- `SessionState` rejects replayed/out-of-order sequences, wrong-lobby traffic, and stale authority epochs. Silent authority loss elects the lowest connected `PlayerId` and advances the epoch.
- `spurfire_net::rustscale::RustScalePeer` enrolls an ephemeral node with a one-use auth key, clears the key after `up()`, binds `Server::listen_packet`, and sends/receives through `UdpListener`.
- Godot's native `PeerSession` node owns RustScale on a background Tokio runtime and moves packets to the main thread through signals. OAuth credentials remain control-plane-only; this class accepts only a narrow join auth key.

The C ABI still has no gameplay UDP API. Spurfire does not use it: the Rust GDExtension links the native Rust `tsnet` API directly.

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
5. Mints two non-reusable, ephemeral, preauthorized 15-minute keys.
6. Enrolls two independent embedded RustScale servers.
7. Exchanges a bounded Spurfire Hello and rider-input frame in both directions over application UDP.
8. Reports RustScale's route class.
9. Closes both nodes and exactly deletes the child tailnet under an exit trap.

On 2026-07-17, the successful probe reported two distinct `100.x` peer addresses and `Direct` in both directions, then printed both `SPURFIRE_P2P_UDP_OK` and `SPURFIRE_P2P_LIFECYCLE_OK`. The child tailnet `tail894241.ts.net` was deleted. Two earlier development probes (`tail63435f.ts.net` and `tailcf3680.ts.net`) also exchanged direct UDP and were deleted; they exposed RustScale's retryable macOS port-mapper shutdown uncertainty, which the smoke now retries and treats as a local teardown warning only after tailnet traffic has succeeded.

## Security boundaries

- Never pass organization or child OAuth credentials to Godot.
- Never print auth keys. The live script writes them only to its private temporary directory and deletes that directory on every exit path.
- Datagram size is checked before JSON parsing and before send.
- Application identity is the validated lobby/player ID in the envelope, not an untrusted hostname.
- Sequence rejection limits replay within a peer session; authority epochs prevent a disconnected old host from resuming authority.

## Remaining production work

- Drive `PeerSession` from the lobby join HTTP flow and distribute peer endpoints.
- Add authenticated session-level packet tags if tailnet membership alone is not sufficient for the final threat model.
- Add snapshot interpolation/prediction and wire the mounted controller's fixed-tick input/state loop.
- Exercise forced DERP, route transitions, roaming, packet loss, 16-peer churn, and authority migration with multiple game processes.
- RustScale currently may report `portmapper cleanup remains uncertain` repeatedly on macOS close even though process exit releases local resources. Track this upstream.
