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
var _remote_horses: Dictionary = {}
var _last_route_query_ms := 0
var _session_binding_key := ""
var _authority_player_id := ""
var _quiesced := false
var _latest_inputs: Dictionary = {}
var _actor_states: Dictionary = {}
var _applied_shot_results: Dictionary = {}
var _migration_pending := false
var _last_migration_poll_ms := 0
var _jump_buffer_until_tick := -1
var _crouch_buffer_until_tick := -1
var _m3_metrics: Dictionary = {}
var _m3_actor_slots: Dictionary = {}
var _m3_horse_classes: Dictionary = {}
var _m3_telemetry_file: FileAccess
var _m3_telemetry_session := ""
var _m3_telemetry_closed := false
var _m5_state: Dictionary = {}

const M3_INPUT_JUMP := 1 << 0
const M3_INPUT_INTERACT := 1 << 1
const M3_INPUT_SPRINT := 1 << 2
const M3_INPUT_CROUCH := 1 << 3
const M3_INPUT_RELOAD := 1 << 4
const M3_INPUT_ADS := 1 << 5
const M3_INPUT_SPUR := 1 << 6
const M3_INPUT_BUFFER_TICKS := 9
const M3_BOLT_SPEED_MPS := 12.0
const M3_RETURN_SPAWN_DISTANCE_M := 60.0
const M3_RETURN_STOP_DISTANCE_M := 3.0
const M3_GALLOP_IN_TICKS := 180
const M3_MOUNT_WINDOW_TICKS := 90

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
	_open_m3_telemetry()
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
	_m3_actor_slots.clear()
	_m3_horse_classes.clear()
	for index in range(sorted_roster.size()):
		_m3_actor_slots[str(sorted_roster[index])] = index
	for value in roster_value:
		if value is Dictionary:
			var roster_player := value as Dictionary
			_m3_horse_classes[str(roster_player.get("player_id", ""))] = str(
				roster_player.get("horse_selection", "courser")
			)
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
	var was_authority := local_is_authority
	_authority_player_id = authority_id
	local_is_authority = authority_id == local_player_id
	if was_authority and not local_is_authority:
		_flush_m3_metrics(simulation_tick, true)
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
	var local_input := _sample_m3_input(tick)
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
		if not state.has("horse_position"):
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
			buttons & M3_INPUT_SPUR != 0, int(state.stance_id) in [1, 2],
			state.position, state.horse_position, (state.horse_velocity as Vector3).length() > 0.1
		) as Dictionary
		if not bool(actor_tick.get("advanced", false)):
			continue
		state["reload_active_ticks"] = int(actor_tick.get("reload_active_ticks", 0))
		state["reload_required_ticks"] = int(actor_tick.get("reload_required_ticks", 0))
		state["reload_paused"] = bool(actor_tick.get("reload_pause_started", false))
		state["spur_meter"] = int(actor_tick.get("spur_meter", 0))
		state["charge_active"] = bool(actor_tick.get("charge_active", false))
		state["charge_started_tick"] = int(actor_tick.get("charge_started_tick", -1))
		state["charge_end_tick"] = int(actor_tick.get("charge_end_tick", -1))
		if player_id == local_player_id and local_horse.has_method("set_majestic_charge_active"):
			local_horse.call("set_majestic_charge_active", bool(state.charge_active))
		if player_id == local_player_id:
			_apply_local_m4_presentation(state, tick)
		if bool(actor_tick.get("on_foot_active", false)):
			state["stance_id"] = 6
			var on_foot_velocity := actor_tick.get("on_foot_velocity", Vector2.ZERO) as Vector2
			var resolved_velocity := Vector3(on_foot_velocity.x, 0.0, on_foot_velocity.y)
			if player_id != local_player_id:
				# Replace the provisional mounted proxy step with the native
				# stance-curve step for this exact authority tick.
				var provisional_velocity := state.velocity as Vector3
				state["position"] = (
					(state.position as Vector3)
					- provisional_velocity / 60.0 + resolved_velocity / 60.0
				)
				state["velocity"] = resolved_velocity
		_update_authority_horse_presentation(player_id, state, actor_tick, tick)
		_record_m4_movement_style(player_id, state, tick)
		_record_m3_actor_tick(player_id, actor_tick, state, input, tick)
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
	var match_tick := peer_session.advance_m5_match(tick) as Dictionary
	if bool(match_tick.get("advanced", false)):
		_m5_state = match_tick
	_flush_m3_metrics(tick, false)

