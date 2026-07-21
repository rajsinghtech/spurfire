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
var _demo_input_count := 0
var _demo_input_counts: Dictionary = {}
var _demo_max_gap_msec := 0
var _demo_peak_snapshot_gap_msec := 0
var _demo_last_snapshot_msec := 0
var _demo_latest_snapshot_position := Vector3.ZERO
var _demo_has_snapshot_position := false
var _demo_probe_index := 0
var _last_route_query_msec := 0
var _demo_qualify := false
var _demo_qualify_deadline_msec := 0
var _demo_qualification_emitted := false
var _demo_soak_msec := 0
var _demo_soak_started_msec := 0
var _demo_soak_snapshot_baseline := 0
var _demo_soak_input_baseline := 0
var _demo_soak_input_baselines: Dictionary = {}
var _demo_soak_first_position := Vector3.ZERO
var _demo_soak_motion_span_mm := 0
var _demo_presentation_samples := 0
var _demo_peak_presentation_desync_msec := 0
var _demo_soak_emitted := false
var _demo_quit_requested := false
var _demo_nodes: Array[String] = []
var _peers: Dictionary = {}
var _remote_riders: Dictionary = {}
var _seen_snapshot_senders: Dictionary = {}
var _seen_input_senders: Dictionary = {}

const DEMO_LOBBY := "00000000-0000-4000-8000-000000000001"
const DEMO_PLAYERS := {
	"a": "00000000-0000-4000-8000-000000000002",
	"b": "00000000-0000-4000-8000-000000000003",
	"c": "00000000-0000-4000-8000-000000000004",
	"d": "00000000-0000-4000-8000-000000000005",
	"e": "00000000-0000-4000-8000-000000000006",
	"f": "00000000-0000-4000-8000-000000000007",
	"g": "00000000-0000-4000-8000-000000000008",
	"h": "00000000-0000-4000-8000-000000000009",
}
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
	if _demo_qualify:
		_sample_demo_presentation_desync()
		_try_finish_demo_qualification(now)

func _start_demo() -> void:
	_demo_node = OS.get_environment("SPURFIRE_P2P_DEMO_NODE").to_lower()
	_demo_dir = OS.get_environment("SPURFIRE_P2P_DEMO_DIR")
	var enrollment_file := OS.get_environment("SPURFIRE_P2P_DEMO_KEY_FILE")
	var requested_nodes := OS.get_environment("SPURFIRE_P2P_DEMO_NODES").strip_edges().to_lower()
	if requested_nodes.is_empty():
		requested_nodes = "a,b,c"
	for requested_node in requested_nodes.split(",", false):
		var clean_node := str(requested_node).strip_edges()
		if not clean_node.is_empty() and clean_node not in _demo_nodes:
			_demo_nodes.append(clean_node)
	_demo_qualify = OS.get_environment("SPURFIRE_P2P_DEMO_QUALIFY") == "1"
	_demo_soak_msec = maxi(0, int(OS.get_environment("SPURFIRE_P2P_DEMO_SOAK_MS")))
	var timeout_msec := maxi(30000, int(OS.get_environment("SPURFIRE_P2P_DEMO_TIMEOUT_MS")))
	_demo_qualify_deadline_msec = Time.get_ticks_msec() + timeout_msec
	if (
		_demo_nodes.size() < 2
		or _demo_nodes.size() > DEMO_PLAYERS.size()
		or _demo_node not in _demo_nodes
		or not DEMO_PLAYERS.has(_demo_node)
		or _demo_dir.is_empty()
		or not FileAccess.file_exists(enrollment_file)
	):
		push_error("P2P demo environment is incomplete")
		if _demo_qualify:
			get_tree().quit.call_deferred(1)
		return
	for node_name in _demo_nodes:
		if not DEMO_PLAYERS.has(node_name):
			push_error("P2P demo node is unsupported: %s" % node_name)
			if _demo_qualify:
				get_tree().quit.call_deferred(1)
			return
	var roster_player_ids := PackedStringArray()
	for node_name in _demo_nodes:
		roster_player_ids.append(str(DEMO_PLAYERS[node_name]))
	if not peer_session.configure_roster_session(
		DEMO_LOBBY,
		DEMO_PLAYERS[_demo_node],
		DEMO_PLAYERS["a"],
		roster_player_ids,
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
	local_horse.global_position.x = float(_demo_nodes.find(_demo_node)) * 4.0
	DisplayServer.window_set_title("Spurfire P2P — Rider %s" % _demo_node.to_upper())

func _discover_demo_peers() -> void:
	for node_name in _demo_nodes:
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
			"rtt_samples": [],
			"rtt_logged": false,
			"last_seen_ms": 0,
		}
	if _peers.size() == _demo_nodes.size() - 1 and not _demo_ready_logged:
		_demo_ready_logged = true
		print("SPURFIRE_GODOT_P2P_READY local=%s peers=%d" % [_demo_node, _peers.size()])

