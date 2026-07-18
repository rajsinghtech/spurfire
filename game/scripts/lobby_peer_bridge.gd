class_name SpurfireLobbyPeerBridge
extends Node

signal authority_departed

var peer_session: Node
var local_rider: CharacterBody3D
var remote_rider_template: Node3D
var local_player_id := ""
var local_is_authority := false
var simulation_tick := 0

var _peers: Dictionary = {}
var _remote_riders: Dictionary = {}
var _last_route_query_ms := 0
var _session_binding_key := ""
var _authority_player_id := ""

func configure(nodes: Dictionary, player_id: String) -> bool:
	peer_session = nodes.get("peer_session") as Node
	local_rider = nodes.get("local_rider") as CharacterBody3D
	remote_rider_template = nodes.get("remote_rider") as Node3D
	local_player_id = player_id
	if peer_session == null or local_rider == null or remote_rider_template == null:
		return false
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
	var binding_key := "%s|%s|%s" % [lobby_id, authority_id, ",".join(sorted_roster)]
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
	_peers = next_peers
	return true

func _process(_delta: float) -> void:
	if peer_session == null or _peers.is_empty():
		return
	var now := Time.get_ticks_msec()
	if now - _last_route_query_ms < 1000:
		return
	_last_route_query_ms = now
	for peer: Dictionary in _peers.values():
		peer_session.query_route(str(peer.address))

func advance_shared_tick(tick: int, stance_changed: bool = false) -> void:
	if tick <= simulation_tick or peer_session == null or local_rider == null:
		return
	simulation_tick = tick
	var packet := PackedByteArray()
	if local_is_authority and (tick % 2 == 0 or stance_changed):
		packet = peer_session.make_rider_snapshot(
			tick, local_rider.global_position, local_rider.velocity,
			rad_to_deg(local_rider.rotation.y), int(local_rider.get("stance_id"))
		)
	elif not local_is_authority:
		var throttle := roundi(Input.get_axis(&"move_back", &"move_forward") * 1000.0)
		var steer := roundi(Input.get_axis(&"steer_left", &"steer_right") * 1000.0)
		var buttons := 0
		if Input.is_action_just_pressed(&"jump"):
			buttons |= 1
		if Input.is_action_just_pressed(&"combat_interact"):
			buttons |= 2
		packet = peer_session.make_rider_input(tick, throttle, steer, buttons)
	elif tick % 6 == 0:
		packet = peer_session.make_heartbeat(tick)
	if not packet.is_empty():
		_send_to_all(packet)
	if tick % 60 == 0:
		for peer: Dictionary in _peers.values():
			var probe: PackedByteArray = peer_session.make_probe(tick, Time.get_ticks_msec(), false)
			peer_session.send_packet(probe, str(peer.address), int(peer.port))

func send_leave() -> void:
	if peer_session == null:
		return
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

func get_peer_status() -> Dictionary:
	return peer_health()

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

func _on_packet_received(packet: PackedByteArray, source_ip: String, source_port: int) -> void:
	# Decode only enough to select the exact control-plane endpoint. Do not let a
	# packet mutate replay/session state until sender and source both match.
	var payload := peer_session.decode_packet(packet) as Dictionary
	var sender := str(payload.get("sender", ""))
	if not _peers.has(sender):
		return
	var peer := _peers[sender] as Dictionary
	if str(peer.address) != source_ip or int(peer.port) != source_port:
		return
	var outcome := int(peer_session.accept_packet(packet, Time.get_ticks_msec()))
	if outcome != 0:
		return
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
		"rider_snapshot":
			var rider := _remote_rider_for(sender)
			if rider:
				rider.push_snapshot(
					int(payload.get("tick", 0)), payload.get("position", Vector3.ZERO),
					payload.get("velocity", Vector3.ZERO), float(payload.get("yaw_degrees", 0.0)),
					int(payload.get("stance_id", 1))
				)
		"leave":
			_peers.erase(sender)
			if sender == _authority_player_id:
				authority_departed.emit()

func _on_route_updated(peer_ip: String, route: String) -> void:
	for peer: Dictionary in _peers.values():
		if str(peer.address) == peer_ip:
			peer.route = route
			return

func _remote_rider_for(player_id: String) -> Node3D:
	if _remote_riders.has(player_id):
		return _remote_riders[player_id] as Node3D
	var rider: Node3D
	if _remote_riders.is_empty():
		rider = remote_rider_template
	else:
		rider = remote_rider_template.duplicate() as Node3D
		remote_rider_template.get_parent().add_child(rider)
	if rider:
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
