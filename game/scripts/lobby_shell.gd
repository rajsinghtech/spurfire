extends Control

const COURSE_SCENE := preload("res://scenes/frontier_arena.tscn")
const PALETTE := preload("res://scripts/ui_palette.gd")
const RANDOM_NAMES := ["Dusty", "Sundown", "Juniper", "Longshot", "Mesa", "Coyote", "Red Rock"]
const ROSTER_ACCENTS := [Color("3fb6c9"), Color("f07a3f"), Color("ef5aaa")]

enum Screen { TITLE, WAITING, TEARDOWN, MATCH }

@onready var peer_session: Node = $PeerSession
@onready var api: Node = peer_session
@onready var background: TextureRect = $Background
@onready var screens: Control = $Screens
@onready var title_screen: Control = $Screens/Title
@onready var waiting_screen: Control = $Screens/Waiting
@onready var teardown_screen: Control = $Screens/Teardown
@onready var name_edit: LineEdit = $Screens/Title/Card/Margin/VBox/Name
@onready var launch_code_input: Control = $Screens/Title/Card/Margin/VBox/LaunchCode
@onready var join_code_input: Control = $Screens/Title/Card/Margin/VBox/JoinCode
@onready var create_button: Button = $Screens/Title/Card/Margin/VBox/Actions/Create
@onready var join_button: Button = $Screens/Title/Card/Margin/VBox/Actions/Join
@onready var title_status: Label = $Screens/Title/Card/Margin/VBox/Status
@onready var lobby_name_label: Label = $Screens/Waiting/Card/Margin/VBox/LobbyName
@onready var share_status: Label = $Screens/Waiting/Card/Margin/VBox/ShareStatus
@onready var roster_box: VBoxContainer = $Screens/Waiting/Card/Margin/VBox/Roster/Rows
@onready var network_summary: Label = $Screens/Waiting/Card/Margin/VBox/NetworkSummary
@onready var waiting_status: Label = $Screens/Waiting/Card/Margin/VBox/Status
@onready var start_button: Button = $Screens/Waiting/Card/Margin/VBox/Actions/Start
@onready var end_button: Button = $Screens/Waiting/Card/Margin/VBox/Actions/End
@onready var teardown_status: Label = $Screens/Teardown/Card/Margin/VBox/Status

var _screen := Screen.TITLE
var _player_id := ""
var _lobby_id := ""
var _lobby: Dictionary = {}
var _network_view: Dictionary = {}
var _network_generation := 0
var _roster_revision := 0
var _session_generation := 0
var _manifest_public_key := ""
var _course: Node = null
var _bridge: SpurfireLobbyPeerBridge = null
var _endpoint_registered := false
var _registered_roster_revision := 0
var _poll_elapsed := 0.0
var _report_elapsed := 0.0
var _heartbeat_elapsed := 0.0
var _endpoint_renew_elapsed := 0.0
var _authority_input_hash := ""
var _quit_after_leave := false
var _leaving := false
var _match_fade: ColorRect

func _ready() -> void:
	get_tree().auto_accept_quit = false
	name_edit.text = RANDOM_NAMES[randi() % RANDOM_NAMES.size()]
	_player_id = SpurfireLobbyContract.new_uuid_v4()
	_connect_signals()
	_match_fade = ColorRect.new()
	_match_fade.name = "MatchFade"
	_match_fade.set_anchors_preset(Control.PRESET_FULL_RECT)
	_match_fade.color = PALETTE.SLATE
	_match_fade.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_match_fade.visible = false
	add_child(_match_fade)
	if not api.configure_lobby_player(_player_id):
		title_status.text = "Private lobbies unavailable • Practice Range is ready"
	else:
		title_status.text = "Checking private-lobby availability…"
		api.probe_lobby_readiness()
	_show(Screen.TITLE)

