extends Node

@export var peer_session: Node
@export var local_horse: CharacterBody3D
@export var local_rider: CharacterBody3D
@export var remote_rider: Node3D

var destination_ip := ""
var destination_port := 0
var local_is_authority := false
var simulation_tick := 0

var _demo_mode := false
var _demo_node := ""
var _demo_dir := ""
var _demo_ready_logged := false
var _demo_sent_count := 0
var _demo_rejected_count := 0
var _demo_snapshot_count := 0
var _demo_max_gap_msec := 0
var _demo_last_snapshot_msec := 0
var _last_route_query_msec := 0
var _peers: Dictionary = {}
var _remote_riders: Dictionary = {}
var _seen_snapshot_senders: Dictionary = {}

const DEMO_LOBBY := "00000000-0000-4000-8000-000000000001"
const DEMO_PLAYERS := {
	"a": "00000000-0000-4000-8000-000000000002",
	"b": "00000000-0000-4000-8000-000000000003",
	"c": "00000000-0000-4000-8000-000000000004",
}
const DEMO_NODES := ["a", "b", "c"]
const RIDER_COLORS := [Color("5ec8e5"), Color("ffb84d"), Color("d979ff")]

func _ready() -> void:
	if peer_session:
		peer_session.packet_received.connect(_on_packet_received)
		peer_session.route_updated.connect(_on_route_updated)
	_demo_mode = OS.get_environment("SPURFIRE_P2P_DEMO") == "1"
	if _demo_mode:
		peer_session.set_insecure_demo_mode(true)
		_start_demo.call_deferred()

func _process(_delta: float) -> void:
	if not _demo_mode:
		return
	_discover_demo_peers()
	var now := Time.get_ticks_msec()
	if now - _last_route_query_msec >= 1000:
		_last_route_query_msec = now
		for peer: Dictionary in _peers.values():
			peer_session.query_route(str(peer.ip))

func _start_demo() -> void:
	_demo_node = OS.get_environment("SPURFIRE_P2P_DEMO_NODE").to_lower()
	_demo_dir = OS.get_environment("SPURFIRE_P2P_DEMO_DIR")
	var enrollment_file := OS.get_environment("SPURFIRE_P2P_DEMO_KEY_FILE")
	if _demo_node not in DEMO_NODES or _demo_dir.is_empty() or not FileAccess.file_exists(enrollment_file):
		push_error("P2P demo environment is incomplete")
		return
	if not peer_session.configure_session(
		DEMO_LOBBY,
		DEMO_PLAYERS[_demo_node],
		DEMO_PLAYERS["a"],
		Time.get_ticks_msec()
	):
		push_error("P2P demo session configuration failed")
		return
	peer_session.connected.connect(_on_demo_connected)
	peer_session.connection_failed.connect(_on_demo_failed)
	if not peer_session.connect_demo_peer("spurfire-godot-%s" % _demo_node, 41643):
		push_error("P2P demo RustScale worker did not start")
		return
	local_is_authority = _demo_node == "a"
	local_horse.global_position.x = float(DEMO_NODES.find(_demo_node) - 1) * 8.0
	DisplayServer.window_set_title("Spurfire P2P — Rider %s" % _demo_node.to_upper())

func _discover_demo_peers() -> void:
	for node_name in DEMO_NODES:
		if node_name == _demo_node or _peers.has(node_name):
			continue
		var endpoint_path := _demo_dir.path_join("endpoint-%s" % node_name)
		if not FileAccess.file_exists(endpoint_path):
			continue
		var endpoint := FileAccess.get_file_as_string(endpoint_path).strip_edges().split(":")
		if endpoint.size() != 2:
			continue
		_peers[node_name] = {
			"name": node_name,
			"player_id": DEMO_PLAYERS[node_name],
			"ip": endpoint[0],
			"port": int(endpoint[1]),
			"route": "UNKNOWN",
			"rtt_ms": -1,
			"rtt_logged": false,
			"last_seen_ms": 0,
		}
	if _peers.size() == DEMO_NODES.size() - 1 and not _demo_ready_logged:
		_demo_ready_logged = true
		print("SPURFIRE_GODOT_P2P_READY local=%s peers=%d" % [_demo_node, _peers.size()])