func _update_authority_horse_presentation(
	player_id: String, state: Dictionary, actor_tick: Dictionary, tick: int
) -> void:
	var horse_state := int(actor_tick.get("horse_state_id", 0))
	var recall_state := int(actor_tick.get("recall_state_id", 0))
	state["horse_state_id"] = horse_state
	state["recall_state_id"] = recall_state
	if horse_state == 0:
		state.erase("horse_bolt_origin")
		state.erase("horse_bolt_started_tick")
		if player_id != local_player_id:
			state["horse_position"] = state.position
			state["horse_velocity"] = state.velocity
			state["horse_yaw_degrees"] = float(state.yaw_degrees)
		return
	if horse_state == 1:
		var bolt_started := int(actor_tick.get("horse_bolt_started_tick", tick))
		if int(state.get("horse_bolt_started_tick", -1)) != bolt_started:
			state["horse_bolt_started_tick"] = bolt_started
			state["horse_bolt_origin"] = state.horse_position
		var planar := actor_tick.get("horse_bolt_direction", Vector2(0.0, -1.0)) as Vector2
		if planar.length_squared() <= 0.000001:
			planar = Vector2(0.0, -1.0)
		planar = planar.normalized()
		var elapsed_seconds := float(maxi(0, tick - bolt_started)) / 60.0
		var bolt_velocity := Vector3(planar.x, 0.0, planar.y) * M3_BOLT_SPEED_MPS
		state["horse_position"] = (state.horse_bolt_origin as Vector3) + bolt_velocity * elapsed_seconds
		state["horse_velocity"] = bolt_velocity
		state["horse_yaw_degrees"] = rad_to_deg(atan2(-bolt_velocity.x, -bolt_velocity.z))
		return

	state["horse_velocity"] = Vector3.ZERO
	if recall_state < 4:
		return
	var rider_forward := Vector3.FORWARD.rotated(
		Vector3.UP, deg_to_rad(float(state.yaw_degrees))
	)
	var spawn := (state.position as Vector3) - rider_forward * M3_RETURN_SPAWN_DISTANCE_M
	var destination := (state.position as Vector3) - rider_forward * M3_RETURN_STOP_DISTANCE_M
	if recall_state == 4:
		state["horse_position"] = spawn
		state["horse_yaw_degrees"] = float(state.yaw_degrees)
		return
	if recall_state == 7:
		state["horse_position"] = destination
		state["horse_yaw_degrees"] = float(state.yaw_degrees)
		return
	var phase_enter := int(actor_tick.get("recall_phase_enter_tick", tick))
	var elapsed := maxi(0, tick - phase_enter)
	if recall_state == 6:
		elapsed += M3_GALLOP_IN_TICKS - M3_MOUNT_WINDOW_TICKS
	var alpha := clampf(float(elapsed) / float(M3_GALLOP_IN_TICKS), 0.0, 1.0)
	state["horse_position"] = spawn.lerp(destination, alpha)
	state["horse_velocity"] = rider_forward * (
		(M3_RETURN_SPAWN_DISTANCE_M - M3_RETURN_STOP_DISTANCE_M)
		/ (float(M3_GALLOP_IN_TICKS) / 60.0)
	)
	state["horse_yaw_degrees"] = float(state.yaw_degrees)

func _open_m3_telemetry() -> void:
	var logs_path := ProjectSettings.globalize_path("user://logs")
	var error := DirAccess.make_dir_recursive_absolute(logs_path)
	if error != OK:
		push_warning("M3 telemetry log directory unavailable: %s" % error_string(error))
		return
	_m3_telemetry_session = Crypto.new().generate_random_bytes(16).hex_encode()
	var started_unix := int(Time.get_unix_time_from_system())
	var path := "user://logs/m3-%d-%s.jsonl" % [started_unix, _m3_telemetry_session.left(12)]
	_m3_telemetry_file = FileAccess.open(path, FileAccess.WRITE)
	if _m3_telemetry_file == null:
		push_warning("M3 telemetry log unavailable: %s" % FileAccess.get_open_error())
		return
	_store_m3_telemetry({
		"event_type": "session_started", "timestamp_ms": started_unix * 1000,
		"simulation_hz": 60,
	})