func _process(delta: float) -> void:
	if _screen not in [Screen.WAITING, Screen.TEARDOWN, Screen.MATCH] or _lobby_id.is_empty():
		return
	_poll_elapsed += delta
	_report_elapsed += delta
	_heartbeat_elapsed += delta
	_endpoint_renew_elapsed += delta
	if _poll_elapsed >= 1.0 and api.has_participant_access():
		_poll_elapsed = 0.0
		api.poll_lobby(_lobby_id)
		api.poll_network(_lobby_id)
	if not _leaving and _report_elapsed >= 3.0 and _bridge and api.has_participant_access():
		var report := _bridge.measurement_report()
		if not report.is_empty():
			_report_elapsed = 0.0
			api.submit_measurements(_lobby_id, JSON.stringify(report))
	if not _leaving and _endpoint_renew_elapsed >= 30.0 and api.has_participant_access():
		_endpoint_renew_elapsed = 0.0
		_try_register_endpoint("", 0, true)
	if (
		_screen == Screen.MATCH
		and _heartbeat_elapsed >= 1.0
		and _bridge
		and _bridge.local_is_authority
		and not _authority_input_hash.is_empty()
	):
		_heartbeat_elapsed = 0.0
		api.authority_heartbeat(_lobby_id, _authority_input_hash)

func _notification(what: int) -> void:
	if what != NOTIFICATION_WM_CLOSE_REQUEST:
		return
	if _lobby_id.is_empty():
		_clear_and_quit()
	else:
		_quit_after_leave = true
		_begin_leave()

func _connect_signals() -> void:
	api.readiness_changed.connect(_on_readiness)
	api.create_completed.connect(_on_created)
	api.invitation_copied.connect(_on_invitation_copied)
	api.join_completed.connect(_on_joined)
	api.lobby_updated.connect(_on_lobby_updated)
	api.network_updated.connect(_on_network_updated)
	api.endpoint_registered.connect(_on_endpoint_registered)
	api.report_completed.connect(_on_report_completed)
	api.start_completed.connect(_on_started)
	api.heartbeat_completed.connect(_on_heartbeat)
	api.leave_completed.connect(_on_left)
	api.end_completed.connect(_on_end_requested)
	api.request_failed.connect(_on_request_failed)
	peer_session.connected.connect(_on_peer_connected)
	peer_session.connection_failed.connect(_on_peer_failed)

func _on_readiness(create_authorized: bool, join_authorized: bool) -> void:
	create_button.disabled = not create_authorized
	join_button.disabled = not join_authorized
	launch_code_input.mouse_filter = Control.MOUSE_FILTER_STOP if create_authorized else Control.MOUSE_FILTER_IGNORE
	join_code_input.mouse_filter = Control.MOUSE_FILTER_STOP if join_authorized else Control.MOUSE_FILTER_IGNORE
	if create_authorized or join_authorized:
		title_status.text = "One private lobby is available"
	else:
		title_status.text = "Invite lobbies are not open yet • Practice Range is ready"

func _on_create_pressed() -> void:
	if SpurfireLobbyContract.clean_display_name(name_edit.text).is_empty():
		title_status.text = "Enter your rider name and one-use launch code."
		return
	_set_title_busy("Creating one private posse…")
	api.submit_create(name_edit.text)

func _on_join_pressed() -> void:
	if SpurfireLobbyContract.clean_display_name(name_edit.text).is_empty():
		title_status.text = SpurfireLobbyContract.SAFE_LOBBY_ERROR
		return
	_set_title_busy("Joining the posse…")
	api.submit_join(name_edit.text)

func _on_practice_pressed() -> void:
	_start_practice()

func _on_created(response_json: String) -> void:
	var response := _public_response(response_json)
	_lobby_id = str(response.get("lobby_id", ""))
	if _lobby_id.is_empty():
		_on_request_failed("create", SpurfireLobbyContract.SAFE_LOBBY_ERROR, "json")
		return
	api.auto_join_creator(_lobby_id, name_edit.text)

