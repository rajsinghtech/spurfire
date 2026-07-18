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

The smallest bridge distributes endpoints and authoritative snapshots, but it does not yet
simulate distinct remote horses from rider inputs or network authoritative combat results. It
therefore cannot qualify one coherent peer-authoritative M2 match. No code here authorizes hosted
real mutations, adds control-service gameplay membership, publishes an artifact, or changes the
`v0.2.0` release gate.
