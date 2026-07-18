# Alpha lobby client integration status

This directory contains the smallest accountless one-lobby shell: Create Lobby, Join Lobby,
selected-lobby waiting room, peer enrollment/setup, Leave/End, truthful teardown, and an offline
Practice Range. It intentionally remains **closed** unless the service returns explicit
`real_lobby_creation_authorized` / `real_lobby_join_authorized` product fields. Provider probe
booleans from the current `/v1/capabilities` DTO never enable either action.

## Control-contract mismatches at `origin/main` 7868860

The client consumes the current additive JSON shapes where they exist, but these missing server /
protocol interfaces prevent an end-to-end managed lobby at this revision:

1. `CapabilitiesResponse` has provider evidence only and no product-readiness fields. The shell
   therefore displays both actions but keeps them disabled.
2. `CreateLobbyRequest` requires client-selected `provisioning_mode`; the accepted public contract
   requires the server to select `tailnet_per_lobby`. This client omits the field and cannot call the
   current create route successfully.
3. `CreateLobbyResponse` does not model the server's one-time creator capability wrapper.
4. Invitation preview/issue/consume DTOs and `POST .../invitations` are absent.
5. `JoinLobbyResponse` has an enrollment key but no participant capability, roster/session
   generation, or peer endpoint/public-key projection.
6. No endpoint registration/session projection or participant network-report DTO/route exists.
7. `LobbyNetworkView` is aggregate-only. It cannot provide a per-roster directional route/RTT row;
   the UI uses local peer probes for per-rider values and leaves missing values `unknown`.
8. Current start/leave/destroy mutations still use asserted player headers rather than the target
   manage/leave-self/destroy capabilities.

The HTTP adapter accepts the additive target fields so integration can proceed once protocol and
control owners land them, without editing server/control-owned files in this workstream.

## Activation blocker: native secret handoff

Capabilities, invitations, and the first-response enrollment key are held only in memory, are never
logged or persisted, and are cleared on leave/exit. However, Godot `HTTPRequest` necessarily creates
GDScript `String` / `GString` copies before `PeerSession.connect_rustscale` moves the enrollment key
into zeroizing Rust memory. This does **not** satisfy the final direct native secret-handoff design.
Before real readiness can ever return true, replace `lobby_http_client.gd` with the planned native
`lobby_client.rs` fixed-origin HTTPS worker and feed the key directly into the RustScale worker.
Until then the explicit product-readiness gate must remain false.

No code here authorizes hosted real mutations, adds control-service gameplay membership, publishes
an artifact, or changes the `v0.2.0` release gate.