func _on_invitation_copied(_lobby_id_value: String) -> void:
	share_status.text = "One-use posse code copied explicitly. Clipboard history may retain it."
	waiting_status.text = "Share the code once; it is consumed by the first successful join."

func _on_joined(response_json: String) -> void:
	var response := _public_response(response_json)
	var lobby_value = response.get("lobby", response)
	if not lobby_value is Dictionary:
		_on_request_failed("join", SpurfireLobbyContract.SAFE_LOBBY_ERROR, "json")
		return
	_lobby = lobby_value as Dictionary
	_lobby_id = str(_lobby.get("lobby_id", _lobby_id))
	_session_generation = int(response.get("session_generation", 0))
	_manifest_public_key = str(response.get("manifest_public_key", ""))
	if (
		_manifest_public_key.is_empty()
		or not peer_session.bind_manifest_key(_manifest_public_key, _session_generation)
		or not peer_session.generate_session_key(_session_generation)
	):
		_on_request_failed("join", SpurfireLobbyContract.SAFE_LOBBY_ERROR, "public_projection")
		return
	if not _prepare_network_course(response):
		waiting_status.text = "Joined the roster, but peer setup failed. Leaving safely…"
		api.leave_lobby(_lobby_id)
		return
	_show(Screen.WAITING)
	_render_waiting()
	if api.has_creator_control():
		api.copy_invitation_to_clipboard(_lobby_id)

func _prepare_network_course(projection: Dictionary) -> bool:
	if _course == null:
		_course = COURSE_SCENE.instantiate()
		add_child(_course)
		_bind_capture_gate()
		if _course is Node3D:
			(_course as Node3D).visible = false
	var rider := _course.get_node_or_null("Rider") as CharacterBody3D
	var remote := _course.get_node_or_null("RemoteRider") as Node3D
	var old_peer := _course.get_node_or_null("PeerSession")
	var old_replication := _course.get_node_or_null("NetworkReplication")
	var m2 := _course.get_node_or_null("M2Gameplay")
	var combat_router := _course.get_node_or_null("Rider/CombatInput")
	var network_layer := _course.get_node_or_null("NetworkLayer")
	if rider == null or remote == null or m2 == null:
		return false
	peer_session.set("gameplay_rider_path", rider.get_path())
	_bridge = SpurfireLobbyPeerBridge.new()
	_bridge.name = "LobbyPeerBridge"
	_course.add_child(_bridge)
	_bridge.authority_departed.connect(_on_authority_departed)
	if not _bridge.configure({
		"peer_session": peer_session, "local_rider": rider,
		"remote_rider": remote, "combat_router": combat_router,
	}, _player_id):
		return false
	m2.set("replication", _bridge)
	if network_layer:
		network_layer.set("peer_session", peer_session)
		network_layer.set("replication", _bridge)
	if old_replication:
		old_replication.queue_free()
	if old_peer:
		old_peer.queue_free()
	return _bridge.apply_projection(projection)

func _on_peer_connected(address: String, port: int) -> void:
	if _leaving:
		return
	waiting_status.text = "Rider network online • registering this session…"
	_try_register_endpoint(address, port)

func _try_register_endpoint(address: String = "", port: int = 0, force: bool = false) -> void:
	if _leaving or (_endpoint_registered and not force) or _network_generation <= 0:
		return
	if address.is_empty():
		address = str(peer_session.get("tailnet_ip"))
	if port <= 0:
		port = int(peer_session.get("local_port"))
	var public_key := str(peer_session.session_public_key())
	var proof := str(peer_session.key_proof(
		_lobby_id, _player_id, _network_generation, _roster_revision, address, port
	))
	api.register_endpoint(
		_lobby_id, _network_generation, _roster_revision, address, port, public_key, proof
	)

