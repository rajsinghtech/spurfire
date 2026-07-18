extends Control

const COURSE_SCENE := preload("res://scenes/graybox_course.tscn")
const RANDOM_NAMES := ["Dusty", "Sundown", "Juniper", "Longshot", "Mesa", "Coyote", "Red Rock"]

enum Screen { TITLE, WAITING, TEARDOWN, MATCH }

@export var service_origin := "https://spurfire.rajsingh.info"
@onready var api: SpurfireLobbyHttpClient = $LobbyHttpClient
@onready var peer_session: Node = $PeerSession
@onready var background: ColorRect = $Background
@onready var screens: Control = $Screens
@onready var title_screen: Control = $Screens/Title
@onready var waiting_screen: Control = $Screens/Waiting
@onready var teardown_screen: Control = $Screens/Teardown
@onready var name_edit: LineEdit = $Screens/Title/Card/Margin/VBox/Name
@onready var create_grant_edit: LineEdit = $Screens/Title/Card/Margin/VBox/CreateGrant
@onready var join_code_edit: LineEdit = $Screens/Title/Card/Margin/VBox/JoinCode
@onready var create_button: Button = $Screens/Title/Card/Margin/VBox/Actions/Create
@onready var join_button: Button = $Screens/Title/Card/Margin/VBox/Actions/Join
@onready var title_status: Label = $Screens/Title/Card/Margin/VBox/Status
@onready var lobby_name_label: Label = $Screens/Waiting/Card/Margin/VBox/LobbyName
@onready var share_code_edit: LineEdit = $Screens/Waiting/Card/Margin/VBox/ShareCode
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
var _course: Node = null
var _bridge: SpurfireLobbyPeerBridge = null
var _creator_join_pending := false
var _endpoint_registered := false
var _poll_elapsed := 0.0
var _quit_after_leave := false

func _ready() -> void:
	get_tree().auto_accept_quit = false
	name_edit.text = RANDOM_NAMES[randi() % RANDOM_NAMES.size()]
	_player_id = SpurfireLobbyContract.new_uuid_v4()
	_connect_signals()
	if not api.configure(service_origin, _player_id):
		title_status.text = "Private lobbies unavailable • Practice Range is ready"
	else:
		title_status.text = "Checking private-lobby availability…"
		api.probe_readiness()
	_show(Screen.TITLE)

func _process(delta: float) -> void:
	if _screen not in [Screen.WAITING, Screen.TEARDOWN] or _lobby_id.is_empty():
		return
	_poll_elapsed += delta
	if _poll_elapsed < 1.0 or not api.has_participant_access():
		return
	_poll_elapsed = 0.0
	api.poll_lobby(_lobby_id)
	api.poll_network(_lobby_id)

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
	api.invitation_completed.connect(_on_invitation)
	api.join_completed.connect(_on_joined)
	api.lobby_updated.connect(_on_lobby_updated)
	api.network_updated.connect(_on_network_updated)
	api.endpoint_registered.connect(_on_endpoint_registered)
	api.start_completed.connect(_on_started)
	api.leave_completed.connect(_on_left)
	api.end_completed.connect(_on_end_requested)
	api.request_failed.connect(_on_request_failed)
	peer_session.connected.connect(_on_peer_connected)
	peer_session.connection_failed.connect(_on_peer_failed)

func _on_readiness(create_authorized: bool, join_authorized: bool) -> void:
	create_button.disabled = not create_authorized
	join_button.disabled = not join_authorized
	create_grant_edit.editable = create_authorized
	join_code_edit.editable = join_authorized
	if create_authorized or join_authorized:
		title_status.text = "One private lobby is available"
	else:
		title_status.text = "Invite lobbies are not open yet • Practice Range is ready"

func _on_create_pressed() -> void:
	var grant := create_grant_edit.text.strip_edges()
	create_grant_edit.clear()
	if grant.is_empty() or SpurfireLobbyContract.clean_display_name(name_edit.text).is_empty():
		title_status.text = "Enter your rider name and one-use alpha grant."
		return
	_set_title_busy("Creating one private posse…")
	api.create_lobby(name_edit.text, grant)
	grant = ""

