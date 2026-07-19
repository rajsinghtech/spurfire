class_name SpurfireLobbyPeerBridge
extends Node

signal authority_departed

var peer_session: Node
var local_rider: CharacterBody3D
var remote_rider_template: Node3D
var combat_router: Node
var local_player_id := ""
var local_is_authority := false
var simulation_tick := 0

var _peers: Dictionary = {}
var _remote_riders: Dictionary = {}
var _last_route_query_ms := 0
var _session_binding_key := ""
var _authority_player_id := ""
var _quiesced := false
var _latest_inputs: Dictionary = {}
var _actor_states: Dictionary = {}
var _applied_shot_results: Dictionary = {}
var _migration_pending := false

func configure(nodes: Dictionary, player_id: String) -> bool:
	peer_session = nodes.get("peer_session") as Node
	local_rider = nodes.get("local_rider") as CharacterBody3D
	remote_rider_template = nodes.get("remote_rider") as Node3D
	combat_router = nodes.get("combat_router") as Node
	local_player_id = player_id
	if peer_session == null or local_rider == null or remote_rider_template == null:
		return false
	if combat_router:
		combat_router.call("set_networked_match", true)
		if combat_router.has_signal(&"network_shot_command"):
			combat_router.network_shot_command.connect(_on_local_shot_command)
	if not peer_session.packet_received.is_connected(_on_packet_received):
		peer_session.packet_received.connect(_on_packet_received)
	if not peer_session.route_updated.is_connected(_on_route_updated):
		peer_session.route_updated.connect(_on_route_updated)
	return true

func apply_projection(response: Dictionary) -> bool:
	var lobby_value = response.get("lobby", response)
	if not lobby_value is Dictionary:
		return false
	var lobby := lobby_value as Dictionary
	var lobby_id := str(lobby.get("lobby_id", ""))
	var authority_id := _authority_id(lobby)
	if authority_id.is_empty():
		# FORMING has no election yet. Binding to self is temporary and is
		# replaced from the exact control projection before match traffic.
		authority_id = local_player_id
	var roster_ids := PackedStringArray()
	var roster_value = lobby.get("roster", [])
	if not roster_value is Array:
		return false
	for value in roster_value:
		if value is Dictionary:
			roster_ids.append(str((value as Dictionary).get("player_id", "")))
	var sorted_roster := Array(roster_ids)
	sorted_roster.sort()
	# Authority changes are epoch transitions inside one signed session. They
	# must not recreate replay/liveness state.
	var binding_key := "%s|%s" % [lobby_id, ",".join(sorted_roster)]
	if binding_key != _session_binding_key:
		if not peer_session.configure_roster_session(
			lobby_id, local_player_id, authority_id, roster_ids, Time.get_ticks_msec()
		):
			return false
		_session_binding_key = binding_key
	_authority_player_id = authority_id
	local_is_authority = authority_id == local_player_id
	var projection_value = response.get("session", response.get("session_projection", {}))
	if not projection_value is Dictionary:
		return true
	var projection := projection_value as Dictionary
	var peers_value = projection.get("peers", [])
	if not peers_value is Array:
		return true
	var next_peers := {}
	for value in peers_value:
		if not value is Dictionary:
			continue
		var row := value as Dictionary
		var player_id := str(row.get("player_id", ""))
		if player_id.is_empty() or player_id == local_player_id:
			continue
		var address := str(row.get("tailnet_address", row.get("address", "")))
		var port := int(row.get("application_port", row.get("port", 0)))
		if address.is_empty() or port <= 0 or port > 65535:
			continue
		var existing := _peers.get(player_id, {}) as Dictionary
		next_peers[player_id] = {
			"address": address,
			"port": port,
			"route": str(existing.get("route", "unknown")),
			"rtt_ms": existing.get("rtt_ms", null),
			"last_seen_ms": int(existing.get("last_seen_ms", 0)),
		}
	# Projection removal may race ahead of the signed Leave packet. Clean all
	# gameplay and presentation state here so packet ordering is irrelevant.
	for previous_id: String in _peers.keys():
		if not next_peers.has(previous_id):
			_remove_peer(previous_id)
	_peers = next_peers
	if bool(projection.get("secure", false)):
		if not peer_session.configure_secure_session(
			lobby_id, JSON.stringify(projection), Time.get_ticks_msec()
		):
			return false
	return true

