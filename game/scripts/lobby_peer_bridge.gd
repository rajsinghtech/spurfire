class_name SpurfireLobbyPeerBridge
extends Node

signal authority_departed

var peer_session: Node
var local_rider: CharacterBody3D
var local_horse: CharacterBody3D
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
var _last_migration_poll_ms := 0

const M3_INPUT_JUMP := 1 << 0
const M3_INPUT_INTERACT := 1 << 1
const M3_INPUT_SPRINT := 1 << 2
const M3_INPUT_CROUCH := 1 << 3
const M3_INPUT_RELOAD := 1 << 4
const M3_INPUT_ADS := 1 << 5

func configure(nodes: Dictionary, player_id: String) -> bool:
	peer_session = nodes.get("peer_session") as Node
	local_rider = nodes.get("local_rider") as CharacterBody3D
	local_horse = nodes.get("local_horse") as CharacterBody3D
	remote_rider_template = nodes.get("remote_rider") as Node3D
	combat_router = nodes.get("combat_router") as Node
	local_player_id = player_id
	if peer_session == null or local_rider == null or local_horse == null or remote_rider_template == null:
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
		var loadouts := _m3_loadouts(roster_value)
		if loadouts.is_empty() or not peer_session.activate_m3_wire(JSON.stringify(loadouts)):
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
	# A crashed authority cannot send Leave. Poll the native liveness/election
	# gate on every follower so timeout alone can trigger deterministic migration.
	if (_migration_pending or not local_is_authority) and now - _last_migration_poll_ms >= 250:
		_last_migration_poll_ms = now
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
	if peer_session.is_m3_wire_active():
		_advance_m3_tick(tick, stance_changed)
		_send_periodic_probe(tick)
		return
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
	_send_periodic_probe(tick)

func _send_periodic_probe(tick: int) -> void:
	if tick % 60 != 0:
		return
	for peer: Dictionary in _peers.values():
		var probe: PackedByteArray = peer_session.make_probe(tick, Time.get_ticks_msec(), false)
		peer_session.send_packet(probe, str(peer.address), int(peer.port))

func _advance_m3_tick(tick: int, stance_changed: bool) -> void:
	var local_input := _sample_m3_input()
	if not local_is_authority:
		var packet: PackedByteArray = peer_session.make_m3_actor_input(
			tick,
			int(local_input.throttle_milli), int(local_input.steer_milli),
			int(local_input.move_x_milli), int(local_input.move_z_milli),
			int(local_input.buttons)
		)
		if not packet.is_empty():
			_send_to_all(packet)
		return

	_record_actor_state(local_player_id, local_rider, tick)
	var local_state := _actor_states[local_player_id] as Dictionary
	local_state["horse_position"] = local_horse.global_position
	local_state["horse_velocity"] = local_horse.velocity
	local_state["horse_yaw_degrees"] = rad_to_deg(local_horse.rotation.y)
	for player_id: String in _peers:
		var input := _latest_inputs.get(player_id, _zero_m3_input()) as Dictionary
		_simulate_remote_actor(player_id, input, tick)
		var state := _actor_states[player_id] as Dictionary
		state["horse_position"] = state.position
		state["horse_velocity"] = state.velocity
		state["horse_yaw_degrees"] = float(state.yaw_degrees)

	var ids := _actor_states.keys()
	ids.sort()
	for player_id: String in ids:
		var state := _actor_states[player_id] as Dictionary
		var input := local_input if player_id == local_player_id else (
			_latest_inputs.get(player_id, _zero_m3_input()) as Dictionary
		)
		var buttons := int(input.get("buttons", 0))
		var movement := Vector2(
			float(input.get("move_x_milli", 0)) / 1000.0,
			float(input.get("move_z_milli", 0)) / 1000.0
		)
		var actor_tick := peer_session.advance_m3_actor(
			player_id, tick, movement,
			buttons & M3_INPUT_SPRINT != 0, buttons & M3_INPUT_CROUCH != 0,
			buttons & M3_INPUT_RELOAD != 0, buttons & M3_INPUT_INTERACT != 0,
			state.position, state.horse_position, (state.horse_velocity as Vector3).length() > 0.1
		) as Dictionary
		if not bool(actor_tick.get("advanced", false)):
			continue
		_record_authority_combat_state(player_id, state)
		_record_m3_horse_state(player_id, state, tick)
		if tick % 2 == 0 or (player_id == local_player_id and stance_changed):
			var snapshot: PackedByteArray = peer_session.make_m3_actor_snapshot_from_pose(
				tick, player_id, state.position, state.velocity,
				float(state.yaw_degrees), int(state.stance_id),
				state.horse_position, state.horse_velocity,
				float(state.horse_yaw_degrees)
			)
			if not snapshot.is_empty():
				_send_to_all(snapshot)

