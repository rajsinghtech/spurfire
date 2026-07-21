extends Node

const LOBBY_ID := "00000000-0000-4000-8000-000000000099"
const PLAYER_A := "00000000-0000-4000-8000-000000000001"
const PLAYER_B := "00000000-0000-4000-8000-000000000002"
const PLAYER_C := "00000000-0000-4000-8000-000000000003"

func _ready() -> void:
	var failures: Array[String] = []
	_check_native_boundary(failures)
	_check_roster_projection(failures)
	_check_peer_roster_binding(failures)
	_check_m3_loadout_projection(failures)
	_check_m3_input_buffer(failures)
	_check_secret_storage_contract(failures)
	_check_control_glue(failures)
	_check_cleanup_truth(failures)
	if failures.is_empty():
		print("SPURFIRE_LOBBY_CLIENT_CONTRACT_OK")
		print("SPURFIRE_ALPHA_LOBBY_SMOKE_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error(failure)
		get_tree().quit(1)

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
		"poll_m3_migration", "record_m3_horse_pose", "actor_snapshot",
		"M3_INPUT_BUFFER_TICKS := 9", "_jump_buffer_until_tick",
		"_crouch_buffer_until_tick", "reload_active_ticks",
	]:
		if not bridge_source.contains(required):
			failures.append("lobby peer bridge omitted M2 multiplayer behavior: %s" % required)
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