func _process(_delta: float) -> void:
	if peer_session == null or _peers.is_empty() or _quiesced:
		return
	var now := Time.get_ticks_msec()
	if _migration_pending:
		_attempt_migration(now)
	if now - _last_route_query_ms < 1000:
		return
	_last_route_query_ms = now
	for peer: Dictionary in _peers.values():
		peer_session.query_route(str(peer.address))

func advance_shared_tick(tick: int, stance_changed: bool = false) -> void:
	if _quiesced or tick <= simulation_tick or peer_session == null or local_rider == null:
		return
	simulation_tick = tick
	var packet := PackedByteArray()
	if local_is_authority:
		_record_actor_state(local_player_id, local_rider, tick)
		for player_id: String in _latest_inputs:
			_simulate_remote_actor(player_id, _latest_inputs[player_id] as Dictionary, tick)
		for player_id: String in _actor_states:
			_record_authority_combat_state(player_id, _actor_states[player_id] as Dictionary)
		if tick % 2 == 0 or stance_changed:
			for player_id: String in _actor_states:
				var state := _actor_states[player_id] as Dictionary
				var snapshot: PackedByteArray = peer_session.make_rider_snapshot(
					tick, player_id, state.position, state.velocity,
					float(state.yaw_degrees), int(state.stance_id)
				)
				if not snapshot.is_empty():
					_send_to_all(snapshot)
		elif tick % 6 == 0:
			packet = peer_session.make_heartbeat(tick)
	else:
		var throttle := roundi(Input.get_axis(&"move_back", &"move_forward") * 1000.0)
		var steer := roundi(Input.get_axis(&"steer_left", &"steer_right") * 1000.0)
		var buttons := 0
		if Input.is_action_just_pressed(&"jump"):
			buttons |= 1
		if Input.is_action_just_pressed(&"combat_interact"):
			buttons |= 2
		packet = peer_session.make_rider_input(tick, throttle, steer, buttons)
	if not packet.is_empty():
		_send_to_all(packet)
	if tick % 60 == 0:
		for peer: Dictionary in _peers.values():
			var probe: PackedByteArray = peer_session.make_probe(tick, Time.get_ticks_msec(), false)
			peer_session.send_packet(probe, str(peer.address), int(peer.port))

func send_leave() -> void:
	if peer_session == null or _quiesced:
		return
	_quiesced = true
	var packet: PackedByteArray = peer_session.make_leave(simulation_tick)
	if not packet.is_empty():
		_send_to_all(packet)

func measurement_report() -> Dictionary:
	if _peers.is_empty():
		return {}
	var direct := 0
	var peer_relay := 0
	var derp := 0
	var rtts: Array[int] = []
	for peer: Dictionary in _peers.values():
		var route := _route_name(str(peer.route))
		var rtt_value = peer.get("rtt_ms", null)
		if route == "unknown" or rtt_value == null:
			return {}
		match route:
			"direct": direct += 1
			"peer_relay": peer_relay += 1
			"derp_relay": derp += 1
			_: return {}
		rtts.append(maxi(0, int(rtt_value)))
	rtts.sort()
	var median := rtts[int(rtts.size() / 2)]
	return {
		"route_summary": {
			"direct_count": direct,
			"peer_relay_count": peer_relay,
			"derp_count": derp,
		},
		"rtt_ms_median": median,
		"rtt_ms_worst": rtts.back(),
		"jitter_ms": 0,
		"loss_pct_milli": 0,
		# Throughput/performance are not measured by this smallest Alpha slice;
		# conservative neutral values affect election only and are never shown as
		# player-visible network facts.
		"upload_mbps_sustained": 1,
		"device_perf_score": 500,
		"observed_peer_count": _peers.size(),
	}

func get_peer_status() -> Array:
	var rows: Array = [{
		"name": "you", "you": true, "authority": local_is_authority,
		"route": "LOCAL", "endpoint": "--", "rtt_ms": 0, "last_seen_ms": 0,
	}]
	var ids := _peers.keys()
	ids.sort()
	var now := Time.get_ticks_msec()
	for player_id: String in ids:
		var peer := _peers[player_id] as Dictionary
		rows.append({
			"name": player_id.left(8), "you": false,
			"authority": player_id == _authority_player_id,
			"route": str(peer.route),
			"endpoint": "%s:%d" % [str(peer.address), int(peer.port)],
			"rtt_ms": -1 if peer.rtt_ms == null else int(peer.rtt_ms),
			"last_seen_ms": -1 if int(peer.last_seen_ms) <= 0 else now - int(peer.last_seen_ms),
		})
	return rows

