extends Node

const REQUIRED_ACTIONS := [
	&"move_forward", &"move_back", &"steer_left", &"steer_right",
	&"gait_up", &"gait_down", &"hard_brake", &"jump", &"reset_horse"
]
const REQUIRED_NODES := [
	"Horse", "Horse/CollisionShape3D", "CameraRig/PitchPivot/SpringArm3D/Camera3D",
	"TestCourse", "TestCourse/BroadGround", "TestCourse/FlatStraight",
	"TestCourse/RoughStrip", "TestCourse/Ramp15", "TestCourse/Ramp25",
	"TestCourse/Face45", "TestCourse/JumpFence_0Rail", "TestCourse/BridgeDeck",
	"TestCourse/SlalomPost_0", "TestCourse/TurnCircle_0", "WorldEnvironment",
	"Sun", "KillResetZone", "HorseSpawn", "Horse/HeadProxy", "Horse/FrontLeftLeg",
	"FrontierPropsWest", "FrontierPropsEast", "FeedbackLayer/StylizedFeedback",
	"ArchetypeLayer/ArchetypeSelector", "HUD", "PeerSession", "RemoteRider", "NetworkReplication",
	"NetworkLayer/Panel/Margin/Label"
]

func _ready() -> void:
	var failures: Array[String] = []
	for action in REQUIRED_ACTIONS:
		if not InputMap.has_action(action):
			failures.append("missing InputMap action: %s" % action)
	if not ClassDB.class_exists(&"NetworkRider"):
		failures.append("native class NetworkRider is unavailable")
	else:
		var network_rider := ClassDB.instantiate(&"NetworkRider") as Node3D
		if network_rider == null:
			failures.append("NetworkRider could not be instantiated")
		else:
			if not bool(network_rider.call("push_snapshot", 10, Vector3.ZERO, Vector3(6, 0, 0), 350.0)):
				failures.append("NetworkRider rejected first valid snapshot")
			if not bool(network_rider.call("push_snapshot", 14, Vector3(4, 0, 0), Vector3(6, 0, 0), 10.0)):
				failures.append("NetworkRider rejected second valid snapshot")
			var sample := network_rider.call("sample_at", 12.0) as Dictionary
			if absf((sample.get("position", Vector3.ZERO) as Vector3).x - 2.0) > 0.001:
				failures.append("NetworkRider interpolation did not bridge jittered snapshots")
			var correction := network_rider.call("reconciliation", 14, Vector3.ZERO, Vector3(3, 0, 0)) as Dictionary
			if not bool(correction.get("snap", false)):
				failures.append("NetworkRider failed large prediction reconciliation")
			network_rider.free()
	if not ClassDB.class_exists(&"PeerSession"):
		failures.append("native class PeerSession is unavailable")
	else:
		var peer_session := ClassDB.instantiate(&"PeerSession") as Node
		if peer_session == null:
			failures.append("PeerSession could not be instantiated")
		else:
			for method in ["configure_session", "make_heartbeat", "make_rider_input", "make_rider_snapshot", "decode_packet", "accept_packet", "connect_rustscale", "send_packet", "shutdown"]:
				if not peer_session.has_method(method):
					failures.append("PeerSession lacks %s" % method)
			if not peer_session.has_signal("packet_received"):
				failures.append("PeerSession lacks packet_received signal")
			var configured := bool(peer_session.call(
				"configure_session",
				"00000000-0000-4000-8000-000000000001",
				"00000000-0000-4000-8000-000000000002",
				"00000000-0000-4000-8000-000000000002",
				0
			))
			if not configured:
				failures.append("PeerSession rejected valid session identifiers")
			else:
				var heartbeat := peer_session.call("make_heartbeat", 1) as PackedByteArray
				if heartbeat.is_empty() or int(peer_session.call("accept_packet", heartbeat, 1)) != 0:
					failures.append("PeerSession heartbeat codec/validation failed")
				elif int(peer_session.call("accept_packet", heartbeat, 2)) != 1:
					failures.append("PeerSession failed to reject a replayed heartbeat")
				var snapshot := peer_session.call("make_rider_snapshot", 2, Vector3(1, 2, 3), Vector3(4, 0, -2), 45.0) as PackedByteArray
				var decoded := peer_session.call("decode_packet", snapshot) as Dictionary
				if decoded.get("type", "") != "rider_snapshot" or decoded.get("position", Vector3.ZERO) != Vector3(1, 2, 3):
					failures.append("PeerSession snapshot codec omitted rider state")
			peer_session.free()
	if not ClassDB.class_exists(&"HorseController"):
		failures.append("native class HorseController is unavailable")
	else:
		var packed := load("res://scenes/graybox_course.tscn") as PackedScene
		if packed == null:
			failures.append("graybox_course.tscn could not be loaded")
		else:
			var course := packed.instantiate()
			add_child(course)
			for path in REQUIRED_NODES:
				if not course.has_node(path):
					failures.append("missing required node: %s" % path)
			var horse := course.get_node_or_null("Horse") as CharacterBody3D
			if horse and not horse.has_method("reset_horse"):
				failures.append("HorseController lacks reset_horse()")
			if horse and not horse.has_signal("gait_changed"):
				failures.append("HorseController lacks gait_changed signal")
			if horse and not horse.has_signal("telemetry_updated"):
				failures.append("HorseController lacks telemetry_updated signal")
			if horse:
				await _exercise_native_input(horse, failures)
	_finish(failures)

