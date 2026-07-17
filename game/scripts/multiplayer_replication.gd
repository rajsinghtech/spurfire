extends Node

@export var peer_session: Node
@export var local_horse: CharacterBody3D
@export var remote_rider: Node3D

var destination_ip := ""
var destination_port := 0
var local_is_authority := false
var simulation_tick := 0
var _demo_mode := false
var _demo_node := ""
var _demo_dir := ""
var _received_demo_snapshot := false

const DEMO_LOBBY := "00000000-0000-4000-8000-000000000001"
const DEMO_PLAYER_A := "00000000-0000-4000-8000-000000000002"
const DEMO_PLAYER_B := "00000000-0000-4000-8000-000000000003"

func _ready() -> void:
	if peer_session:
		peer_session.packet_received.connect(_on_packet_received)
	_demo_mode = OS.get_environment("SPURFIRE_P2P_DEMO") == "1"
	if _demo_mode:
		_start_demo.call_deferred()

func _process(_delta: float) -> void:
	if not _demo_mode or not destination_ip.is_empty():
		return
	var other := "b" if _demo_node == "a" else "a"
	var endpoint_path := _demo_dir.path_join("endpoint-%s" % other)
	if not FileAccess.file_exists(endpoint_path):
		return
	var endpoint := FileAccess.get_file_as_string(endpoint_path).strip_edges().split(":")
	if endpoint.size() != 2:
		return
	if configure_remote(endpoint[0], int(endpoint[1]), _demo_node == "a"):
		print("SPURFIRE_GODOT_P2P_READY local=%s remote=%s:%d" % [_demo_node, destination_ip, destination_port])

func _start_demo() -> void:
	_demo_node = OS.get_environment("SPURFIRE_P2P_DEMO_NODE").to_lower()
	_demo_dir = OS.get_environment("SPURFIRE_P2P_DEMO_DIR")
	var key_path := OS.get_environment("SPURFIRE_P2P_DEMO_KEY_FILE")
	if _demo_node not in ["a", "b"] or _demo_dir.is_empty() or not FileAccess.file_exists(key_path):
		push_error("P2P demo environment is incomplete")
		return
	var local_player := DEMO_PLAYER_A if _demo_node == "a" else DEMO_PLAYER_B
	if not peer_session.configure_session(DEMO_LOBBY, local_player, DEMO_PLAYER_A, Time.get_ticks_msec()):
		push_error("P2P demo session configuration failed")
		return
	peer_session.connected.connect(_on_demo_connected)
	peer_session.connection_failed.connect(_on_demo_failed)
	var auth_key := FileAccess.get_file_as_string(key_path).strip_edges()
	if not peer_session.connect_rustscale("spurfire-godot-%s" % _demo_node, auth_key, 41643):
		push_error("P2P demo RustScale worker did not start")
		return
	auth_key = ""
	DirAccess.remove_absolute(key_path)
	local_is_authority = _demo_node == "a"
	local_horse.global_position.x = -5.0 if _demo_node == "a" else 5.0
	DisplayServer.window_set_title("Spurfire P2P — Rider %s" % _demo_node.to_upper())

func _on_demo_connected(ip: String, port: int) -> void:
	var file := FileAccess.open(_demo_dir.path_join("endpoint-%s" % _demo_node), FileAccess.WRITE)
	if file:
		file.store_string("%s:%d" % [ip, port])

func _on_demo_failed(message: String) -> void:
	push_error("P2P demo connection failed: %s" % message)

## Called by the lobby/join flow after PeerSession.configure_session and enrollment.
func configure_remote(ip: String, port: int, is_authority: bool) -> bool:
	if ip.is_empty() or port <= 0 or port > 65535:
		return false
	destination_ip = ip
	destination_port = port
	local_is_authority = is_authority
	return true

func _physics_process(_delta: float) -> void:
	simulation_tick += 1
	if destination_ip.is_empty() or peer_session == null or local_horse == null:
		return
	var packet := PackedByteArray()
	if (_demo_mode or local_is_authority) and simulation_tick % 3 == 0:
		packet = peer_session.make_rider_snapshot(
			simulation_tick,
			local_horse.global_position,
			local_horse.velocity,
			rad_to_deg(local_horse.rotation.y)
		)
	elif not local_is_authority:
		var throttle := roundi(Input.get_axis(&"move_back", &"move_forward") * 1000.0)
		var steer := roundi(Input.get_axis(&"steer_left", &"steer_right") * 1000.0)
		var buttons := 1 if Input.is_action_pressed(&"jump") else 0
		packet = peer_session.make_rider_input(simulation_tick, throttle, steer, buttons)
	elif simulation_tick % 6 == 0:
		packet = peer_session.make_heartbeat(simulation_tick)
	if not packet.is_empty():
		peer_session.send_packet(packet, destination_ip, destination_port)

func _on_packet_received(packet: PackedByteArray, _source_ip: String, _source_port: int) -> void:
	if peer_session.accept_packet(packet, Time.get_ticks_msec()) != 0:
		return
	var payload := peer_session.decode_packet(packet) as Dictionary
	if payload.get("type", "") == "rider_snapshot" and remote_rider:
		remote_rider.visible = true
		if _demo_mode and not _received_demo_snapshot:
			_received_demo_snapshot = true
			print("SPURFIRE_GODOT_P2P_SNAPSHOT local=%s sender=%s" % [_demo_node, payload.get("sender", "unknown")])
		remote_rider.push_snapshot(
			int(payload.get("tick", 0)),
			payload.get("position", Vector3.ZERO),
			payload.get("velocity", Vector3.ZERO),
			float(payload.get("yaw_degrees", 0.0))
		)