func peer_health() -> Dictionary:
	var result := {}
	var now := Time.get_ticks_msec()
	for player_id: String in _peers:
		var peer := _peers[player_id] as Dictionary
		var seen := int(peer.last_seen_ms)
		result[player_id] = {
			"route": _route_name(str(peer.route)),
			"rtt_ms": peer.rtt_ms,
			"freshness": "unknown" if seen <= 0 else ("fresh" if now - seen <= 15000 else "stale"),
		}
	return result

func _send_to_all(packet: PackedByteArray) -> void:
	for peer: Dictionary in _peers.values():
		peer_session.send_packet(packet, str(peer.address), int(peer.port))

func _on_packet_received(
	packet: PackedByteArray, source_ip: String, source_port: int, source_node_key: String
) -> void:
	if _quiesced:
		return
	var payload := peer_session.dispatch_packet_with_source(
		packet, source_ip, source_port, source_node_key, Time.get_ticks_msec()
	) as Dictionary
	if not bool(payload.get("accepted", false)):
		return
	var sender := str(payload.get("sender", ""))
	if not _peers.has(sender):
		return
	var peer := _peers[sender] as Dictionary
	peer.last_seen_ms = Time.get_ticks_msec()
	match str(payload.get("type", "")):
		"probe":
			if bool(payload.get("reply", false)):
				peer.rtt_ms = maxi(0, Time.get_ticks_msec() - int(payload.get("nonce", Time.get_ticks_msec())))
			else:
				var reply: PackedByteArray = peer_session.make_probe(
					simulation_tick, int(payload.get("nonce", 0)), true
				)
				peer_session.send_packet(reply, source_ip, source_port)
		"rider_input":
			if local_is_authority:
				_latest_inputs[sender] = {
					"tick": int(payload.get("tick", 0)),
					"throttle_milli": int(payload.get("throttle_milli", 0)),
					"steer_milli": int(payload.get("steer_milli", 0)),
					"buttons": int(payload.get("buttons", 0)),
				}
		"rider_snapshot":
			var subject := str(payload.get("rider_player_id", ""))
			if subject.is_empty():
				return
			_actor_states[subject] = payload.duplicate(true)
			if subject == local_player_id:
				var authoritative_position := payload.get("position", local_rider.global_position) as Vector3
				var correction := authoritative_position - local_rider.global_position
				# Reconcile local prediction: snap gross divergence, otherwise apply a
				# bounded correction while subsequent local inputs continue normally.
				local_rider.global_position += correction if correction.length() >= 2.0 else correction * 0.35
				local_rider.velocity = payload.get("velocity", local_rider.velocity) as Vector3
			else:
				var rider := _remote_rider_for(subject)
				if rider and rider.has_method("push_snapshot"):
					rider.push_snapshot(
						int(payload.get("tick", 0)), payload.get("position", Vector3.ZERO),
						payload.get("velocity", Vector3.ZERO), float(payload.get("yaw_degrees", 0.0)),
						int(payload.get("stance_id", 1))
					)
		"shot_command":
			if local_is_authority:
				_resolve_and_broadcast_command(payload)
		"shot_result":
			_apply_shot_result_once(payload)
		"migration_snapshot":
			_install_checkpoint(str(payload.get("checkpoint_json", "")))
			_authority_player_id = str(payload.get("authority", _authority_player_id))
			local_is_authority = _authority_player_id == local_player_id
			_migration_pending = false
		"leave":
			_remove_peer(sender)
			if sender == _authority_player_id:
				authority_departed.emit()

func _on_local_shot_command(tick: int, command_json: String) -> void:
	var payload := {"tick": tick, "command_json": command_json}
	if local_is_authority:
		_resolve_and_broadcast_command(payload)
	else:
		var packet: PackedByteArray = peer_session.make_shot_command(tick, command_json)
		if not packet.is_empty():
			_send_to_all(packet)

