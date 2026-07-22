extends Node

const LOBBY_ID := "00000000-0000-4000-8000-000000000099"
const PLAYER_A := "00000000-0000-4000-8000-000000000001"
const PLAYER_B := "00000000-0000-4000-8000-000000000002"
const PLAYER_C := "00000000-0000-4000-8000-000000000003"
const LOBBY_SHELL_SCENE := preload("res://lobby/lobby_shell.tscn")

func _ready() -> void:
	var failures: Array[String] = []
	_check_native_boundary(failures)
	_check_roster_projection(failures)
	_check_peer_roster_binding(failures)
	_check_m3_loadout_projection(failures)
	_check_m3_input_buffer(failures)
	_check_m3_horse_presentation_path(failures)
	_check_m4_remote_charge_proxy(failures)
	_check_m4_follower_charge_snapshot(failures)
	_check_m5_match_state_normalization(failures)
	_check_secret_storage_contract(failures)
	_check_control_glue(failures)
	_check_cleanup_truth(failures)
	await _check_offline_alpha_loop(failures)
	if failures.is_empty():
		print("SPURFIRE_LOBBY_CLIENT_CONTRACT_OK")
		print("SPURFIRE_ALPHA_LOBBY_SMOKE_OK")
		print("SPURFIRE_OFFLINE_ALPHA_SMOKE_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error(failure)
		get_tree().quit(1)

func _check_offline_alpha_loop(failures: Array[String]) -> void:
	var shell := LOBBY_SHELL_SCENE.instantiate()
	add_child(shell)
	await get_tree().process_frame
	shell.call("_start_practice")
	for _index in range(12):
		await get_tree().physics_frame
	var course := shell.get("_course") as Node
	if course == null:
		failures.append("offline Alpha launcher did not instantiate the playable course")
		shell.queue_free()
		await get_tree().process_frame
		return
	var bridge := course.get_node_or_null("LobbyPeerBridge") as SpurfireLobbyPeerBridge
	if bridge == null or not bridge.is_offline_practice() or bridge.practice_bot_count() != 3:
		failures.append("offline Alpha launcher did not bind its three-bot practice authority")
	else:
		var match_state := bridge.get_m5_state()
		if (
			int(match_state.get("current_tick", 0)) <= 0
			or (match_state.get("players", []) as Array).size() != 4
		):
			failures.append("offline Alpha Bounty Run did not advance all four M5 actors")
		var peer_rows := bridge.get_peer_status()
		if peer_rows.size() != 4 or str((peer_rows[1] as Dictionary).route) != "LOCAL BOT":
			failures.append("offline Alpha roster did not expose local practice opponents")
		var actor_states := bridge.actor_states()
		if actor_states.size() != 4:
			failures.append("offline Alpha authority did not retain four M3 actor states")
		var remote_riders := bridge.get("_remote_riders") as Dictionary
		var remote_horses := bridge.get("_remote_horses") as Dictionary
		if remote_riders.size() != 3 or remote_horses.size() != 3:
			failures.append("offline Alpha opponents were simulated but not presented")
		var first_tick := int(bridge.get("simulation_tick")) + 1
		for tick in range(first_tick, 331):
			bridge.advance_shared_tick(tick)
		for bot_id in [
			"00000000-0000-4000-8000-0000000000b1",
			"00000000-0000-4000-8000-0000000000b2",
			"00000000-0000-4000-8000-0000000000b3",
		]:
			var combat := bridge.peer_session.combat_checkpoint_state(bot_id) as Dictionary
			if int(combat.get("last_shot_tick", -1)) < 300:
				var shot_status := bridge.practice_shot_status().get(bot_id, {}) as Dictionary
				failures.append(
					"offline Alpha practice opponent did not fire through authority: %s"
					% JSON.stringify(shot_status)
				)
	shell.queue_free()
	await get_tree().process_frame

func _check_native_boundary(failures: Array[String]) -> void:
	if not ClassDB.class_exists(&"PeerSession") or not ClassDB.class_exists(&"NativeSecretInput"):
		failures.append("native lobby/input classes unavailable")
		return
	var session := ClassDB.instantiate(&"PeerSession") as Node
	if session == null:
		failures.append("native lobby owner could not be instantiated")
		return
	add_child(session)
	if session.has_method(StringName("connect_" + "rustscale")):
		failures.append("legacy Godot secret-taking transport method remains exported")
	for method in [
		"configure_lobby_player", "probe_lobby_readiness", "capture_launch_code",
		"capture_join_code", "submit_create", "submit_join", "auto_join_creator",
		"copy_invitation_to_clipboard", "cancel_lobby_operations",
	]:
		if not session.has_method(method):
			failures.append("native lobby method unavailable: %s" % method)
	var signals := ClassDB.class_get_signal_list(&"PeerSession", true)
	for descriptor in signals:
		var signal_name := str((descriptor as Dictionary).get("name", ""))
		if signal_name in ["create_completed", "join_completed", "invitation_copied"]:
			for argument in (descriptor as Dictionary).get("args", []):
				var argument_name := str((argument as Dictionary).get("name", ""))
				if argument_name.contains("secret") or argument_name.contains("token"):
					failures.append("native signal exposes secret-shaped argument")
	session.cancel_lobby_operations()
	session.queue_free()

func _check_roster_projection(failures: Array[String]) -> void:
	var lobby := {
		"authority": {"candidate_player_id": PLAYER_A},
		"roster": [
			{"player_id": PLAYER_A, "display_name": "Dusty"},
			{"player_id": PLAYER_B, "display_name": "Mesa"},
		],
	}
	var rows := SpurfireLobbyContract.safe_roster(lobby, PLAYER_B, {
		PLAYER_A: {"route": "direct", "rtt_ms": 31, "freshness": "fresh"},
	})
	if rows.size() != 2 or not bool(rows[0].authority) or not bool(rows[1].you):
		failures.append("selected-lobby roster badges are incorrect")
	if rows[1].rtt_ms != null or str(rows[1].freshness) != "unknown":
		failures.append("missing health was fabricated instead of remaining unknown")
	for row: Dictionary in rows:
		if row.has("endpoint") or row.has("tailnet_address"):
			failures.append("player-visible roster exposed a private endpoint")

func _check_peer_roster_binding(failures: Array[String]) -> void:
	if not ClassDB.class_exists(&"PeerSession"):
		failures.append("PeerSession native class unavailable")
		return
	var receiver := ClassDB.instantiate(&"PeerSession") as Node
	var outsider := ClassDB.instantiate(&"PeerSession") as Node
	if receiver == null or outsider == null:
		failures.append("PeerSession could not be instantiated")
		return
	add_child(receiver)
	add_child(outsider)
	receiver.set_insecure_demo_mode(true)
	outsider.set_insecure_demo_mode(true)
	var roster := PackedStringArray([PLAYER_A, PLAYER_B])
	if not receiver.configure_roster_session(LOBBY_ID, PLAYER_A, PLAYER_A, roster, 1):
		failures.append("exact roster session configuration failed")
	if not outsider.configure_roster_session(
		LOBBY_ID, PLAYER_C, PLAYER_A, PackedStringArray([PLAYER_A, PLAYER_C]), 1
	):
		failures.append("outsider fixture session configuration failed")
	var outsider_packet: PackedByteArray = outsider.make_heartbeat(1)
	if receiver.accept_packet(outsider_packet, 2) != 4:
		failures.append("packet sender outside selected roster was not rejected")
	var leave_packet: PackedByteArray = receiver.make_leave(2)
	if str((receiver.decode_packet(leave_packet) as Dictionary).get("type", "")) != "leave":
		failures.append("orderly leave packet was not encoded")
	receiver.clear_lobby_session()
	outsider.clear_lobby_session()
	receiver.queue_free()
	outsider.queue_free()

func _check_m3_loadout_projection(failures: Array[String]) -> void:
	var bridge := SpurfireLobbyPeerBridge.new()
	var loadouts := bridge.call("_m3_loadouts", [
		{"player_id": PLAYER_B, "horse_selection": "warhorse"},
		{"player_id": PLAYER_A, "horse_selection": "courser"},
	]) as Array
	if (
		loadouts.size() != 2 or str((loadouts[0] as Dictionary).player_id) != PLAYER_A
		or str((loadouts[0] as Dictionary).horse_class) != "courser"
		or str((loadouts[1] as Dictionary).weapon_id) != "dustwalker"
	):
		failures.append("M3 lobby loadout graph was not exact, sorted, and Alpha-locked")
	var invalid := bridge.call("_m3_loadouts", [
		{"player_id": PLAYER_A, "horse_selection": "unknown"},
	]) as Array
	if not invalid.is_empty():
		failures.append("M3 lobby loadout graph accepted an unknown horse selection")
	bridge.free()

func _check_m3_input_buffer(failures: Array[String]) -> void:
	var bridge := SpurfireLobbyPeerBridge.new()
	var pressed := int(bridge.call("_buffered_m3_buttons", 100, true, true, false))
	var final_buffered := int(bridge.call("_buffered_m3_buttons", 108, false, false, false))
	var expired := int(bridge.call("_buffered_m3_buttons", 109, false, false, false))
	var held := int(bridge.call("_buffered_m3_buttons", 200, false, false, true))
	if pressed != 9 or final_buffered != 9 or expired != 0 or held != 8:
		failures.append("M3 jump/crouch input latch was not exactly nine 60 Hz ticks")
	bridge.free()

func _check_m3_horse_presentation_path(failures: Array[String]) -> void:
	var bridge := SpurfireLobbyPeerBridge.new()
	var state := {
		"position": Vector3.ZERO, "velocity": Vector3.ZERO, "yaw_degrees": 0.0,
		"horse_position": Vector3.ZERO, "horse_velocity": Vector3.ZERO,
		"horse_yaw_degrees": 0.0,
	}
	bridge.call("_update_authority_horse_presentation", PLAYER_B, state, {
		"horse_state_id": 1, "recall_state_id": 0,
		"horse_bolt_started_tick": 100, "horse_bolt_direction": Vector2.RIGHT,
	}, 160)
	if not (state.horse_position as Vector3).is_equal_approx(Vector3(12.0, 0.0, 0.0)):
		failures.append("M3 remote horse bolt did not use its authority tick/direction")
	state["horse_position"] = Vector3.ZERO
	bridge.call("_update_authority_horse_presentation", PLAYER_B, state, {
		"horse_state_id": 2, "recall_state_id": 5, "recall_phase_enter_tick": 200,
	}, 290)
	if not (state.horse_position as Vector3).is_equal_approx(Vector3(0.0, 0.0, 31.5)):
		failures.append("M3 Majestic Return did not traverse 60m to the rider deterministically")
	var presentation_root := Node3D.new()
	add_child(presentation_root)
	presentation_root.add_child(bridge)
	var local_horse := CharacterBody3D.new()
	presentation_root.add_child(local_horse)
	var visual := MeshInstance3D.new()
	visual.mesh = BoxMesh.new()
	local_horse.add_child(visual)
	var remote_template := Node3D.new()
	presentation_root.add_child(remote_template)
	bridge.local_horse = local_horse
	bridge.remote_rider_template = remote_template
	state["horse_state"] = "despawned"
	state["recall_state"] = "gallop_in"
	bridge.call("_apply_remote_horse_snapshot", PLAYER_B, 290, state)
	var remote_horses := bridge.get("_remote_horses") as Dictionary
	var presented := remote_horses.get(PLAYER_B) as Node3D
	if (
		presented == null or not presented.visible or not presented.has_method("push_snapshot")
		or presented.get_node_or_null("M3PhaseCue") == null
	):
		failures.append("M3 remote horse proxy did not instantiate its interpolated return visual")
	presentation_root.free()

func _check_m4_remote_charge_proxy(failures: Array[String]) -> void:
	var bridge := SpurfireLobbyPeerBridge.new()
	bridge.set("_m3_horse_classes", {PLAYER_B: "mustang"})
	bridge.set("_actor_states", {PLAYER_B: {
		"tick": 9, "position": Vector3.ZERO, "velocity": Vector3.ZERO,
		"yaw_degrees": 0.0, "stance_id": 1, "dive_id": -1,
		"charge_active": true, "charge_started_tick": 9, "charge_end_tick": 369,
	}})
	bridge.call("_simulate_remote_actor", PLAYER_B, {
		"tick": 10, "throttle_milli": 1000, "steer_milli": 1000, "buttons": 0,
	}, 10)
	var state := (bridge.get("_actor_states") as Dictionary).get(PLAYER_B, {}) as Dictionary
	if not is_equal_approx((state.velocity as Vector3).length(), 14.5):
		failures.append("M4 remote Mustang Charge did not use archetype sprint speed")
	if not is_equal_approx(float(state.yaw_degrees), 117.0 / 60.0):
		failures.append("M4 remote Charge did not apply the +30% proxy turn row")
	bridge.call("_simulate_remote_actor", PLAYER_B, {
		"tick": 11, "throttle_milli": 0, "steer_milli": 0, "buttons": 1,
	}, 11)
	bridge.call("_simulate_remote_actor", PLAYER_B, {
		"tick": 59, "throttle_milli": 0, "steer_milli": 0, "buttons": 0,
	}, 59)
	state = (bridge.get("_actor_states") as Dictionary).get(PLAYER_B, {}) as Dictionary
	if int(state.previous_stance_id) != 2 or int(state.stance_id) != 1:
		failures.append("M4 remote Mustang jump did not produce its 48-tick clean landing edge")
	bridge.free()

func _check_m4_follower_charge_snapshot(failures: Array[String]) -> void:
	var bridge := SpurfireLobbyPeerBridge.new()
	add_child(bridge)
	bridge.local_player_id = PLAYER_A
	bridge.local_rider = CharacterBody3D.new()
	bridge.local_horse = ClassDB.instantiate(&"HorseController") as CharacterBody3D
	add_child(bridge.local_rider)
	add_child(bridge.local_horse)
	bridge.call("_apply_m3_snapshot", JSON.stringify({
		"i": PLAYER_A, "p": [0, 0, 0], "v": [0, 0, 0], "y": 0, "s": "mounted",
		"o": {"p": [0, 0, 0], "v": [0, 0, 0], "y": 0, "h": 200,
			"s": "available", "c": "courser"},
		"r": "horse_present", "u": 0, "b": 10, "e": 370,
	}), 20)
	var state := (bridge.get("_actor_states") as Dictionary).get(PLAYER_A, {}) as Dictionary
	if not bool(state.get("charge_active", false)):
		failures.append("M4 follower snapshot did not reconstruct the authority Charge window")
	if bridge.local_horse == null or not bool(bridge.local_horse.get("majestic_charge_active")):
		failures.append("M4 follower snapshot did not apply Charge to local locomotion")
	bridge.local_rider.free()
	bridge.local_horse.free()
	bridge.free()

func _check_m5_match_state_normalization(failures: Array[String]) -> void:
	var bridge := SpurfireLobbyPeerBridge.new()
	bridge.call("_install_m5_state_json", JSON.stringify({
		"e": 2, "g": 91, "t": 7200, "n": 54000, "f": false,
		"p": [[PLAYER_A, 275, 2, 1, 1, false, 7500, null, null]],
		"w": {"i": PLAYER_A, "s": 7200, "e": 7800},
		"o": {"i": 1, "k": "moving_bounty", "s": 5400, "e": 9000, "c": false, "x": 120000, "z": -80000},
	}))
	var state := bridge.call("get_m5_state") as Dictionary
	var players := state.get("players", []) as Array
	if (
		int(state.get("current_tick", -1)) != 7200 or players.size() != 1
		or int((players[0] as Dictionary).get("score", 0)) != 275
		or int((players[0] as Dictionary).get("respawn_at_tick", 0)) != 7500
		or str((state.get("active_reveal", {}) as Dictionary).get("player_id", "")) != PLAYER_A
		or str((state.get("active_objective", {}) as Dictionary).get("kind", "")) != "moving_bounty"
		or (state.get("active_objective", {}) as Dictionary).get("position", Vector3.ZERO) != Vector3(120, 1, -80)
	):
		failures.append("M5 compact MatchState did not normalize into follower HUD state")
	bridge.call("_install_m5_state_json", JSON.stringify({
		"e": 2, "g": 91, "t": 54000, "n": 54000, "f": true, "x": PLAYER_A,
		"p": [[PLAYER_A, 275, 2, 1, 1, true, null, null, null, [200, 50, 0, 0, 0, 25, 0, 0]]],
	}))
	state = bridge.call("get_m5_state") as Dictionary
	players = state.get("players", []) as Array
	if (
		players.size() != 1
		or int(((players[0] as Dictionary).get("score_breakdown", {}) as Dictionary).get("objective", 0)) != 25
	):
		failures.append("M5 final MatchState omitted results score categories")
	bridge.free()

func _check_secret_storage_contract(failures: Array[String]) -> void:
	var shell_source := FileAccess.get_file_as_string("res://scripts/lobby_shell.gd")
	var scene_source := FileAccess.get_file_as_string("res://lobby/lobby_shell.tscn")
	for forbidden in [
		"connect_" + "rustscale", "make_" + "join_code", "parse_" + "join_code",
		"clipboard_" + "get", "clipboard_" + "set",
	]:
		if shell_source.contains(forbidden) or scene_source.contains(forbidden):
			failures.append("Godot lobby boundary contains forbidden secret path")
	if shell_source.contains("LineEdit = $Screens/Title/Card/Margin/VBox/LaunchCode"):
		failures.append("create grant still uses a Godot text control")
	if scene_source.contains("name=\"ShareCode\""):
		failures.append("share code is still rendered into a Godot control")
	for required in ["NativeSecretInput", "cancel_lobby_operations", "_public_response"]:
		if not (shell_source + scene_source).contains(required):
			failures.append("native lobby containment contract omitted: %s" % required)

func _check_control_glue(failures: Array[String]) -> void:
	var shell_source := FileAccess.get_file_as_string("res://scripts/lobby_shell.gd").replace("\r\n", "\n")
	for required in [
		"submit_measurements", "_stop_peer_transport",
		"api.capture_launch_code()", "api.capture_join_code()",
		"and bool(session.get(\"secure\", false))",
		"_try_register_endpoint(\"\", 0, true)",
	]:
		if not shell_source.contains(required):
			failures.append("lobby shell omitted integration behavior: %s" % required)
	var bridge_source := FileAccess.get_file_as_string("res://scripts/lobby_peer_bridge.gd")
	if not bridge_source.contains("configure_roster_session"):
		failures.append("lobby peer bridge omitted exact-roster session binding")
	for required in [
		"dispatch_packet_with_source", "rider_player_id", "_simulate_remote_actor",
		"_apply_shot_result_once", "begin_authority_migration", "poll_migration",
		"_remove_peer(previous_id)", "_latest_inputs.erase(player_id)",
		"_actor_states.erase(player_id)", "combat_checkpoint_state",
		"combat_resolved_shots_json", "record_authority_rider_snapshot",
		"_migration_pending or not local_is_authority", "dive_id",
		"activate_m3_wire", "make_m3_actor_input", "make_m3_actor_snapshot_from_pose",
		"poll_m3_migration", "configure_migration_election", "authority_election",
		"record_m3_horse_pose", "actor_snapshot",
		"advance_m5_match", "make_m5_match_state", "match_state", "get_m5_state",
		"m5_respawn_position", "complete_m5_objective", "record_m5_signal_hold",
		"_apply_m5_respawns", "_advance_m5_objective_interactions",
		"m5_interval", "m5_match_result", "m5_survey", "record_m5_play_again",
		"M3_INPUT_BUFFER_TICKS := 9", "_jump_buffer_until_tick",
		"_crouch_buffer_until_tick", "reload_active_ticks",
		"_update_authority_horse_presentation", "_apply_remote_horse_snapshot",
		"_remote_horses.erase(player_id)", "M3_RETURN_SPAWN_DISTANCE_M := 60.0",
		"m3_interval", "m3_horse_lost", "m3_duel_elimination",
		"running_mount_attempts", "post_spook_deaths", "user://logs/m3-",
	]:
		if not bridge_source.contains(required):
			failures.append("lobby peer bridge omitted M2 multiplayer behavior: %s" % required)
	var network_source := FileAccess.get_file_as_string("res://scripts/network_status.gd")
	for required in ["M5ResultsPanel", "PLAY AGAIN", "score_breakdown", "_record_result_choice"]:
		if not network_source.contains(required):
			failures.append("M5 results UI omitted behavior: %s" % required)
	for required in ["submit_results", "results_completed", "_on_m5_match_choice"]:
		if not shell_source.contains(required):
			failures.append("lobby shell omitted M5 results lifecycle: %s" % required)
	if bridge_source.contains("accept_packet_with_source(") or bridge_source.contains("decode_packet(packet"):
		failures.append("secure bridge retained split packet acceptance/decoding")
	if shell_source.contains("Host left • ending this Alpha match"):
		failures.append("authority loss still tears down implemented M2 play")
	for required in ["if _leaving:\n\t\treturn", "if not _leaving and _report_elapsed"]:
		if not shell_source.contains(required):
			failures.append("lobby shell omitted leave-race gate: %s" % required)

func _check_cleanup_truth(failures: Array[String]) -> void:
	var pending := {"backing": {"network_lifecycle": "VERIFYING_ABSENCE"}}
	var absent := {"backing": {"network_lifecycle": "DEDICATED_ABSENT"}}
	if SpurfireLobbyContract.cleanup_message(pending).begins_with("Confirmed"):
		failures.append("delete acknowledgement was represented as confirmed cleanup")
	if not SpurfireLobbyContract.cleanup_message(absent).begins_with("Confirmed"):
		failures.append("exact absence did not produce confirmed cleanup copy")