func _m3_metric_for(player_id: String, tick: int) -> Dictionary:
	if not _m3_metrics.has(player_id):
		_m3_metrics[player_id] = {
			"interval_start_tick": tick, "mounted_ticks": 0, "on_foot_ticks": 0,
			"roll_ticks": 0, "spook_stun_ticks": 0, "horse_losses": 0,
			"remounts": 0, "running_mount_attempts": 0, "running_remounts": 0,
			"duel_wins": 0, "on_foot_vs_mounted_duels": 0,
			"on_foot_vs_mounted_wins": 0, "post_spook_deaths": 0,
			"charge_ticks": 0, "full_spur_ticks": 0, "charge_starts": 0,
			"instant_returns": 0, "charged_duels": 0, "charged_duel_wins": 0,
			"uncharged_duels": 0, "uncharged_duel_wins": 0,
			"spur_points_jump": 0, "spur_points_clean_landing": 0,
			"spur_points_near_miss": 0, "spur_points_mounted_hit": 0,
			"spur_points_mounted_elimination": 0,
			"spur_points_saddle_dive_elimination": 0,
			"last_horse_state": -1, "horse_loss_tick": -1,
			"interact_was_down": false,
		}
	return _m3_metrics[player_id] as Dictionary

func _record_m3_actor_tick(
	player_id: String, actor_tick: Dictionary, state: Dictionary, input: Dictionary, tick: int
) -> void:
	var metric := _m3_metric_for(player_id, tick)
	if bool(actor_tick.get("on_foot_active", false)):
		metric.on_foot_ticks = int(metric.on_foot_ticks) + 1
		match int(actor_tick.get("on_foot_state_id", -1)):
			0: metric.spook_stun_ticks = int(metric.spook_stun_ticks) + 1
			4: metric.roll_ticks = int(metric.roll_ticks) + 1
	else:
		metric.mounted_ticks = int(metric.mounted_ticks) + 1
	if bool(actor_tick.get("charge_active", false)):
		metric.charge_ticks = int(metric.charge_ticks) + 1
	if int(actor_tick.get("spur_meter", 0)) >= 100:
		metric.full_spur_ticks = int(metric.full_spur_ticks) + 1
	var spend_id := int(actor_tick.get("spur_spend_id", 0))
	if spend_id == 1:
		metric.charge_starts = int(metric.charge_starts) + 1
		_store_m4_spend(player_id, tick, "majestic_charge")
	elif spend_id == 2:
		metric.instant_returns = int(metric.instant_returns) + 1
		_store_m4_spend(player_id, tick, "instant_majestic_return")
	var horse_state := int(actor_tick.get("horse_state_id", 0))
	if int(metric.last_horse_state) == 0 and horse_state == 1:
		metric.horse_losses = int(metric.horse_losses) + 1
		metric.horse_loss_tick = tick
		_store_m3_telemetry({
			"event_type": "m3_horse_lost", "schema_version": 1,
			"session_id": _m3_telemetry_session,
			"authority_epoch": int(peer_session.get("authority_epoch")), "tick": tick,
			"actor_slot": int(_m3_actor_slots.get(player_id, -1)),
			"notification_points": 15,
		})
		print("SPURFIRE_M3_EVENT kind=horse_lost points=15 tick=%d" % tick)
	metric.last_horse_state = horse_state
	var buttons := int(input.get("buttons", 0))
	var interact_down := buttons & M3_INPUT_INTERACT != 0
	if interact_down and not bool(metric.interact_was_down) and (
		int(actor_tick.get("recall_state_id", 0)) == 6
		or bool(actor_tick.get("running_mount", false))
	):
		metric.running_mount_attempts = int(metric.running_mount_attempts) + 1
	metric.interact_was_down = interact_down
	if bool(actor_tick.get("remounted", false)):
		metric.remounts = int(metric.remounts) + 1
		var running := bool(actor_tick.get("running_mount", false))
		if running:
			metric.running_remounts = int(metric.running_remounts) + 1
		var lost_tick := int(metric.horse_loss_tick)
		_store_m3_telemetry({
			"event_type": "m3_remount", "schema_version": 1,
			"session_id": _m3_telemetry_session,
			"authority_epoch": int(peer_session.get("authority_epoch")), "tick": tick,
			"actor_slot": int(_m3_actor_slots.get(player_id, -1)),
			"running_mount": running,
			"lose_horse_to_remount_ticks": -1 if lost_tick < 0 else tick - lost_tick,
		})
		metric.horse_loss_tick = -1