func _sample_m3_input() -> Dictionary:
	var throttle := roundi(Input.get_axis(&"move_back", &"move_forward") * 1000.0)
	var steer := roundi(Input.get_axis(&"steer_left", &"steer_right") * 1000.0)
	var buttons := 0
	if Input.is_action_just_pressed(&"jump"):
		buttons |= M3_INPUT_JUMP
	if Input.is_action_just_pressed(&"combat_interact"):
		buttons |= M3_INPUT_INTERACT
	if Input.is_action_pressed(&"on_foot_sprint"):
		buttons |= M3_INPUT_SPRINT
	if Input.is_action_pressed(&"on_foot_crouch"):
		buttons |= M3_INPUT_CROUCH
	if Input.is_action_pressed(&"combat_reload"):
		buttons |= M3_INPUT_RELOAD
	if Input.is_action_pressed(&"combat_aim"):
		buttons |= M3_INPUT_ADS
	var on_foot_move := Vector2(float(steer), float(-throttle))
	if on_foot_move.length() > 1000.0:
		on_foot_move = on_foot_move.normalized() * 1000.0
	return {
		"throttle_milli": throttle, "steer_milli": steer,
		"move_x_milli": roundi(on_foot_move.x), "move_z_milli": roundi(on_foot_move.y),
		"buttons": buttons,
	}

func _zero_m3_input() -> Dictionary:
	return {
		"throttle_milli": 0, "steer_milli": 0,
		"move_x_milli": 0, "move_z_milli": 0, "buttons": 0,
	}

func _record_m3_horse_state(player_id: String, state: Dictionary, tick: int) -> void:
	var yaw := deg_to_rad(float(state.horse_yaw_degrees))
	var forward := Vector3.FORWARD.rotated(Vector3.UP, yaw)
	var center := (state.horse_position as Vector3) + Vector3.UP * 0.9
	peer_session.record_m3_horse_pose(
		player_id, tick, center, forward, center + Vector3.UP * 0.65 + forward * 0.9
	)

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
		"actor_input":
			if local_is_authority:
				var decoded = JSON.parse_string(str(payload.get("input_json", "")))
				if decoded is Dictionary:
					var input := decoded as Dictionary
					_latest_inputs[sender] = {
						"tick": int(payload.get("tick", 0)),
						"throttle_milli": int(input.get("t", input.get("throttle_milli", 0))),
						"steer_milli": int(input.get("s", input.get("steer_milli", 0))),
						"move_x_milli": int(input.get("x", input.get("move_x_milli", 0))),
						"move_z_milli": int(input.get("z", input.get("move_z_milli", 0))),
						"buttons": int(input.get("b", input.get("buttons", 0))),
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
		"actor_snapshot":
			_apply_m3_snapshot(str(payload.get("snapshot_json", "")))
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
		"migration_fragment", "authority":
			_authority_player_id = str(peer_session.get("authority_player_id"))
			local_is_authority = _authority_player_id == local_player_id
			if int(peer_session.get("authority_epoch")) >= int(payload.get("authority_epoch", 0)):
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
	var result_json := ""
	if peer_session.is_m3_wire_active():
		var resolved := peer_session.resolve_m3_shot_command(
			str(payload.get("command_json", "")), simulation_tick
		) as Dictionary
		result_json = str(resolved.get("result_json", ""))
	else:
		result_json = str(peer_session.resolve_shot_command(str(payload.get("command_json", ""))))
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
			"stance": int(state.stance_id), "health": int(combat.health),
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
	var packets: Array[PackedByteArray] = []
	if peer_session.is_m3_wire_active():
		for value in peer_session.poll_m3_migration(now_ms, JSON.stringify(checkpoint)):
			packets.append(value as PackedByteArray)
	else:
		var packet: PackedByteArray = peer_session.poll_migration(
			now_ms, JSON.stringify(checkpoint)
		)
		if not packet.is_empty():
			packets.append(packet)
	_authority_player_id = str(peer_session.get("authority_player_id"))
	local_is_authority = _authority_player_id == local_player_id
	if not packets.is_empty():
		for packet: PackedByteArray in packets:
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
			"health": int(row.get("h", 0)),
		}