func _on_join_pressed() -> void:
	var parsed := SpurfireLobbyContract.parse_join_code(join_code_edit.text)
	join_code_edit.clear()
	if parsed.is_empty():
		title_status.text = SpurfireLobbyContract.SAFE_LOBBY_ERROR
		return
	_lobby_id = str(parsed.get("lobby_id", ""))
	var invitation := str(parsed.get("invitation", ""))
	parsed.clear()
	_set_title_busy("Joining the posse…")
	api.join_lobby(_lobby_id, invitation, name_edit.text)
	invitation = ""

func _on_practice_pressed() -> void:
	_start_practice()

func _on_created(response: Dictionary) -> void:
	_lobby_id = str(response.get("lobby_id", ""))
	if _lobby_id.is_empty():
		_on_request_failed("create", SpurfireLobbyContract.SAFE_LOBBY_ERROR)
		return
	_creator_join_pending = true
	api.issue_invitation(_lobby_id)

func _on_invitation(join_code: String) -> void:
	if _creator_join_pending:
		_creator_join_pending = false
		var parsed := SpurfireLobbyContract.parse_join_code(join_code)
		join_code = ""
		if parsed.is_empty():
			_on_request_failed("invitation", SpurfireLobbyContract.SAFE_LOBBY_ERROR)
			return
		var invitation := str(parsed.get("invitation", ""))
		parsed.clear()
		api.join_lobby(_lobby_id, invitation, name_edit.text)
		invitation = ""
		return
	share_code_edit.text = join_code
	waiting_status.text = "Share this one-use code with your posse."
	join_code = ""

func _on_joined(response: Dictionary, enrollment_key: String) -> void:
	var lobby_value = response.get("lobby", response)
	if not lobby_value is Dictionary:
		enrollment_key = ""
		_on_request_failed("join", SpurfireLobbyContract.SAFE_LOBBY_ERROR)
		return
	_lobby = lobby_value as Dictionary
	_lobby_id = str(_lobby.get("lobby_id", _lobby_id))
	if not _prepare_network_course(response):
		enrollment_key = ""
		waiting_status.text = "Joined the roster, but peer setup failed. Leaving safely…"
		api.leave_lobby(_lobby_id)
		return
	var hostname := "spurfire-rider-%s" % _player_id.left(8)
	if not peer_session.connect_rustscale(hostname, enrollment_key, 41643):
		enrollment_key = ""
		waiting_status.text = "Peer network did not start. Leaving safely…"
		api.leave_lobby(_lobby_id)
		return
	enrollment_key = ""
	_show(Screen.WAITING)
	_render_waiting()
	if api.has_creator_control():
		api.issue_invitation(_lobby_id)

func _prepare_network_course(projection: Dictionary) -> bool:
	if _course == null:
		_course = COURSE_SCENE.instantiate()
		add_child(_course)
		if _course is Node3D:
			(_course as Node3D).visible = false
	var rider := _course.get_node_or_null("Rider") as CharacterBody3D
	var remote := _course.get_node_or_null("RemoteRider") as Node3D
	var old_peer := _course.get_node_or_null("PeerSession")
	var old_replication := _course.get_node_or_null("NetworkReplication")
	var m2 := _course.get_node_or_null("M2Gameplay")
	var network_layer := _course.get_node_or_null("NetworkLayer")
	if rider == null or remote == null or m2 == null:
		return false
	peer_session.set("gameplay_rider_path", rider.get_path())
	_bridge = SpurfireLobbyPeerBridge.new()
	_bridge.name = "LobbyPeerBridge"
	_course.add_child(_bridge)
	if not _bridge.configure({"peer_session": peer_session, "local_rider": rider, "remote_rider": remote}, _player_id):
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
	waiting_status.text = "Rider network online • registering this session…"
	_try_register_endpoint(address, port)

func _try_register_endpoint(address: String = "", port: int = 0) -> void:
	if _endpoint_registered or _network_generation <= 0:
		return
	if address.is_empty():
		address = str(peer_session.get("tailnet_ip"))
	if port <= 0:
		port = int(peer_session.get("local_port"))
	api.register_endpoint(_lobby_id, _network_generation, _roster_revision, address, port)

func _on_endpoint_registered(response: Dictionary) -> void:
	_endpoint_registered = true
	if _bridge:
		_bridge.apply_projection(response)
	waiting_status.text = "Network ready • waiting for the posse"

func _on_peer_failed(_message: String) -> void:
	waiting_status.text = "Peer network failed. Leaving safely…"
	api.leave_lobby(_lobby_id)