func _record_m4_movement_style(player_id: String, state: Dictionary, tick: int) -> void:
	var previous_stance := int(state.get("previous_stance_id", state.stance_id))
	var stance := int(state.stance_id)
	if previous_stance == 1 and stance == 2:
		var awarded := int(peer_session.issue_m4_spur_credit(player_id, tick, 0, 0, 0))
		_record_m4_award(player_id, tick, "jump", awarded)
	elif previous_stance == 2 and stance == 1:
		var collision_free := true
		if player_id == local_player_id:
			for index in range(local_horse.get_slide_collision_count()):
				var collision := local_horse.get_slide_collision(index)
				if collision and collision.get_normal().y < 0.7:
					collision_free = false
					break
		var awarded := int(peer_session.issue_m4_spur_credit(
			player_id, tick, 1, 1 if collision_free else 0, 0 if collision_free else 1000
		))
		_record_m4_award(player_id, tick, "clean_landing", awarded)

func _record_m4_award(player_id: String, tick: int, source: String, points: int) -> void:
	if points <= 0:
		return
	var metric := _m3_metric_for(player_id, tick)
	var field := "spur_points_%s" % source
	if metric.has(field):
		metric[field] = int(metric[field]) + points
	_store_m3_telemetry({
		"event_type": "m4_spur_award", "tick": tick,
		"authority_epoch": int(peer_session.get("authority_epoch")),
		"actor_slot": int(_m3_actor_slots.get(player_id, -1)),
		"source": source, "points": points,
	})

func _store_m4_spend(player_id: String, tick: int, kind: String) -> void:
	_store_m3_telemetry({
		"event_type": "m4_spend", "tick": tick,
		"authority_epoch": int(peer_session.get("authority_epoch")),
		"actor_slot": int(_m3_actor_slots.get(player_id, -1)), "kind": kind,
	})

func _apply_local_m4_presentation(state: Dictionary, tick: int) -> void:
	if combat_router:
		var hud = combat_router.get("combat_hud")
		if hud and hud.has_method("set_spur_state"):
			hud.call(
				"set_spur_state", int(state.spur_meter), bool(state.charge_active),
				int(state.charge_end_tick), tick
			)
	var course := local_horse.get_parent()
	var feedback := course.get_node_or_null("FeedbackLayer/StylizedFeedback") if course else null
	if feedback and feedback.has_method("set_majestic_charge_active"):
		feedback.call("set_majestic_charge_active", bool(state.charge_active))

func _record_m3_duel_outcome(shooter: String, resolved: Dictionary, tick: int) -> void:
	var target := str(resolved.get("eliminated_rider_player_id", ""))
	if shooter.is_empty() or target.is_empty():
		return
	var shooter_metric := _m3_metric_for(shooter, tick)
	shooter_metric.duel_wins = int(shooter_metric.duel_wins) + 1
	var shooter_on_foot := bool(resolved.get("shooter_on_foot", false))
	var target_on_foot := bool(resolved.get("eliminated_rider_on_foot", false))
	if shooter_on_foot != target_on_foot:
		shooter_metric.on_foot_vs_mounted_duels = int(
			shooter_metric.on_foot_vs_mounted_duels
		) + 1
	if shooter_on_foot and not target_on_foot:
		shooter_metric.on_foot_vs_mounted_wins = int(shooter_metric.on_foot_vs_mounted_wins) + 1
	var target_metric := _m3_metric_for(target, tick)
	var shooter_charged := bool(resolved.get("shooter_charge_active", false))
	var target_state := _actor_states.get(target, {}) as Dictionary
	var target_charged := bool(target_state.get("charge_active", false))
	if shooter_charged:
		shooter_metric.charged_duels = int(shooter_metric.charged_duels) + 1
		shooter_metric.charged_duel_wins = int(shooter_metric.charged_duel_wins) + 1
	else:
		shooter_metric.uncharged_duels = int(shooter_metric.uncharged_duels) + 1
		shooter_metric.uncharged_duel_wins = int(shooter_metric.uncharged_duel_wins) + 1
	if target_charged:
		target_metric.charged_duels = int(target_metric.charged_duels) + 1
	else:
		target_metric.uncharged_duels = int(target_metric.uncharged_duels) + 1
	if int(target_metric.horse_loss_tick) >= 0:
		target_metric.post_spook_deaths = int(target_metric.post_spook_deaths) + 1
	_store_m3_telemetry({
		"event_type": "m3_duel_elimination", "schema_version": 1,
		"session_id": _m3_telemetry_session,
		"authority_epoch": int(peer_session.get("authority_epoch")), "tick": tick,
		"shooter_slot": int(_m3_actor_slots.get(shooter, -1)),
		"target_slot": int(_m3_actor_slots.get(target, -1)),
		"shooter_on_foot": shooter_on_foot, "target_on_foot": target_on_foot,
		"target_post_spook": int(target_metric.horse_loss_tick) >= 0,
	})