func _apply_m3_snapshot(snapshot_json: String) -> void:
	var decoded = JSON.parse_string(snapshot_json)
	if not decoded is Dictionary:
		return
	var row := decoded as Dictionary
	var player_id := str(row.get("i", row.get("rider_player_id", "")))
	var position = row.get("p", row.get("rider_position_mm", []))
	var velocity = row.get("v", row.get("rider_velocity_mmps", []))
	var horse_value = row.get("o", row.get("horse", {}))
	if (
		player_id.is_empty() or not position is Array or (position as Array).size() != 3
		or not velocity is Array or (velocity as Array).size() != 3
		or not horse_value is Dictionary
	):
		return
	var horse := horse_value as Dictionary
	var horse_position = horse.get("p", horse.get("position_mm", []))
	var horse_velocity = horse.get("v", horse.get("velocity_mmps", []))
	if (
		not horse_position is Array or (horse_position as Array).size() != 3
		or not horse_velocity is Array or (horse_velocity as Array).size() != 3
	):
		return
	var rider_position := _mm_vector(position as Array)
	var rider_velocity := _mm_vector(velocity as Array)
	var stance_name := str(row.get("s", row.get("stance", "on_foot_standing")))
	var stance_id := _legacy_stance_from_m3(stance_name)
	_actor_states[player_id] = {
		"tick": int(row.get("tick", simulation_tick)),
		"position": rider_position, "velocity": rider_velocity,
		"yaw_degrees": float(row.get("y", row.get("rider_yaw_millidegrees", 0))) / 1000.0,
		"stance_id": stance_id, "dive_id": -1,
		"horse_position": _mm_vector(horse_position as Array),
		"horse_velocity": _mm_vector(horse_velocity as Array),
		"horse_yaw_degrees": float(horse.get("y", horse.get("yaw_millidegrees", 0))) / 1000.0,
		"m3_stance": stance_name,
		"horse_health": int(horse.get("h", horse.get("health", 0))),
		"horse_state": str(horse.get("s", horse.get("state", "despawned"))),
	}
	if player_id == local_player_id:
		var correction := rider_position - local_rider.global_position
		local_rider.global_position += correction if correction.length() >= 2.0 else correction * 0.35
		local_rider.velocity = rider_velocity
	else:
		var rider := _remote_rider_for(player_id)
		if rider and rider.has_method("push_snapshot"):
			rider.push_snapshot(
			simulation_tick, rider_position, rider_velocity,
			float(_actor_states[player_id].yaw_degrees), stance_id
		)

func _mm_vector(values: Array) -> Vector3:
	return Vector3(float(values[0]), float(values[1]), float(values[2])) / 1000.0

func _legacy_stance_from_m3(stance: String) -> int:
	match stance:
		"mounted": return 1
		"mounted_airborne": return 2
		"saddle_dive_airborne": return 3
		"landing_prone": return 4
		"landing_recovery": return 5
		_: return 6

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
		"dive_id": int(rider.get("dive_id")),
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
		player_id, int(state.tick), muzzle, state.position as Vector3,
		state.velocity as Vector3,
		PackedInt64Array([int(state.stance_id), int(state.get("dive_id", -1))])
	)

func _simulate_remote_actor(player_id: String, input: Dictionary, tick: int) -> void:
	var input_tick := int(input.get("tick", -1))
	if input_tick > tick:
		return
	var stale := input_tick < 0 or tick - input_tick > 6
	var state := _actor_states.get(player_id, {
		"tick": tick - 1, "position": Vector3.ZERO, "velocity": Vector3.ZERO,
		"yaw_degrees": 0.0, "stance_id": 1, "dive_id": -1,
	}) as Dictionary
	var throttle := 0.0 if stale else clampf(float(input.get("throttle_milli", 0)) / 1000.0, -1.0, 1.0)
	var steer := 0.0 if stale else clampf(float(input.get("steer_milli", 0)) / 1000.0, -1.0, 1.0)
	var yaw := float(state.yaw_degrees) + steer * 90.0 / 60.0
	var forward := Vector3.FORWARD.rotated(Vector3.UP, deg_to_rad(yaw))
	var velocity := forward * throttle * 13.0
	var position := (state.position as Vector3) + velocity / 60.0
	var stance := int(state.stance_id)
	var buttons := 0 if stale else int(input.get("buttons", 0))
	if buttons & 1 and stance == 1:
		stance = 2
	if buttons & 2:
		stance = 6 if stance == 1 else (1 if stance == 6 else stance)
	_actor_states[player_id] = {
		"tick": tick, "position": position, "velocity": velocity,
		"yaw_degrees": yaw, "stance_id": stance,
		"dive_id": int(state.get("dive_id", -1)),
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

func _m3_loadouts(roster: Array) -> Array[Dictionary]:
	var rows: Array[Dictionary] = []
	for value in roster:
		if not value is Dictionary:
			return []
		var player := value as Dictionary
		var player_id := str(player.get("player_id", ""))
		var horse_class := str(player.get("horse_selection", ""))
		if player_id.is_empty() or horse_class not in ["courser", "warhorse", "mustang"]:
			return []
		rows.append({
			"player_id": player_id,
			"horse_class": horse_class,
			"weapon_id": "dustwalker",
		})
	rows.sort_custom(func(a: Dictionary, b: Dictionary) -> bool:
		return str(a.player_id) < str(b.player_id)
	)
	return rows

func _route_name(route: String) -> String:
	var normalized := route.to_lower()
	if normalized.contains("direct"):
		return "direct"
	if normalized.contains("relay") and not normalized.contains("derp"):
		return "peer_relay"
	if normalized.contains("derp"):
		return "derp_relay"
	return "unknown"
