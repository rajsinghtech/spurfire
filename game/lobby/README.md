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

`res://lobby/tests/lobby_contract_test.tscn` is a credential-free contract smoke. It proves strict
origin and join-code parsing, exact-roster sender rejection, required HTTP glue, no secret sink in
the adapter, unknown health behavior, and truthful cleanup copy. It is not a live provider,
two-download, or human-play qualification.

## Activation blocker: native secret handoff

Capabilities, invitations, and the first-response enrollment key are held only in memory, are never
logged or persisted, and are cleared on leave/exit. However, Godot `HTTPRequest` necessarily creates
GDScript `String` / `GString` copies before `PeerSession.connect_rustscale` moves the enrollment key
into zeroizing Rust memory. This does **not** satisfy the final direct native secret-handoff design.
Before real readiness can ever return true, replace `lobby_http_client.gd` with the planned native
`lobby_client.rs` fixed-origin HTTPS worker and feed the key directly into the RustScale worker.
Until then the explicit product-readiness gates must remain false.

## Invited-friends M2 source path

The signed bridge now admits and dispatches each datagram atomically in native code. The authority
consumes bounded subject-bound rider input, maintains distinct player-keyed rider/horse state, and
publishes subject-tagged snapshots; non-authorities render remote actors and reconcile their local
prediction. Native shooter-bound commands are delivered to the installed authority, resolved once
through `CombatAuthority`, and returned as authority-only epoch-bound results with transport and
combat-level deduplication. Authority loss no longer tears down the match: survivors advance one
epoch and install a hash-checked bounded movement/combat checkpoint before continuing.

This is **implemented source proof**, covered by credential-free Rust, Godot contract, and separate
OS-process gates. It is not credentialed/human evidence. Real create/join stays dark until the native
secret-handoff blocker above is closed, and route/quality, natural M2 tuning, packaged two-client,
and provider cleanup evidence remain required. No code here authorizes provider mutations,
publishes an artifact, or changes the release gate.