func _on_endpoint_registered(response_json: String) -> void:
	var response := _public_response(response_json)
	if _leaving:
		return
	_endpoint_registered = true
	_registered_roster_revision = int(
		(response.get("session", {}) as Dictionary).get("roster_revision", _roster_revision)
	)
	if _bridge:
		_bridge.apply_projection(response)
	waiting_status.text = "Network ready • measuring the posse"

func _on_report_completed(_response_json: String) -> void:
	if not _leaving:
		waiting_status.text = "Network measured • waiting for the posse"

func _on_peer_failed(_message: String) -> void:
	waiting_status.text = "Peer network failed. Leaving safely…"
	_begin_leave()

func _on_authority_departed() -> void:
	if _screen == Screen.MATCH and not _leaving and _bridge:
		_bridge.begin_authority_migration()
		# The scene remains live while signed peers converge on the exactly-next
		# epoch and restore the bounded M2 checkpoint.
		waiting_status.text = "Host lost • restoring the posse…"

func _on_lobby_updated(response_json: String) -> void:
	var response := _public_response(response_json)
	if _leaving:
		return
	var lobby_value = response.get("lobby", response)
	if lobby_value is Dictionary:
		_lobby = lobby_value as Dictionary
	_network_generation = int(response.get("network_generation", _network_generation))
	_authority_input_hash = str(response.get("authority_input_hash", _authority_input_hash))
	var next_session_generation := int(response.get("session_generation", _session_generation))
	var next_manifest_key := str(response.get("manifest_public_key", _manifest_public_key))
	if next_session_generation != _session_generation or next_manifest_key != _manifest_public_key:
		if (
			not peer_session.bind_manifest_key(next_manifest_key, next_session_generation)
			or not peer_session.generate_session_key(next_session_generation)
		):
			_on_peer_failed("session identity rebind failed")
			return
		_session_generation = next_session_generation
		_manifest_public_key = next_manifest_key
		_endpoint_registered = false
	var next_roster_revision := int(response.get("roster_revision", _lobby.get("roster_revision", 0)))
	if next_roster_revision != _roster_revision:
		_roster_revision = next_roster_revision
		_endpoint_registered = _registered_roster_revision == _roster_revision
	if _bridge:
		_bridge.apply_projection(response)
	_try_register_endpoint()
	_render_waiting()
	var lobby_state := str(_lobby.get("state", ""))
	var session := response.get("session", {}) as Dictionary
	if (
		lobby_state in ["STARTING", "IN_MATCH"]
		and _screen == Screen.WAITING
		and _endpoint_registered
		and bool(session.get("secure", false))
	):
		_start_match()
	elif _screen == Screen.MATCH and lobby_state in ["CLOSING", "FAILED", "EXPIRED", "DESTROYED"]:
		_begin_leave()
		teardown_status.text = "Match ended • closing this lobby session safely…"

func _on_network_updated(response_json: String) -> void:
	var response := _public_response(response_json)
	_network_view = response
	_network_generation = int((response.get("backing", {}) as Dictionary).get("network_generation", 0))
	if not _leaving:
		_try_register_endpoint()
	_render_waiting()
	if _screen == Screen.TEARDOWN:
		teardown_status.text = SpurfireLobbyContract.cleanup_message(_network_view)
		var lifecycle := str((response.get("backing", {}) as Dictionary).get("network_lifecycle", ""))
		if lifecycle in ["DEDICATED_ABSENT", "SHARED_RESOURCES_CLEAN"]:
			_reset_to_title()

func _on_start_pressed() -> void:
	start_button.disabled = true
	waiting_status.text = "Starting peer-hosted free ride…"
	api.start_lobby(_lobby_id)

func _on_started(response_json: String) -> void:
	var response := _public_response(response_json)
	if _leaving:
		return
	_authority_input_hash = str(response.get("input_hash", ""))
	var next_generation := int(response.get("session_generation", 0))
	if (
		next_generation <= _session_generation
		or not peer_session.bind_manifest_key(_manifest_public_key, next_generation)
		or not peer_session.generate_session_key(next_generation)
	):
		_on_peer_failed("start session identity rebind failed")
		return
	_session_generation = next_generation
	_endpoint_registered = false
	_try_register_endpoint("", 0, true)
	# Gameplay starts only after a later capability-protected poll projects a
	# complete secure roster for this exact generation.

