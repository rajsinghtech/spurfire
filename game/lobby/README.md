# Alpha lobby client integration status

This directory contains the smallest accountless one-lobby shell: Create Lobby, Join Lobby,
selected-lobby waiting room, peer enrollment/setup, Leave/End, truthful teardown, and an offline
Practice Range. It intentionally remains **closed** unless the service returns explicit
`real_lobby_creation_authorized` and `real_lobby_join_authorized` product fields. Provider probe
booleans never enable either action.

## Integrated control contract

The integration branch aligns the shell with the capability-protected control surface:

1. Public real create consumes an operator-issued one-use grant, sends the client UUID only as the
   creator subject, omits `provisioning_mode`, and receives a first-response-only creator capability.
2. The creator issues a one-use invitation. Every rider, including the creator, consumes an
   invitation and receives a first-response-only participant capability plus enrollment key.
3. Lobby reads return exact `network_generation`, monotonic `roster_revision`, and a
   capability-protected memory-only session endpoint projection. The peer bridge binds
   `PeerSession` to that exact roster and ignores unknown senders.
4. Each participant registers only a validated tailnet application endpoint and submits bounded
   route/RTT election measurements after actual peer probes. Raw endpoints are never rendered in
   the waiting-room roster.
5. Start carries the creator identifier but is authorized by `lobby.manage`; self-leave is
   authorized by the participant capability. Leave sends a peer Leave, waits up to one second for
   RustScale shutdown, then calls the control API. End Lobby reports cleanup as confirmed only when
   the exact network lifecycle says the resource is absent.

`res://lobby/tests/lobby_contract_test.tscn` is a credential-free contract smoke. It proves native
class availability, removal of the secret-taking Godot transport ABI, exact-roster sender rejection,
public control glue, unknown health behavior, cancellation, and truthful cleanup copy. It is not a
live provider, two-download, or human-play qualification.

## Native secret boundary

`PeerSession` now owns the fixed-origin rustls/WebPKI HTTPS client, creator/participant capabilities,
first-response parser buffers, and the direct move-only RustScale enrollment handoff. Secret input is
a Rust-backed masked `NativeSecretInput`; neither it nor lobby signals expose secret text through a
Godot `String`, `Variant`, dictionary, byte array, text control, or `DisplayServer` clipboard call.
Redirects and proxies are disabled, routes are closed, operations are bounded, and response bodies
are streamed into bounded zeroizing buffers.

Invitation sharing remains an explicit OS/human boundary. On supported Linux desktops the native
client reads an explicit paste and writes the one-use code directly through platform clipboard helpers
without routing either through Godot. A consumed paste is cleared immediately, and cancellation/exit
clears a copied value only when the current clipboard still exactly matches it. Clipboard managers/history,
IMEs, accessibility services, key hooks, compositor capture,
crash dumps, allocators, TLS internals, and the pinned RustScale builder may retain copies and cannot
be promised zeroized. Unsupported clipboard platforms fail closed; there is no claim of OS clipboard
zeroization. Product-readiness gates remain server-controlled and credentialed human qualification
is still required before activation.

## Invited-friends M2 source path

The signed bridge now admits and dispatches each datagram atomically in native code. The authority
consumes bounded subject-bound rider input, maintains distinct player-keyed rider/horse state, and
publishes subject-tagged snapshots; non-authorities render remote actors and reconcile their local
prediction. Native shooter-bound commands are delivered to the installed authority, resolved once
through `CombatAuthority`, and returned as authority-only epoch-bound results with transport and
combat-level deduplication. Authority loss no longer tears down the match: survivors advance one
epoch and install a hash-checked bounded movement/combat checkpoint before continuing.

This is **implemented source proof**, covered by credential-free Rust, Godot contract, and separate
OS-process gates. It is not credentialed/human evidence. Real create/join stays dark until credentialed qualification closes the server readiness gate, and route/quality, natural M2 tuning, packaged two-client,
and provider cleanup evidence remain required. No code here authorizes provider mutations,
publishes an artifact, or changes the release gate.