func _try_finish_demo_qualification(now: int) -> void:
	if _demo_quit_requested:
		return
	if now >= _demo_qualify_deadline_msec:
		push_error("SPURFIRE_GODOT_P2P_QUALIFY_FAILED local=%s reason=timeout" % _demo_node)
		_demo_quit_requested = true
		get_tree().quit.call_deferred(1)
		return
	if not _demo_qualification_emitted:
		if _peers.size() != _demo_nodes.size() - 1:
			return
		if local_is_authority:
			if _seen_input_senders.size() != _demo_nodes.size() - 1:
				return
		elif not _seen_snapshot_senders.has(str(DEMO_PLAYERS["a"])):
			return
		for peer: Dictionary in _peers.values():
			var rtt_samples := peer.get("rtt_samples", []) as Array
			if (
				str(peer.route).to_upper() == "UNKNOWN"
				or int(peer.rtt_ms) < 0
				or int(peer.last_seen_ms) <= 0
				or rtt_samples.size() < 5
			):
				return
		var network_layer := get_parent().get_node_or_null("NetworkLayer")
		if network_layer == null or not network_layer.has_method("qualification_peer_rows"):
			return
		var hud_rows := {}
		for row: Dictionary in network_layer.call("qualification_peer_rows"):
			hud_rows[str(row.get("name", ""))] = row
		if hud_rows.size() != _peers.size():
			return
		var names := _peers.keys()
		names.sort()
		for peer_name: String in names:
			var measured: Dictionary = _peers[peer_name]
			var hud := hud_rows.get(peer_name, {}) as Dictionary
			var measured_samples := measured.get("rtt_samples", []) as Array
			print("SPURFIRE_GODOT_P2P_RTT_READY local=%s peer=%s samples=%d" % [
				_demo_node, peer_name, measured_samples.size(),
			])
			print("SPURFIRE_GODOT_P2P_MEASURED local=%s peer=%s route=%s rtt_ms=%d" % [
				_demo_node, peer_name, str(measured.route).to_upper(), int(measured.rtt_ms),
			])
			print("SPURFIRE_GODOT_P2P_HUD local=%s peer=%s route=%s rtt_ms=%d" % [
				_demo_node, peer_name, str(hud.get("route", "UNKNOWN")), int(hud.get("rtt_ms", -1)),
			])
		var marker := FileAccess.open(_demo_dir.path_join("qualified-%s" % _demo_node), FileAccess.WRITE)
		if marker == null:
			push_error("SPURFIRE_GODOT_P2P_QUALIFY_FAILED local=%s reason=barrier_write" % _demo_node)
			_demo_quit_requested = true
			get_tree().quit.call_deferred(1)
			return
		marker.store_string("ready\n")
		_demo_qualification_emitted = true
		if _demo_soak_msec > 0:
			_demo_soak_started_msec = now
			_demo_soak_snapshot_baseline = _demo_snapshot_count
			_demo_soak_input_baseline = _demo_input_count
			_demo_soak_input_baselines = _demo_input_counts.duplicate(true)
			_demo_peak_snapshot_gap_msec = 0
			_demo_last_snapshot_msec = now
			_demo_soak_motion_span_mm = 0
			_demo_presentation_samples = 0
			_demo_peak_presentation_desync_msec = 0
			if _demo_has_snapshot_position:
				_demo_soak_first_position = _demo_latest_snapshot_position
	if _demo_soak_msec > 0:
		if now - _demo_soak_started_msec < _demo_soak_msec:
			return
		if not _demo_soak_emitted:
			var soak_snapshots := _demo_snapshot_count - _demo_soak_snapshot_baseline
			var soak_inputs := _demo_input_count - _demo_soak_input_baseline
			var minimum_snapshots := floori(float(_demo_soak_msec * 16) / 1000.0)
			var minimum_inputs_per_sender := floori(float(_demo_soak_msec * 10) / 1000.0)
			var minimum_presentation_samples := floori(float(_demo_soak_msec * 30) / 1000.0)
			var minimum_sender_inputs := soak_inputs
			if local_is_authority:
				for sender_id: String in _seen_input_senders:
					minimum_sender_inputs = mini(
						minimum_sender_inputs,
						int(_demo_input_counts.get(sender_id, 0))
							- int(_demo_soak_input_baselines.get(sender_id, 0))
					)
			var role := "authority" if local_is_authority else "follower"
			var snapshot_age_msec := 0 if local_is_authority else now - _demo_last_snapshot_msec
			var failure := ""
			if local_is_authority and minimum_sender_inputs < minimum_inputs_per_sender:
				failure = "input_starvation"
			elif not local_is_authority and soak_snapshots < minimum_snapshots:
				failure = "snapshot_starvation"
			elif not local_is_authority and _demo_peak_snapshot_gap_msec > 200:
				failure = "snapshot_gap"
			elif not local_is_authority and snapshot_age_msec > 200:
				failure = "snapshot_stale"
			elif not local_is_authority and _demo_soak_motion_span_mm < 30000:
				failure = "motion_span"
			elif not local_is_authority and _demo_presentation_samples < minimum_presentation_samples:
				failure = "presentation_samples"
			elif not local_is_authority and _demo_peak_presentation_desync_msec > 200:
				failure = "presentation_desync"
			if not failure.is_empty():
				push_error(
					"SPURFIRE_GODOT_P2P_QUALIFY_FAILED local=%s reason=%s snapshots=%d inputs=%d min_sender_inputs=%d peak_gap_ms=%d motion_span_mm=%d presentation_samples=%d presentation_desync_ms=%d"
					% [
						_demo_node, failure, soak_snapshots, soak_inputs, minimum_sender_inputs,
						_demo_peak_snapshot_gap_msec, _demo_soak_motion_span_mm,
						_demo_presentation_samples, _demo_peak_presentation_desync_msec,
					]
				)
				_demo_quit_requested = true
				get_tree().quit.call_deferred(1)
				return
			print(
				"SPURFIRE_GODOT_P2P_SOAK local=%s role=%s duration_ms=%d snapshots=%d inputs=%d min_sender_inputs=%d peak_gap_ms=%d motion_span_mm=%d last_age_ms=%d presentation_samples=%d presentation_desync_ms=%d rejects=%d"
				% [
					_demo_node, role, now - _demo_soak_started_msec, soak_snapshots,
					soak_inputs, minimum_sender_inputs, _demo_peak_snapshot_gap_msec, _demo_soak_motion_span_mm,
					snapshot_age_msec, _demo_presentation_samples,
					_demo_peak_presentation_desync_msec, _demo_rejected_count,
				]
			)
			var soak_marker := FileAccess.open(_demo_dir.path_join("soaked-%s" % _demo_node), FileAccess.WRITE)
			if soak_marker == null:
				push_error("SPURFIRE_GODOT_P2P_QUALIFY_FAILED local=%s reason=soak_barrier_write" % _demo_node)
				_demo_quit_requested = true
				get_tree().quit.call_deferred(1)
				return
			soak_marker.store_string("ready\n")
			_demo_soak_emitted = true
		for node_name in _demo_nodes:
			if not FileAccess.file_exists(_demo_dir.path_join("soaked-%s" % node_name)):
				return
	for node_name in _demo_nodes:
		if not FileAccess.file_exists(_demo_dir.path_join("qualified-%s" % node_name)):
			return
	print("SPURFIRE_GODOT_P2P_QUALIFIED local=%s peers=%d snapshots=%d" % [
		_demo_node, _peers.size(), _demo_snapshot_count,
	])
	_demo_quit_requested = true
	get_tree().quit.call_deferred(0)