func _resolve_and_broadcast_command(payload: Dictionary) -> void:
	var result_json := str(peer_session.resolve_shot_command(str(payload.get("command_json", ""))))
	if result_json.is_empty():
		return
	var tick := int(payload.get("tick", 0))
	var result_packet: PackedByteArray = peer_session.make_shot_result(tick, result_json)
	if result_packet.is_empty():
		return
	_send_to_all(result_packet)
	var decoded = JSON.parse_string(result_json)
	var shooter := ""
	if decoded is Dictionary:
		shooter = str((decoded as Dictionary).get("shooter_peer_id", ""))
	_apply_shot_result_once({
		"authority_epoch": int(peer_session.get("authority_epoch")),
		"tick": tick, "result_json": result_json,
		"shooter_player_id": shooter,
	})

func _apply_shot_result_once(payload: Dictionary) -> void:
	var shot_key := "%s:%s:%s" % [
		str(payload.get("authority_epoch", 0)),
		str(payload.get("shooter_player_id", "")), str(payload.get("tick", 0))
	]
	if _applied_shot_results.has(shot_key):
		return
	_applied_shot_results[shot_key] = true
	if combat_router:
		combat_router.call("apply_network_shot_result", payload)

func begin_authority_migration() -> void:
	if not _quiesced:
		_migration_pending = true

func _attempt_migration(now_ms: int) -> void:
	var riders: Array = []
	for player_id: String in _actor_states:
		var state := _actor_states[player_id] as Dictionary
		var combat := peer_session.combat_checkpoint_state(player_id) as Dictionary
		if combat.is_empty():
			# Never invent a loadout or refill/empty ammo during authority handoff.
			return
		var last_shot := int(combat.get("last_shot_tick", -1))
		var last_command := int(combat.get("last_command_tick", -1))
		riders.append({
			"rider_player_id": player_id,
			"position_mm": _vector_mm(state.position as Vector3),
			"velocity_mmps": _vector_mm(state.velocity as Vector3),
			"yaw_millidegrees": roundi(float(state.yaw_degrees) * 1000.0),
			"stance": int(state.stance_id), "health": int(state.get("health", 100)),
			"weapon_id": int(combat.weapon_id),
			"ammo_magazine": int(combat.ammo_magazine),
			"ammo_reserve": int(combat.ammo_reserve),
			"last_input_tick": int(state.tick),
			"last_shot_tick": null if last_shot < 0 else last_shot,
			"last_command_tick": null if last_command < 0 else last_command,
			"shot_index": int(combat.shot_index),
		})
	if riders.is_empty():
		_record_actor_state(local_player_id, local_rider, simulation_tick)
		return
	var resolved_value = JSON.parse_string(str(peer_session.combat_resolved_shots_json()))
	if not resolved_value is Array:
		return
	var checkpoint := {
		"source_epoch": int(peer_session.get("authority_epoch")),
		"tick": simulation_tick, "riders": riders, "resolved_shots": resolved_value,
	}
	var packet: PackedByteArray = peer_session.poll_migration(
		now_ms, JSON.stringify(checkpoint)
	)
	_authority_player_id = str(peer_session.get("authority_player_id"))
	local_is_authority = _authority_player_id == local_player_id
	if not packet.is_empty():
		_send_to_all(packet)
		_migration_pending = false

func _install_checkpoint(checkpoint_json: String) -> void:
	var decoded = JSON.parse_string(checkpoint_json)
	if not decoded is Dictionary:
		return
	var rows = (decoded as Dictionary).get("r", [])
	if not rows is Array:
		return
	for value in rows:
		if not value is Dictionary:
			continue
		var row := value as Dictionary
		var player_id := str(row.get("p", ""))
		var position := row.get("x", []) as Array
		var velocity := row.get("v", []) as Array
		if player_id.is_empty() or position.size() != 3 or velocity.size() != 3:
			continue
		_actor_states[player_id] = {
			"tick": int((decoded as Dictionary).get("t", simulation_tick)),
			"position": Vector3(float(position[0]), float(position[1]), float(position[2])) / 1000.0,
			"velocity": Vector3(float(velocity[0]), float(velocity[1]), float(velocity[2])) / 1000.0,
			"yaw_degrees": float(row.get("y", 0)) / 1000.0,
			"stance_id": int(row.get("s", 1)),
		}