func _flush_m3_metrics(tick: int, terminal: bool) -> void:
	if not terminal and tick % 60 != 0:
		return
	var ids := _m3_metrics.keys()
	ids.sort()
	for player_id: String in ids:
		var metric := _m3_metrics[player_id] as Dictionary
		var observed := int(metric.mounted_ticks) + int(metric.on_foot_ticks)
		if observed <= 0 and not terminal:
			continue
		_store_m3_telemetry({
			"event_type": "m3_interval", "schema_version": 1,
			"session_id": _m3_telemetry_session,
			"authority_epoch": int(peer_session.get("authority_epoch")),
			"tick_start": int(metric.interval_start_tick), "tick_end": tick,
			"actor_slot": int(_m3_actor_slots.get(player_id, -1)),
			"mounted_ticks": int(metric.mounted_ticks),
			"on_foot_ticks": int(metric.on_foot_ticks),
			"roll_ticks": int(metric.roll_ticks),
			"spook_stun_ticks": int(metric.spook_stun_ticks),
			"horse_losses": int(metric.horse_losses), "remounts": int(metric.remounts),
			"running_mount_attempts": int(metric.running_mount_attempts),
			"running_remounts": int(metric.running_remounts),
			"duel_wins": int(metric.duel_wins),
			"on_foot_vs_mounted_duels": int(metric.on_foot_vs_mounted_duels),
			"on_foot_vs_mounted_wins": int(metric.on_foot_vs_mounted_wins),
			"post_spook_deaths": int(metric.post_spook_deaths), "terminal": terminal,
			"charge_ticks": int(metric.charge_ticks),
			"full_spur_ticks": int(metric.full_spur_ticks),
			"charge_starts": int(metric.charge_starts),
			"instant_returns": int(metric.instant_returns),
			"charged_duels": int(metric.charged_duels),
			"charged_duel_wins": int(metric.charged_duel_wins),
			"uncharged_duels": int(metric.uncharged_duels),
			"uncharged_duel_wins": int(metric.uncharged_duel_wins),
			"spur_points_jump": int(metric.spur_points_jump),
			"spur_points_clean_landing": int(metric.spur_points_clean_landing),
			"spur_points_near_miss": int(metric.spur_points_near_miss),
			"spur_points_mounted_hit": int(metric.spur_points_mounted_hit),
			"spur_points_mounted_elimination": int(metric.spur_points_mounted_elimination),
			"spur_points_saddle_dive_elimination": int(metric.spur_points_saddle_dive_elimination),
		})
		for field in [
			"mounted_ticks", "on_foot_ticks", "roll_ticks", "spook_stun_ticks",
			"horse_losses", "remounts", "running_mount_attempts", "running_remounts",
			"duel_wins", "on_foot_vs_mounted_duels",
			"on_foot_vs_mounted_wins", "post_spook_deaths",
			"charge_ticks", "full_spur_ticks", "charge_starts", "instant_returns",
			"charged_duels", "charged_duel_wins", "uncharged_duels",
			"uncharged_duel_wins", "spur_points_jump", "spur_points_clean_landing",
			"spur_points_near_miss", "spur_points_mounted_hit",
			"spur_points_mounted_elimination", "spur_points_saddle_dive_elimination",
		]:
			metric[field] = 0
		metric.interval_start_tick = tick + 1