func _on_heartbeat(response_json: String) -> void:
	var response := _public_response(response_json)
	if not _leaving:
		_lobby["state"] = str(response.get("state", _lobby.get("state", "IN_MATCH")))

func _start_match() -> void:
	if _course is Node3D:
		(_course as Node3D).visible = true
	_show(Screen.MATCH)
	_fade_into_match()
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE

func _start_practice() -> void:
	_course = COURSE_SCENE.instantiate()
	add_child(_course)
	_bind_capture_gate()
	_show(Screen.MATCH)
	_fade_into_match()
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE

func _bind_capture_gate() -> void:
	if _course == null:
		return
	var gate := _course.get_node_or_null("CaptureLayer/CaptureGate")
	if gate and gate.has_signal(&"quit_requested"):
		var callback := Callable(self, "_on_gameplay_quit_requested")
		if not gate.is_connected(&"quit_requested", callback):
			gate.connect(&"quit_requested", callback)

func _on_gameplay_quit_requested() -> void:
	if _lobby_id.is_empty():
		_clear_and_quit()
	else:
		_quit_after_leave = true
		_begin_leave()

func _on_leave_pressed() -> void:
	_begin_leave()

func _begin_leave() -> void:
	if _leaving:
		return
	_leaving = true
	_show(Screen.TEARDOWN)
	teardown_status.text = "Leaving peers and closing your lobby session…"
	if _bridge:
		_bridge.send_leave()
	await _stop_peer_transport()
	api.leave_lobby(_lobby_id)
	if _quit_after_leave:
		_quit_after_budget()

func _stop_peer_transport() -> void:
	peer_session.shutdown()
	var deadline := Time.get_ticks_msec() + 1000
	while str(peer_session.get("connection_state")) != "offline" and Time.get_ticks_msec() < deadline:
		await get_tree().process_frame

func _on_left(_response_json: String) -> void:
	if not _quit_after_leave:
		_reset_to_title()

func _on_end_pressed() -> void:
	if _leaving:
		return
	_leaving = true
	_show(Screen.TEARDOWN)
	teardown_status.text = "Closing the private lobby…"
	if _bridge:
		_bridge.send_leave()
	await _stop_peer_transport()
	api.end_lobby(_lobby_id)

func _on_end_requested(_response_json: String) -> void:
	teardown_status.text = "Still cleaning up — you can close the game; we'll keep at it."
	api.poll_network(_lobby_id)

func _on_request_failed(operation: String, safe_message: String, _safe_code: String) -> void:
	if operation in ["lobby", "network"]:
		return
	if _screen == Screen.TITLE:
		title_status.text = safe_message
		if operation == "create":
			api.capture_launch_code()
		elif operation == "join":
			api.capture_join_code()
		api.probe_lobby_readiness()
	elif _screen == Screen.TEARDOWN:
		teardown_status.text = "Still cleaning up — you can close the game; we'll keep at it."
	else:
		waiting_status.text = safe_message

func _public_response(value: String) -> Dictionary:
	var parsed = JSON.parse_string(value)
	return parsed as Dictionary if parsed is Dictionary else {}