func _on_demo_connected(ip: String, port: int) -> void:
	var file := FileAccess.open(_demo_dir.path_join("endpoint-%s" % _demo_node), FileAccess.WRITE)
	if file:
		file.store_string("%s:%d" % [ip, port])

func _on_demo_failed(message: String) -> void:
	push_error("P2P demo connection failed: %s" % message)

## Called by the lobby/join flow after PeerSession configuration and enrollment.
func configure_remote(ip: String, port: int, is_authority: bool) -> bool:
	if ip.is_empty() or port <= 0 or port > 65535:
		return false
	destination_ip = ip
	destination_port = port
	local_is_authority = is_authority
	_peers["peer"] = {
		"name": "peer",
		"player_id": "",
		"ip": ip,
		"port": port,
		"route": "UNKNOWN",
		"rtt_ms": -1,
		"rtt_logged": false,
		"last_seen_ms": 0,
	}
	return true

## The M2 gameplay coordinator owns the sole absolute simulation clock.
func advance_shared_tick(tick: int, stance_changed: bool = false) -> void:
	if tick <= simulation_tick:
		return
	simulation_tick = tick
	if peer_session == null or local_rider == null or (_peers.is_empty() and destination_ip.is_empty()):
		return
	var packet := PackedByteArray()
	if (_demo_mode or local_is_authority) and (simulation_tick % 2 == 0 or stance_changed):
		packet = peer_session.make_rider_snapshot(
			simulation_tick,
			str(peer_session.get("local_player_id")),
			local_rider.global_position,
			local_rider.velocity,
			rad_to_deg(local_rider.rotation.y),
			int(local_rider.get("stance_id"))
		)
	elif not local_is_authority:
		var throttle := roundi(Input.get_axis(&"move_back", &"move_forward") * 1000.0)
		var steer := roundi(Input.get_axis(&"steer_left", &"steer_right") * 1000.0)
		var buttons := 0
		if Input.is_action_just_pressed(&"jump"):
			buttons |= 1
		if Input.is_action_just_pressed(&"combat_interact"):
			buttons |= 2
		packet = peer_session.make_rider_input(simulation_tick, throttle, steer, buttons)
	elif simulation_tick % 6 == 0:
		packet = peer_session.make_heartbeat(simulation_tick)
	if not packet.is_empty():
		_send_to_all(packet)
		if _demo_mode:
			_demo_sent_count += 1

	if simulation_tick % 60 == 0:
		for peer: Dictionary in _peers.values():
			var probe: PackedByteArray = peer_session.make_probe(simulation_tick, Time.get_ticks_msec(), false)
			peer_session.send_packet(probe, str(peer.ip), int(peer.port))

func _send_to_all(packet: PackedByteArray) -> void:
	if _demo_mode:
		for peer: Dictionary in _peers.values():
			peer_session.send_packet(packet, str(peer.ip), int(peer.port))
	elif not destination_ip.is_empty():
		peer_session.send_packet(packet, destination_ip, destination_port)

func _on_packet_received(
	packet: PackedByteArray, source_ip: String, source_port: int, _source_node_key: String
) -> void:
	var outcome := int(peer_session.accept_packet(packet, Time.get_ticks_msec()))
	if outcome != 0:
		if _demo_mode:
			_demo_rejected_count += 1
			if _demo_rejected_count <= 3 or _demo_rejected_count % 40 == 0:
				print("SPURFIRE_GODOT_P2P_REJECT local=%s outcome=%d count=%d" % [_demo_node, outcome, _demo_rejected_count])
		return
	var now := Time.get_ticks_msec()
	var peer_name := _peer_name_for_ip(source_ip)
	if not peer_name.is_empty():
		var source_peer: Dictionary = _peers[peer_name]
		source_peer.last_seen_ms = now
	var payload := peer_session.decode_packet(packet) as Dictionary
	match str(payload.get("type", "")):
		"probe":
			if bool(payload.get("reply", false)):
				if not peer_name.is_empty():
					var measured_peer: Dictionary = _peers[peer_name]
					measured_peer.rtt_ms = maxi(0, now - int(payload.get("nonce", now)))
					if _demo_mode and not bool(measured_peer.rtt_logged):
						measured_peer.rtt_logged = true
						print("SPURFIRE_GODOT_P2P_RTT local=%s peer=%s rtt_ms=%d" % [_demo_node, peer_name, int(measured_peer.rtt_ms)])
			else:
				var reply: PackedByteArray = peer_session.make_probe(
					simulation_tick,
					int(payload.get("nonce", now)),
					true
				)
				peer_session.send_packet(reply, source_ip, source_port)
		"rider_snapshot":
			_apply_remote_snapshot(payload, now)