func _vector_mm(value: Vector3) -> Array[int]:
	return [roundi(value.x * 1000.0), roundi(value.y * 1000.0), roundi(value.z * 1000.0)]

func latest_input_for(player_id: String) -> Dictionary:
	return (_latest_inputs.get(player_id, {}) as Dictionary).duplicate(true)

func actor_states() -> Dictionary:
	return _actor_states.duplicate(true)

func _record_actor_state(player_id: String, rider: CharacterBody3D, tick: int) -> void:
	_actor_states[player_id] = {
		"tick": tick, "position": rider.global_position, "velocity": rider.velocity,
		"yaw_degrees": rad_to_deg(rider.rotation.y), "stance_id": int(rider.get("stance_id")),
	}

func _record_authority_combat_state(player_id: String, state: Dictionary) -> void:
	var muzzle := state.position as Vector3
	if player_id == local_player_id:
		var controller := local_rider.get_node_or_null("WeaponController") as Node3D
		if controller:
			muzzle = controller.global_position
	else:
		# Canonical authority-owned proxy muzzle. It is derived only from the
		# simulated actor transform, never from the client's ShotCommand origin.
		var offset := Vector3(0.52, 1.12, -0.18).rotated(
			Vector3.UP, deg_to_rad(float(state.yaw_degrees))
		)
		muzzle += offset
	peer_session.record_authority_rider_snapshot(
		player_id, int(state.tick), muzzle, state.velocity as Vector3, int(state.stance_id)
	)

func _simulate_remote_actor(player_id: String, input: Dictionary, tick: int) -> void:
	if int(input.get("tick", -1)) > tick or tick - int(input.get("tick", -1)) > 6:
		return
	var state := _actor_states.get(player_id, {
		"tick": tick - 1, "position": Vector3.ZERO, "velocity": Vector3.ZERO,
		"yaw_degrees": 0.0, "stance_id": 1,
	}) as Dictionary
	var throttle := clampf(float(input.get("throttle_milli", 0)) / 1000.0, -1.0, 1.0)
	var steer := clampf(float(input.get("steer_milli", 0)) / 1000.0, -1.0, 1.0)
	var yaw := float(state.yaw_degrees) + steer * 90.0 / 60.0
	var forward := Vector3.FORWARD.rotated(Vector3.UP, deg_to_rad(yaw))
	var velocity := forward * throttle * 13.0
	var position := (state.position as Vector3) + velocity / 60.0
	var stance := int(state.stance_id)
	var buttons := int(input.get("buttons", 0))
	if buttons & 1 and stance == 1:
		stance = 2
	if buttons & 2:
		stance = 6 if stance == 1 else (1 if stance == 6 else stance)
	_actor_states[player_id] = {
		"tick": tick, "position": position, "velocity": velocity,
		"yaw_degrees": yaw, "stance_id": stance,
	}

func _on_route_updated(peer_ip: String, route: String) -> void:
	for peer: Dictionary in _peers.values():
		if str(peer.address) == peer_ip:
			peer.route = route
			return

func _remove_peer(player_id: String) -> void:
	_peers.erase(player_id)
	_latest_inputs.erase(player_id)
	_actor_states.erase(player_id)
	if _remote_riders.has(player_id):
		var departing := _remote_riders[player_id] as Node3D
		_remote_riders.erase(player_id)
		departing.visible = false
		departing.queue_free()

func _remote_rider_for(player_id: String) -> Node3D:
	if _remote_riders.has(player_id):
		return _remote_riders[player_id] as Node3D
	# Keep the scene node as a hidden prototype. Reusing it would retain a
	# departed rider's native interpolation buffer across roster revisions.
	var rider := remote_rider_template.duplicate() as Node3D
	remote_rider_template.get_parent().add_child(rider)
	rider.visible = true
	_remote_riders[player_id] = rider
	return rider

func _authority_id(lobby: Dictionary) -> String:
	var authority_value = lobby.get("authority", {})
	if authority_value is Dictionary:
		return str((authority_value as Dictionary).get("candidate_player_id", ""))
	return ""

func _route_name(route: String) -> String:
	var normalized := route.to_lower()
	if normalized.contains("direct"):
		return "direct"
	if normalized.contains("relay") and not normalized.contains("derp"):
		return "peer_relay"
	if normalized.contains("derp"):
		return "derp_relay"
	return "unknown"