func _store_m3_telemetry(record: Dictionary) -> void:
	if _m3_telemetry_file == null:
		return
	record["schema_version"] = 1
	record["session_id"] = _m3_telemetry_session
	record["build_commit"] = str(ProjectSettings.get_setting(
		"application/config/build_commit", "development"
	))
	_m3_telemetry_file.store_line(JSON.stringify(record))
	_m3_telemetry_file.flush()

func _close_m3_telemetry(reason: String) -> void:
	if _m3_telemetry_closed:
		return
	_m3_telemetry_closed = true
	_store_m3_telemetry({
		"event_type": "session_ended",
		"timestamp_ms": int(Time.get_unix_time_from_system()) * 1000,
		"reason": reason,
	})
	if _m3_telemetry_file:
		_m3_telemetry_file.close()
		_m3_telemetry_file = null

func _notification(what: int) -> void:
	if what == NOTIFICATION_PREDELETE:
		_flush_m3_metrics(simulation_tick, true)
		_close_m3_telemetry("scene_close")

func _sample_m3_input(tick: int) -> Dictionary:
	var throttle := roundi(Input.get_axis(&"move_back", &"move_forward") * 1000.0)
	var steer := roundi(Input.get_axis(&"steer_left", &"steer_right") * 1000.0)
	var buttons := _buffered_m3_buttons(
		tick, Input.is_action_just_pressed(&"jump"),
		Input.is_action_just_pressed(&"on_foot_crouch"),
		Input.is_action_pressed(&"on_foot_crouch")
	)
	if Input.is_action_just_pressed(&"combat_interact"):
		buttons |= M3_INPUT_INTERACT
	if Input.is_action_pressed(&"on_foot_sprint"):
		buttons |= M3_INPUT_SPRINT
	if Input.is_action_pressed(&"combat_reload"):
		buttons |= M3_INPUT_RELOAD
	if Input.is_action_pressed(&"combat_aim"):
		buttons |= M3_INPUT_ADS
	if Input.is_action_just_pressed(&"spur_spend"):
		buttons |= M3_INPUT_SPUR
	var on_foot_move := Vector2(float(steer), float(-throttle))
	if on_foot_move.length() > 1000.0:
		on_foot_move = on_foot_move.normalized() * 1000.0
	return {
		"throttle_milli": throttle, "steer_milli": steer,
		"move_x_milli": roundi(on_foot_move.x), "move_z_milli": roundi(on_foot_move.y),
		"buttons": buttons,
	}