func _on_lobby_updated(response: Dictionary) -> void:
	var lobby_value = response.get("lobby", response)
	if lobby_value is Dictionary:
		_lobby = lobby_value as Dictionary
	_roster_revision = int(response.get("roster_revision", _lobby.get("roster_revision", 0)))
	if _bridge:
		_bridge.apply_projection(response)
	_render_waiting()
	if str(_lobby.get("state", "")) in ["STARTING", "IN_MATCH"] and _screen == Screen.WAITING:
		_start_match()

func _on_network_updated(response: Dictionary) -> void:
	_network_view = response
	_network_generation = int((response.get("backing", {}) as Dictionary).get("network_generation", 0))
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

func _on_started(response: Dictionary) -> void:
	if _bridge:
		_bridge.apply_projection(response)
	_start_match()

func _start_match() -> void:
	if _course is Node3D:
		(_course as Node3D).visible = true
	_show(Screen.MATCH)
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED

func _start_practice() -> void:
	_course = COURSE_SCENE.instantiate()
	add_child(_course)
	_show(Screen.MATCH)
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED

func _on_leave_pressed() -> void:
	_begin_leave()

func _begin_leave() -> void:
	_show(Screen.TEARDOWN)
	teardown_status.text = "Leaving peers and closing your lobby session…"
	if _bridge:
		_bridge.send_leave()
	peer_session.shutdown()
	api.leave_lobby(_lobby_id)
	if _quit_after_leave:
		_quit_after_budget()

func _on_left(_response: Dictionary) -> void:
	if not _quit_after_leave:
		_reset_to_title()

func _on_end_pressed() -> void:
	_show(Screen.TEARDOWN)
	teardown_status.text = "Closing the private lobby…"
	if _bridge:
		_bridge.send_leave()
	peer_session.shutdown()
	api.end_lobby(_lobby_id)

func _on_end_requested(_response: Dictionary) -> void:
	teardown_status.text = "Still cleaning up — you can close the game; we'll keep at it."
	api.poll_network(_lobby_id)

func _on_request_failed(operation: String, safe_message: String) -> void:
	if operation in ["lobby", "network"]:
		return
	if _screen == Screen.TITLE:
		title_status.text = safe_message
		api.probe_readiness()
	elif _screen == Screen.TEARDOWN:
		teardown_status.text = "Still cleaning up — you can close the game; we'll keep at it."
	else:
		waiting_status.text = safe_message

func _render_waiting() -> void:
	if _lobby.is_empty():
		return
	lobby_name_label.text = str(_lobby.get("display_name", "PRIVATE POSSE")).to_upper()
	start_button.visible = api.has_creator_control()
	end_button.visible = api.has_creator_control()
	start_button.disabled = (_lobby.get("roster", []) as Array).size() < 2
	for child in roster_box.get_children():
		child.queue_free()
	var health := _bridge.peer_health() if _bridge else {}
	for row in SpurfireLobbyContract.safe_roster(_lobby, _player_id, health):
		var label := Label.new()
		var badges := ""
		if bool(row.you): badges += " • YOU"
		if bool(row.authority): badges += " • HOST"
		var rtt := "measuring…" if row.rtt_ms == null else "%d ms" % int(row.rtt_ms)
		label.text = "%s%s\n    %s • %s • %s" % [
			str(row.display_name), badges, _route_label(str(row.route)), rtt, str(row.freshness).to_upper()
		]
		roster_box.add_child(label)
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
	api.clear_lobby_secrets()
	peer_session.clear_lobby_session()
	if is_instance_valid(_course):
		_course.queue_free()
	_course = null
	_bridge = null
	_lobby_id = ""
	_lobby.clear()
	_network_view.clear()
	share_code_edit.clear()
	_endpoint_registered = false
	_network_generation = 0
	_roster_revision = 0
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	_show(Screen.TITLE)
	title_status.text = "Restart the client to enter another private lobby."
	create_button.disabled = true
	join_button.disabled = true

func _quit_after_budget() -> void:
	await get_tree().create_timer(2.0).timeout
	_clear_and_quit()

func _clear_and_quit() -> void:
	api.clear_lobby_secrets()
	peer_session.clear_lobby_session()
	create_grant_edit.clear()
	join_code_edit.clear()
	share_code_edit.clear()
	get_tree().quit()
