extends Node

@export var peer_session: Node
@export var local_horse: CharacterBody3D
@export var remote_rider: Node3D

var destination_ip := ""
var destination_port := 0
var local_is_authority := false
var simulation_tick := 0

func _ready() -> void:
	if peer_session:
		peer_session.packet_received.connect(_on_packet_received)

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
	if local_is_authority and simulation_tick % 3 == 0:
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
		remote_rider.push_snapshot(
			int(payload.get("tick", 0)),
			payload.get("position", Vector3.ZERO),
			payload.get("velocity", Vector3.ZERO),
			float(payload.get("yaw_degrees", 0.0))
		)