func _buffered_m3_buttons(
	tick: int, jump_just_pressed: bool, crouch_just_pressed: bool, crouch_held: bool
) -> int:
	var buttons := 0
	if jump_just_pressed:
		_jump_buffer_until_tick = tick + M3_INPUT_BUFFER_TICKS - 1
	if crouch_just_pressed:
		_crouch_buffer_until_tick = tick + M3_INPUT_BUFFER_TICKS - 1
	if tick <= _jump_buffer_until_tick:
		buttons |= M3_INPUT_JUMP
	if crouch_held or tick <= _crouch_buffer_until_tick:
		buttons |= M3_INPUT_CROUCH
	return buttons

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
	_flush_m3_metrics(simulation_tick, true)
	_quiesced = true
	var packet: PackedByteArray = peer_session.make_leave(simulation_tick)
	if not packet.is_empty():
		_send_to_all(packet)
	_close_m3_telemetry("leave")

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
			_apply_m3_snapshot(
				str(payload.get("snapshot_json", "")), int(payload.get("tick", simulation_tick))
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
	var m3_resolution := {}
	if peer_session.is_m3_wire_active():
		m3_resolution = peer_session.resolve_m3_shot_command(
			str(payload.get("command_json", "")), simulation_tick
		) as Dictionary
		result_json = str(m3_resolution.get("result_json", ""))
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
	if not m3_resolution.is_empty():
		_record_m3_duel_outcome(shooter, m3_resolution, tick)
		_record_m4_award(
			shooter, tick, str(m3_resolution.get("spur_award_source", "")),
			int(m3_resolution.get("spur_awarded_points", 0))
		)
		if decoded is Dictionary:
			_record_m4_near_misses(shooter, decoded as Dictionary, m3_resolution, tick)
	_apply_shot_result_once({
		"authority_epoch": int(peer_session.get("authority_epoch")),
		"tick": tick, "result_json": result_json,
		"shooter_player_id": shooter,
	})

func _record_m4_near_misses(
	shooter: String, resolved: Dictionary, authority: Dictionary, tick: int
) -> void:
	if str(resolved.get("outcome", "")) != "miss":
		return
	var origin_value = authority.get("authority_origin")
	if not origin_value is Vector3:
		return
	var direction_row := resolved.get("resolved_direction", {}) as Dictionary
	var origin := origin_value as Vector3
	var direction := Vector3(
		float(direction_row.get("x", 0)), float(direction_row.get("y", 0)),
		float(direction_row.get("z", 0))
	) / 1000000.0
	if direction.length_squared() < 0.99:
		return
	direction = direction.normalized()
	for player_id: String in _actor_states:
		if player_id == shooter:
			continue
		var state := _actor_states[player_id] as Dictionary
		if int(state.get("stance_id", 0)) not in [1, 2]:
			continue
		var target := (state.position as Vector3) + Vector3.UP
		var along := clampf((target - origin).dot(direction), 0.0, 250.0)
		var distance_mm := roundi(target.distance_to(origin + direction * along) * 1000.0)
		if distance_mm <= 1500:
			var awarded := int(peer_session.issue_m4_spur_credit(player_id, tick, 2, 1, distance_mm))
			_record_m4_award(player_id, tick, "near_miss", awarded)

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

func _apply_m3_snapshot(snapshot_json: String, snapshot_tick: int) -> void:
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
	var charge_started_tick := int(row.get("b", row.get("charge_started_tick", -1)))
	var charge_end_tick := int(row.get("e", row.get("charge_end_tick", -1)))
	var charge_active := (
		charge_started_tick >= 0 and snapshot_tick >= charge_started_tick
		and snapshot_tick < charge_end_tick
	)
	_actor_states[player_id] = {
		"tick": snapshot_tick,
		"position": rider_position, "velocity": rider_velocity,
		"yaw_degrees": float(row.get("y", row.get("rider_yaw_millidegrees", 0))) / 1000.0,
		"stance_id": stance_id, "dive_id": -1,
		"horse_position": _mm_vector(horse_position as Array),
		"horse_velocity": _mm_vector(horse_velocity as Array),
		"horse_yaw_degrees": float(horse.get("y", horse.get("yaw_millidegrees", 0))) / 1000.0,
		"m3_stance": stance_name,
		"horse_health": int(horse.get("h", horse.get("health", 0))),
		"horse_state": str(horse.get("s", horse.get("state", "despawned"))),
		"horse_class": str(horse.get("c", horse.get("class", "courser"))),
		"recall_state": str(row.get("r", row.get("recall_state", "horse_present"))),
		"spur_meter": int(row.get("u", row.get("spur_meter", 0))),
		"charge_active": charge_active,
		"charge_started_tick": charge_started_tick,
		"charge_end_tick": charge_end_tick,
	}
	if player_id == local_player_id:
		var correction := rider_position - local_rider.global_position
		local_rider.global_position += correction if correction.length() >= 2.0 else correction * 0.35
		local_rider.velocity = rider_velocity
		if local_horse.has_method("set_majestic_charge_active"):
			local_horse.call("set_majestic_charge_active", charge_active)
		_apply_local_m4_presentation(_actor_states[player_id] as Dictionary, snapshot_tick)
	else:
		var rider := _remote_rider_for(player_id)
		if rider and rider.has_method("push_snapshot"):
			rider.push_snapshot(
				snapshot_tick, rider_position, rider_velocity,
				float(_actor_states[player_id].yaw_degrees), stance_id
			)
		_apply_remote_horse_snapshot(player_id, snapshot_tick, _actor_states[player_id])

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
	var previous_stance := int((_actor_states.get(player_id, {}) as Dictionary).get(
		"stance_id", int(rider.get("stance_id"))
	))
	_actor_states[player_id] = {
		"tick": tick, "position": rider.global_position, "velocity": rider.velocity,
		"yaw_degrees": rad_to_deg(rider.rotation.y), "stance_id": int(rider.get("stance_id")),
		"previous_stance_id": previous_stance, "dive_id": int(rider.get("dive_id")),
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
	var charge_active := bool(state.get("charge_active", false))
	var horse_class := str(_m3_horse_classes.get(player_id, state.get("horse_class", "courser")))
	var sprint_speed := float(
		{"courser": 16.5, "warhorse": 13.5, "mustang": 14.5}.get(horse_class, 16.5)
	)
	var turn_rate := 117.0 if charge_active else 90.0
	var yaw := float(state.yaw_degrees) + steer * turn_rate / 60.0
	var forward := Vector3.FORWARD.rotated(Vector3.UP, deg_to_rad(yaw))
	var velocity := forward * throttle * (sprint_speed if charge_active else 13.0)
	var position := (state.position as Vector3) + velocity / 60.0
	var stance := int(state.stance_id)
	var airborne_until_tick := int(state.get("mounted_airborne_until_tick", -1))
	if stance == 2 and airborne_until_tick >= 0 and tick >= airborne_until_tick:
		stance = 1
		airborne_until_tick = -1
	var buttons := 0 if stale else int(input.get("buttons", 0))
	if buttons & 1 and stance == 1:
		stance = 2
		var airtime_ticks := int(
			{"courser": 42, "warhorse": 30, "mustang": 48}.get(horse_class, 42)
		)
		airborne_until_tick = tick + airtime_ticks
	if buttons & 2:
		stance = 6 if stance == 1 else (1 if stance == 6 else stance)
	_actor_states[player_id] = {
		"tick": tick, "position": position, "velocity": velocity,
		"yaw_degrees": yaw, "stance_id": stance, "previous_stance_id": int(state.stance_id),
		"dive_id": int(state.get("dive_id", -1)), "horse_class": horse_class,
		"spur_meter": int(state.get("spur_meter", 0)),
		"charge_active": charge_active,
		"charge_started_tick": int(state.get("charge_started_tick", -1)),
		"charge_end_tick": int(state.get("charge_end_tick", -1)),
		"mounted_airborne_until_tick": airborne_until_tick,
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
	if _remote_horses.has(player_id):
		var horse := _remote_horses[player_id] as Node3D
		_remote_horses.erase(player_id)
		horse.visible = false
		horse.queue_free()

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

func _remote_horse_for(player_id: String) -> Node3D:
	if _remote_horses.has(player_id):
		return _remote_horses[player_id] as Node3D
	var horse := ClassDB.instantiate(&"NetworkRider") as Node3D
	if horse == null:
		return null
	horse.name = "RemoteHorse_%s" % player_id.left(8)
	remote_rider_template.get_parent().add_child(horse)
	for child in local_horse.get_children():
		if child is MeshInstance3D:
			var visual := (child as MeshInstance3D).duplicate() as MeshInstance3D
			horse.add_child(visual)
	var cue := Label3D.new()
	cue.name = "M3PhaseCue"
	cue.position = Vector3(0.0, 3.35, 0.0)
	cue.billboard = BaseMaterial3D.BILLBOARD_ENABLED
	cue.font_size = 28
	cue.outline_size = 8
	cue.no_depth_test = true
	horse.add_child(cue)
	_remote_horses[player_id] = horse
	return horse

func _apply_remote_horse_snapshot(player_id: String, tick: int, state: Dictionary) -> void:
	var horse := _remote_horse_for(player_id)
	if horse == null:
		return
	var horse_state := str(state.get("horse_state", "despawned"))
	var recall_state := str(state.get("recall_state", "horse_present"))
	var returning := recall_state in ["dust_reveal", "gallop_in", "mount_window", "waiting_mount"]
	horse.visible = horse_state in ["available", "bolting"] or returning
	var cue := horse.get_node_or_null("M3PhaseCue") as Label3D
	if cue:
		var charging := tick < int(state.get("charge_end_tick", -1))
		match horse_state:
			"bolting": cue.text = "SPOOK!"
			_:
				match recall_state:
					"dust_reveal": cue.text = "DUST ON THE HORIZON"
					"gallop_in": cue.text = "MAJESTIC RETURN"
					"mount_window": cue.text = "RUNNING MOUNT"
					_: cue.text = "MAJESTIC CHARGE" if charging else ""
	if horse.has_method("push_snapshot"):
		horse.call(
			"push_snapshot", tick, state.horse_position, state.horse_velocity,
			float(state.horse_yaw_degrees), 1
		)

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