func _render_waiting() -> void:
	if _lobby.is_empty():
		return
	lobby_name_label.text = str(_lobby.get("display_name", "PRIVATE POSSE")).to_upper()
	start_button.visible = api.has_creator_control()
	end_button.visible = api.has_creator_control()
	start_button.disabled = (
		(_lobby.get("roster", []) as Array).size() < 2
		or str(_lobby.get("state", "")) != "READY"
	)
	for child in roster_box.get_children():
		child.queue_free()
	var health := _bridge.peer_health() if _bridge else {}
	var roster_index := 0
	for row in SpurfireLobbyContract.safe_roster(_lobby, _player_id, health):
		var line := HBoxContainer.new()
		line.add_theme_constant_override("separation", 10)
		var pip := Label.new()
		pip.text = "▌"
		pip.add_theme_color_override("font_color", ROSTER_ACCENTS[roster_index % ROSTER_ACCENTS.size()])
		pip.add_theme_font_size_override("font_size", 28)
		line.add_child(pip)
		var label := Label.new()
		var badges := ""
		if bool(row.you): badges += " • YOU"
		if bool(row.authority): badges += " • HOST"
		var rtt := "measuring…" if row.rtt_ms == null else "%d ms" % int(row.rtt_ms)
		label.text = "%s%s\n%s • %s • %s" % [
			str(row.display_name), badges, _route_label(str(row.route)), rtt, str(row.freshness).to_upper()
		]
		label.add_theme_color_override("font_color", PALETTE.CREAM)
		line.add_child(label)
		roster_box.add_child(line)
		roster_index += 1
	var count := (_lobby.get("roster", []) as Array).size()
	var direct_value = ((_network_view.get("routes", {}) as Dictionary).get("direct_count", {}) as Dictionary).get("value", null)
	var rtt_value = ((_network_view.get("application_quality", {}) as Dictionary).get("application_rtt_ms_median", {}) as Dictionary).get("value", null)
	var direct_text := "measuring…" if direct_value == null else "%d direct paths" % int(direct_value)
	var rtt_text := "RTT unknown" if rtt_value == null else "median %d ms" % int(rtt_value)
	network_summary.text = "%d riders • %s • %s" % [count, direct_text, rtt_text]

func _route_label(value: String) -> String:
	match value.to_lower():
		"direct": return "Direct"
		"peer_relay": return "Peer Relay"
		"derp_relay": return "DERP Relay"
		"unavailable": return "Unavailable"
		_: return "Measuring…"

func _fade_into_match() -> void:
	_match_fade.modulate.a = 1.0
	_match_fade.visible = true
	var tween := create_tween()
	tween.tween_property(_match_fade, "modulate:a", 0.0, 0.25)
	tween.tween_callback(func(): _match_fade.visible = false)

func _set_title_busy(message: String) -> void:
	create_button.disabled = true
	join_button.disabled = true
	title_status.text = message

func _show(screen: Screen) -> void:
	_screen = screen
	background.visible = screen != Screen.MATCH
	screens.visible = screen != Screen.MATCH
	title_screen.visible = screen == Screen.TITLE
	waiting_screen.visible = screen == Screen.WAITING
	teardown_screen.visible = screen == Screen.TEARDOWN
	if _course is Node3D and screen != Screen.MATCH:
		(_course as Node3D).visible = false

func _reset_to_title() -> void:
	api.cancel_lobby_operations()
	peer_session.clear_lobby_session()
	if is_instance_valid(_course):
		_course.queue_free()
	_course = null
	_bridge = null
	_lobby_id = ""
	_lobby.clear()
	_network_view.clear()
	share_status.text = "One-use code not copied yet."
	_endpoint_registered = false
	_registered_roster_revision = 0
	_leaving = false
	_network_generation = 0
	_roster_revision = 0
	_authority_input_hash = ""
	_heartbeat_elapsed = 0.0
	_endpoint_renew_elapsed = 0.0
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	_show(Screen.TITLE)
	title_status.text = "Restart the client to enter another private lobby."
	create_button.disabled = true
	join_button.disabled = true

func _quit_after_budget() -> void:
	await get_tree().create_timer(2.0).timeout
	_clear_and_quit()

func _clear_and_quit() -> void:
	api.cancel_lobby_operations()
	peer_session.clear_lobby_session()
	share_status.text = "One-use code not copied yet."
	get_tree().quit()