func _apply_remote_snapshot(payload: Dictionary, now: int) -> void:
	var sender := str(payload.get("sender", ""))
	var rider := _remote_rider_for(sender)
	if rider == null:
		return
	rider.visible = true
	if _demo_mode:
		if _demo_last_snapshot_msec > 0:
			_demo_max_gap_msec = maxi(_demo_max_gap_msec, now - _demo_last_snapshot_msec)
		_demo_last_snapshot_msec = now
		_demo_snapshot_count += 1
		if not _seen_snapshot_senders.has(sender):
			_seen_snapshot_senders[sender] = true
			print("SPURFIRE_GODOT_P2P_SNAPSHOT local=%s sender=%s" % [_demo_node, sender])
		elif _demo_snapshot_count % 120 == 0:
			print("SPURFIRE_GODOT_P2P_HEALTH local=%s snapshots=%d max_gap_ms=%d" % [_demo_node, _demo_snapshot_count, _demo_max_gap_msec])
			_demo_max_gap_msec = 0
	rider.push_snapshot(
		int(payload.get("tick", 0)),
		payload.get("position", Vector3.ZERO),
		payload.get("velocity", Vector3.ZERO),
		float(payload.get("yaw_degrees", 0.0)),
		int(payload.get("stance_id", 1))
	)

func _remote_rider_for(sender: String) -> Node3D:
	if _remote_riders.has(sender):
		return _remote_riders[sender]
	var rider: Node3D
	if _remote_riders.is_empty():
		rider = remote_rider
	else:
		rider = remote_rider.duplicate() as Node3D
		remote_rider.get_parent().add_child(rider)
		rider.name = "RemoteRider_%d" % _remote_riders.size()
	if rider == null:
		return null
	var body := rider.get_node_or_null("RiderProxy/Body") as MeshInstance3D
	if body:
		var material := StandardMaterial3D.new()
		material.albedo_color = RIDER_COLORS[_remote_riders.size() % RIDER_COLORS.size()]
		material.roughness = 0.75
		body.material_override = material
	_remote_riders[sender] = rider
	return rider

func _peer_name_for_ip(ip: String) -> String:
	for name: String in _peers:
		var peer: Dictionary = _peers[name]
		if str(peer.ip) == ip:
			return name
	return ""

func _on_route_updated(peer_ip: String, route: String) -> void:
	var peer_name := _peer_name_for_ip(peer_ip)
	if not peer_name.is_empty():
		var peer: Dictionary = _peers[peer_name]
		var normalized := route.to_upper()
		if str(peer.route) != normalized:
			peer.route = normalized
			if _demo_mode:
				print("SPURFIRE_GODOT_P2P_ROUTE local=%s peer=%s route=%s" % [_demo_node, peer_name, normalized])

func get_peer_status() -> Array:
	var authority_id := str(peer_session.get("authority_player_id"))
	var local_name := _demo_node if _demo_mode else "you"
	var local_ip := str(peer_session.get("tailnet_ip"))
	var local_port := int(peer_session.get("local_port"))
	var local_endpoint := "%s:%d" % [local_ip, local_port] if not local_ip.is_empty() else "--"
	var result: Array = [{
		"name": local_name,
		"you": true,
		"authority": _demo_mode and DEMO_PLAYERS.get(local_name, "") == authority_id,
		"route": "LOCAL",
		"endpoint": local_endpoint,
		"rtt_ms": 0,
		"last_seen_ms": 0,
	}]
	var names := _peers.keys()
	names.sort()
	var now := Time.get_ticks_msec()
	for name: String in names:
		var peer: Dictionary = _peers[name]
		result.append({
			"name": name,
			"you": false,
			"authority": _demo_mode and DEMO_PLAYERS.get(name, "") == authority_id,
			"route": str(peer.route),
			"endpoint": "%s:%d" % [str(peer.ip), int(peer.port)],
			"rtt_ms": int(peer.rtt_ms),
			"last_seen_ms": now - int(peer.last_seen_ms) if int(peer.last_seen_ms) > 0 else -1,
		})
	return result