func _sample_demo_presentation_desync() -> void:
	if _demo_soak_started_msec <= 0 or local_is_authority:
		return
	var authority_rider := _remote_riders.get(str(DEMO_PLAYERS["a"])) as Node3D
	if authority_rider == null or not authority_rider.visible:
		return
	var render_tick := float(authority_rider.get("render_tick"))
	if render_tick <= 0.0:
		return
	var angle := fposmod(render_tick, 720.0) * TAU / 720.0
	var expected := Vector2(cos(angle) * 20.0, sin(angle) * 20.0)
	var presented := Vector2(authority_rider.global_position.x, authority_rider.global_position.z)
	var desync_msec := roundi(expected.distance_to(presented) * 1000.0 / 10.472)
	_demo_presentation_samples += 1
	_demo_peak_presentation_desync_msec = maxi(
		_demo_peak_presentation_desync_msec,
		desync_msec
	)

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
		"rtt_samples": [],
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
	if local_is_authority and (simulation_tick % 3 == 0 or stance_changed):
		var snapshot_position := local_rider.global_position
		var snapshot_velocity := local_rider.velocity
		if _demo_soak_msec > 0:
			# A deterministic 20 m circle makes the transport carry changing actor
			# state without pretending this practice harness is a horse-physics test.
			var angle := float(simulation_tick % 720) * TAU / 720.0
			snapshot_position = Vector3(cos(angle) * 20.0, snapshot_position.y, sin(angle) * 20.0)
			snapshot_velocity = Vector3(-sin(angle) * 10.472, 0.0, cos(angle) * 10.472)
		packet = peer_session.make_rider_snapshot(
			simulation_tick,
			str(peer_session.get("local_player_id")),
			snapshot_position,
			snapshot_velocity,
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
		if _demo_mode and not local_is_authority:
			var authority_peer := _peers.get("a", {}) as Dictionary
			if not authority_peer.is_empty():
				peer_session.send_packet(packet, str(authority_peer.ip), int(authority_peer.port))
		else:
			_send_to_all(packet)
		if _demo_mode:
			_demo_sent_count += 1

	if _demo_mode and not _peers.is_empty():
		var probe_interval_ticks := maxi(1, floori(60.0 / float(_peers.size())))
		var probe_phase := _demo_nodes.find(_demo_node) % probe_interval_ticks
		if simulation_tick % probe_interval_ticks == probe_phase:
			var probe_names := _peers.keys()
			probe_names.sort()
			var peer: Dictionary = _peers[probe_names[_demo_probe_index % probe_names.size()]]
			_demo_probe_index += 1
			var probe: PackedByteArray = peer_session.make_probe(simulation_tick, Time.get_ticks_msec(), false)
			peer_session.send_packet(probe, str(peer.ip), int(peer.port))
	elif simulation_tick % 60 == 0:
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
					var sample_msec := maxi(0, now - int(payload.get("nonce", now)))
					var samples := measured_peer.get("rtt_samples", []) as Array
					samples.append(sample_msec)
					while samples.size() > 9:
						samples.pop_front()
					measured_peer.rtt_samples = samples
					var ordered_samples := samples.duplicate()
					ordered_samples.sort()
					measured_peer.rtt_ms = int(
						ordered_samples[floori(float(ordered_samples.size()) / 2.0)]
					)
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
		"rider_input":
			if _demo_mode and local_is_authority:
				var input_sender := str(payload.get("sender", ""))
				_demo_input_count += 1
				_demo_input_counts[input_sender] = int(_demo_input_counts.get(input_sender, 0)) + 1
				if not _seen_input_senders.has(input_sender):
					_seen_input_senders[input_sender] = true
					print("SPURFIRE_GODOT_P2P_INPUT local=%s sender=%s" % [_demo_node, input_sender])

func _apply_remote_snapshot(payload: Dictionary, now: int) -> void:
	var sender := str(payload.get("rider_player_id", payload.get("sender", "")))
	var snapshot_position: Vector3 = payload.get("position", Vector3.ZERO)
	var rider := _remote_rider_for(sender)
	if rider == null:
		return
	rider.visible = true
	if _demo_mode:
		if _demo_last_snapshot_msec > 0:
			var snapshot_gap := now - _demo_last_snapshot_msec
			_demo_max_gap_msec = maxi(_demo_max_gap_msec, snapshot_gap)
			_demo_peak_snapshot_gap_msec = maxi(_demo_peak_snapshot_gap_msec, snapshot_gap)
		_demo_last_snapshot_msec = now
		_demo_latest_snapshot_position = snapshot_position
		_demo_has_snapshot_position = true
		if _demo_soak_started_msec > 0:
			_demo_soak_motion_span_mm = maxi(
				_demo_soak_motion_span_mm,
				roundi(_demo_soak_first_position.distance_to(snapshot_position) * 1000.0)
			)
		_demo_snapshot_count += 1
		if not _seen_snapshot_senders.has(sender):
			_seen_snapshot_senders[sender] = true
			print("SPURFIRE_GODOT_P2P_SNAPSHOT local=%s sender=%s" % [_demo_node, sender])
		elif _demo_snapshot_count % 120 == 0:
			print("SPURFIRE_GODOT_P2P_HEALTH local=%s snapshots=%d max_gap_ms=%d" % [_demo_node, _demo_snapshot_count, _demo_max_gap_msec])
			_demo_max_gap_msec = 0
	rider.push_snapshot(
		int(payload.get("tick", 0)),
		snapshot_position,
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