func _exercise_native_input(horse: CharacterBody3D, failures: Array[String]) -> void:
	# Let CharacterBody3D establish floor contact, then exercise the real InputMap -> GDExtension
	# boundary. Kernel-only tests cannot catch a reversed Godot yaw convention or an Idle gait that
	# ignores W.
	await _wait_physics_frames(5)
	if not horse.has_method("set_archetype") or not horse.has_method("get_archetype_stats"):
		failures.append("HorseController lacks the M0.5 archetype API")
	else:
		horse.call("set_archetype", 0)
		if int(horse.get("archetype")) != 0:
			failures.append("Courser archetype selection failed")
		var stats := horse.call("get_archetype_stats") as Dictionary
		if float(stats.get("max_vitality", 0.0)) <= 0.0:
			failures.append("archetype stats omitted max_vitality")
		horse.call("set_archetype", 2)

	horse.call("reset_horse")
	# Spawn-floor contact intentionally applies a short landing recovery before lateral steps.
	await _wait_physics_frames(20)
	var sidestep_start := horse.global_position
	Input.action_press(&"steer_left")
	await _wait_physics_frames(30)
	Input.action_release(&"steer_left")
	if horse.global_position.x >= sidestep_start.x - 0.1:
		failures.append("A from rest did not sidestep toward negative X")
	if absf(horse.rotation.y) > 0.02:
		failures.append("stationary sidestep incorrectly changed yaw")

	horse.call("reset_horse")
	await _wait_physics_frames(20)
	sidestep_start = horse.global_position
	Input.action_press(&"steer_right")
	await _wait_physics_frames(30)
	Input.action_release(&"steer_right")
	if horse.global_position.x <= sidestep_start.x + 0.1:
		failures.append("D from rest did not sidestep toward positive X")
	if absf(horse.rotation.y) > 0.02:
		failures.append("stationary sidestep incorrectly changed yaw")

	horse.call("reset_horse")
	await _wait_physics_frames(2)
	var start := horse.global_position
	Input.action_press(&"move_forward")
	await _wait_physics_frames(30)
	Input.action_release(&"move_forward")
	if horse.global_position.z >= start.z - 0.1:
		failures.append("W did not move HorseController forward along local -Z")
	if int(horse.get("current_gait")) < 1:
		failures.append("W from Idle did not automatically enter Walk")

	var before_right := horse.global_position
	Input.action_press(&"move_forward")
	Input.action_press(&"steer_right")
	await _wait_physics_frames(60)
	Input.action_release(&"steer_right")
	Input.action_release(&"move_forward")
	if horse.global_position.x <= before_right.x + 0.05:
		failures.append("D did not turn/move HorseController toward positive X")
	if horse.rotation.y >= 0.0:
		failures.append("D did not use Godot's negative-yaw right-turn convention")
	horse.call("reset_horse")
	await _exercise_native_combat(horse, failures)

func _exercise_native_combat(horse: CharacterBody3D, failures: Array[String]) -> void:
	var course := horse.get_parent()
	var controller := horse.get_node_or_null("WeaponController")
	var target := course.get_node_or_null("TargetBodyNear")
	if controller == null or target == null:
		failures.append("integrated weapon controller or target dummy is missing")
		return
	for method in ["equip_weapon", "request_fire", "request_reload", "get_weapon_stats", "resolve_local_hit"]:
		if not controller.has_method(method):
			failures.append("MountedWeaponController lacks %s" % method)
	if not failures.is_empty():
		return
	controller.call("equip_weapon", 0)
	var origin: Vector3 = controller.global_position
	var body_zone := target.get_node("BodyZone") as StaticBody3D
	var direction := origin.direction_to(body_zone.global_position)
	var tick := int(controller.get("current_tick"))
	var vitality_before := float(target.get("vitality"))
	if not bool(controller.call("request_fire", origin, direction, tick)):
		failures.append("native mounted rifle rejected a valid first shot")
		return
	var distance := origin.distance_to(body_zone.global_position)
	if not bool(controller.call("resolve_local_hit", 101, "body", distance)):
		failures.append("native local authority rejected valid target evidence")
		return
	await get_tree().process_frame
	if float(target.get("vitality")) >= vitality_before:
		failures.append("authority-computed rifle damage did not reach target dummy")

func _wait_physics_frames(count: int) -> void:
	for _frame in range(count):
		await get_tree().physics_frame

func _finish(failures: Array[String]) -> void:
	Input.action_release(&"move_forward")
	Input.action_release(&"steer_left")
	Input.action_release(&"steer_right")
	if failures.is_empty():
		print("SPURFIRE_GODOT_SMOKE_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error("SMOKE: " + failure)
		get_tree().quit(1)
