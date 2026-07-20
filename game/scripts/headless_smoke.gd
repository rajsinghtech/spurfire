extends Node

const CAMERA_RIG_SCRIPT := preload("res://scripts/camera_rig.gd")
const RIDER_POSE_SCRIPT := preload("res://scripts/rider_pose.gd")

const REQUIRED_ACTIONS := [
	&"move_forward", &"move_back", &"steer_left", &"steer_right",
	&"gait_up", &"gait_down", &"hard_brake", &"jump", &"reset_horse", &"scoreboard",
	&"combat_fire", &"combat_reload", &"combat_interact", &"toggle_diagnostics"
]
const REQUIRED_NODES := [
	"Horse", "Horse/CollisionShape3D", "Rider", "Rider/CollisionShape3D",
	"Rider/RiderProxy", "Rider/WeaponController", "Rider/RiderProxy/MountedRifle",
	"Rider/CombatInput", "M2Gameplay", "CameraRig/PitchPivot/SpringArm3D/Camera3D",
	"TestCourse", "TestCourse/BroadGround", "TestCourse/FlatStraight",
	"TestCourse/RoughStrip", "TestCourse/Ramp15", "TestCourse/Ramp25",
	"TestCourse/Face45", "TestCourse/Landing30", "TestCourse/Landing31",
	"TestCourse/JumpFence_0Rail", "TestCourse/BridgeDeck",
	"TestCourse/SlalomPost_0", "TestCourse/TurnCircle_0", "WorldEnvironment",
	"Sun", "KillResetZone", "HorseSpawn", "Horse/HeadProxy", "Horse/FrontLeftLeg",
	"FrontierPropsWest", "FrontierPropsEast", "FeedbackLayer/StylizedFeedback",
	"ArchetypeLayer/ArchetypeSelector", "HUD", "PeerSession", "RemoteRider", "NetworkReplication",
	"GameplayEventLayer/GameplayToast/Notification", "HUD/Panel/Margin/VBox/RemountHint",
	"NetworkLayer/Panel/Margin/Label", "NetworkLayer/RosterPanel/Margin/VBox/Rows",
	"CaptureLayer/CaptureGate", "DustFx/HoofDust", "DustFx/LandingDust"
]
const STANCE_MOUNTED := 1
const STANCE_MOUNTED_AIRBORNE := 2
const STANCE_DIVE := 3
const STANCE_PRONE := 4
const STANCE_RECOVERY := 5
const STANCE_ON_FOOT := 6
const HUD_SETTLE_PROCESS_FRAMES := 3

func _ready() -> void:
	var failures: Array[String] = []
	_check_input_map(failures)
	_check_render_interpolation(failures)
	_check_airborne_yaw_limiter(failures)
	_check_network_rider(failures)
	_check_peer_session(failures)
	if not ClassDB.class_exists(&"HorseController"):
		failures.append("native class HorseController is unavailable")
		_finish(failures)
		return
	if not ClassDB.class_exists(&"SaddleDiveController"):
		failures.append("native class SaddleDiveController is unavailable")
		_finish(failures)
		return

	var packed := load("res://scenes/graybox_course.tscn") as PackedScene
	if packed == null:
		failures.append("graybox_course.tscn could not be loaded")
		_finish(failures)
		return
	var course := packed.instantiate()
	add_child(course)
	for path in REQUIRED_NODES:
		if not course.has_node(path):
			failures.append("missing required node: %s" % path)
	await _check_capture_contract(course, failures)
	await _check_frontier_arena_contract(course, failures)
	# The remaining deterministic simulation scenarios drive InputMap directly;
	# explicitly open the presentation gate after its dedicated assertions.
	course.get_node("Horse").call("set_presentation_input_enabled", true, false)
	course.get_node("M2Gameplay").call("set_presentation_input_enabled", true, false)
	course.get_node("Rider/CombatInput").call("set_presentation_input_enabled", true, false)
	Input.action_press(&"scoreboard")
	await get_tree().process_frame
	await get_tree().process_frame
	var roster_panel := course.get_node_or_null("NetworkLayer/RosterPanel") as Control
	if roster_panel == null or not roster_panel.visible:
		failures.append("TAB did not reveal the peer route/RTT roster")
	Input.action_release(&"scoreboard")

	var horse := course.get_node_or_null("Horse") as CharacterBody3D
	var rider := course.get_node_or_null("Rider") as CharacterBody3D
	if horse == null or rider == null:
		failures.append("integrated horse/rider bodies are missing")
		_finish(failures)
		return
	var course_peer := course.get_node("PeerSession")
	var bound_player := "00000000-0000-4000-8000-000000000042"
	if not bool(course_peer.call(
		"configure_session",
		"00000000-0000-4000-8000-000000000001",
		bound_player,
		bound_player,
		0
	)):
		failures.append("scene PeerSession could not bind gameplay identity")
	elif str(rider.get("actor_id")) != bound_player or int(rider.get("authority_epoch")) != 1:
		failures.append("network session identity did not reach SaddleDiveController")
	elif str(course.get_node("Rider/WeaponController").get("shooter_peer_id")) != bound_player:
		failures.append("network session identity did not reach combat authority")
	_check_native_apis(horse, rider, failures)
	await _exercise_reload_after_remount(course, horse, rider, failures)
	await _exercise_native_input(course, horse, failures)
	await _exercise_m2(course, horse, rider, failures)
	await _exercise_landing_boundaries(course, horse, rider, failures)
	await _exercise_bridge_caps(course, horse, rider, failures)
	_check_persisted_telemetry(course, failures)
	_finish(failures)

func _check_input_map(failures: Array[String]) -> void:
	for action in REQUIRED_ACTIONS:
		if not InputMap.has_action(action):
			failures.append("missing InputMap action: %s" % action)
	var tab_event := InputEventKey.new()
	tab_event.physical_keycode = KEY_TAB
	if not InputMap.event_is_action(tab_event, &"scoreboard"):
		failures.append("physical Tab is not mapped to scoreboard")
	var e_event := InputEventKey.new()
	e_event.physical_keycode = KEY_E
	if not InputMap.event_is_action(e_event, &"combat_interact"):
		failures.append("physical E is not mapped to combat_interact")
	var f3_event := InputEventKey.new()
	f3_event.physical_keycode = KEY_F3
	if not InputMap.event_is_action(f3_event, &"toggle_diagnostics"):
		failures.append("physical F3 is not mapped to toggle_diagnostics")

func _check_capture_contract(course: Node, failures: Array[String]) -> void:
	var gate := course.get_node("CaptureLayer/CaptureGate")
	var camera_rig := course.get_node("CameraRig")
	var horse := course.get_node("Horse")
	var gameplay := course.get_node("M2Gameplay")
	var combat := course.get_node("Rider/CombatInput")
	if bool(gate.get("captured")):
		failures.append("capture gate did not begin released")
	if bool(horse.get("presentation_input_enabled")):
		failures.append("horse input did not begin neutralized behind the capture gate")
	gate.call("request_capture")
	if not bool(gate.get("captured")) or int(gate.get("capture_count")) != 1:
		failures.append("capture gate did not capture in one click")
	if not bool(horse.get("presentation_input_enabled")):
		failures.append("capture gate did not enable horse input")
	if not bool(gameplay.get("_capture_button_blocked")) or not bool(combat.get("_capture_button_blocked")):
		failures.append("capture click was not suppressed from movement/combat")
	var yaw_before := float(camera_rig.get("_world_yaw"))
	var motion := InputEventMouseMotion.new()
	motion.relative = Vector2(80.0, -20.0)
	camera_rig.call("_input", motion)
	if is_equal_approx(float(camera_rig.get("_world_yaw")), yaw_before):
		failures.append("captured mouse motion did not reach CameraRig._input")
	var escape := InputEventKey.new()
	escape.physical_keycode = KEY_ESCAPE
	escape.pressed = true
	camera_rig.call("_input", escape)
	if (
		bool(gate.get("captured"))
		or bool(horse.get("presentation_input_enabled"))
		or bool(gameplay.get("_presentation_input_enabled"))
		or bool(combat.get("_presentation_input_enabled"))
	):
		failures.append("Escape did not release capture and neutralize all gameplay input")
	# A released Escape is intentionally a no-op; quitting is gate-button-only.
	camera_rig.call("_input", escape)
	if bool(gate.get("captured")):
		failures.append("released Escape unexpectedly changed capture state")
	gate.call("request_capture")
	camera_rig.call("_notification", NOTIFICATION_APPLICATION_FOCUS_OUT)
	if (
		bool(gate.get("captured"))
		or bool(horse.get("presentation_input_enabled"))
		or bool(gameplay.get("_presentation_input_enabled"))
		or bool(combat.get("_presentation_input_enabled"))
	):
		failures.append("focus loss did not release capture and neutralize all gameplay input")
	var gait_events: Array[int] = []
	var gait_callback := func(_old_gait: int, new_gait: int): gait_events.append(new_gait)
	horse.gait_changed.connect(gait_callback)
	for action in [&"move_forward", &"steer_right", &"gait_up", &"jump", &"reset_horse"]:
		Input.action_press(action)
	for _frame in 3:
		await get_tree().physics_frame
	for action in [&"move_forward", &"steer_right", &"gait_up", &"jump", &"reset_horse"]:
		Input.action_release(action)
	horse.gait_changed.disconnect(gait_callback)
	var horizontal_speed := Vector2(horse.velocity.x, horse.velocity.z).length()
	if not gait_events.is_empty() or horizontal_speed > 0.05 or horse.velocity.y > 0.05:
		failures.append("released horse applied movement, gait, jump, or reset input")
	camera_rig.set("_world_yaw", deg_to_rad(90.0))
	camera_rig.call("_on_telemetry", {"speed_mps": 13.0, "stance_id": STANCE_MOUNTED})
	camera_rig.call("_process", 2.0)
	if not is_equal_approx(float(camera_rig.get("_world_yaw")), deg_to_rad(90.0)):
		failures.append("camera forced recenter after player stopped aiming")

func _check_frontier_arena_contract(graybox: Node, failures: Array[String]) -> void:
	var packed := load("res://scenes/frontier_arena.tscn") as PackedScene
	if packed == null:
		failures.append("frontier_arena.tscn could not be loaded")
		return
	var arena := packed.instantiate()
	add_child(arena)
	await get_tree().process_frame
	for path in REQUIRED_NODES:
		if path != "TestCourse/BroadGround" and not arena.has_node(path):
			failures.append("frontier arena lost inherited node: %s" % path)
	if arena.has_node("TestCourse/BroadGround"):
		failures.append("frontier arena retained overlapping BroadGround")
	for path in ["FrontierGround", "Corral", "MainStreet", "WaterTower", "CactusFlats", "DryWash", "TerracottaMesas"]:
		if not arena.has_node(path):
			failures.append("frontier arena missing landmark: %s" % path)
			continue
		var landmark := arena.get_node(path)
		if landmark.find_children("*", "CollisionShape3D", true, false).is_empty():
			failures.append("frontier arena landmark has no collision: %s" % path)
	for fixture in ["SpeedMarker_-40", "SlalomPost_0", "TurnCircle_0", "SpawnPad", "CourseResetPad"]:
		var fixture_mesh := arena.get_node("TestCourse/" + fixture).get_child(0) as MeshInstance3D
		if fixture_mesh.material_override != null:
			failures.append("frontier restyle erased authored fixture color: %s" % fixture)
	for fixture in ["FlatStraight", "RoughStrip", "Ramp15", "Ramp25", "Landing30", "Landing31", "JumpFence_0Rail", "BridgeDeck"]:
		var expected := graybox.get_node("TestCourse/" + fixture) as Node3D
		var actual := arena.get_node("TestCourse/" + fixture) as Node3D
		if expected.transform != actual.transform:
			failures.append("frontier arena moved smoke fixture: %s" % fixture)
	arena.queue_free()
	await get_tree().process_frame

func _check_render_interpolation(failures: Array[String]) -> void:
	if int(Engine.physics_ticks_per_second) != 60:
		failures.append("authoritative simulation is not fixed at 60 Hz")
	if not bool(ProjectSettings.get_setting("physics/common/physics_interpolation", false)):
		failures.append("local physics interpolation is not enabled")
	var p95_speed_by_rate: Dictionary = {}
	var p95_angular_speed_by_rate: Dictionary = {}
	for render_hz in [60, 120, 144]:
		var samples: Array[Transform3D] = []
		for frame in 10 * render_hz:
			var physics_time := float(frame) * 60.0 / float(render_hz)
			var previous_tick := int(floor(physics_time))
			var fraction := physics_time - float(previous_tick)
			var previous := _synthetic_physics_transform(previous_tick)
			var current := _synthetic_physics_transform(previous_tick + 1)
			samples.append(CAMERA_RIG_SCRIPT.interpolate_render_transform(previous, current, fraction))
		var speeds: Array[float] = []
		var angular_speeds: Array[float] = []
		var repeated_position := 0
		var repeated_yaw := 0
		for index in range(1, samples.size()):
			var distance := samples[index].origin.distance_to(samples[index - 1].origin)
			var yaw_delta := absf(wrapf(
				samples[index].basis.get_euler().y - samples[index - 1].basis.get_euler().y,
				-PI,
				PI
			))
			if distance <= 0.000001:
				repeated_position += 1
			if yaw_delta <= 0.0000001:
				repeated_yaw += 1
			speeds.append(distance * float(render_hz))
			angular_speeds.append(yaw_delta * float(render_hz))
		if render_hz > 60 and (repeated_position > 0 or repeated_yaw > 0):
			failures.append(
				"%d Hz sampling repeated position=%d yaw=%d" % [
					render_hz,
					repeated_position,
					repeated_yaw,
				]
			)
		speeds.sort()
		angular_speeds.sort()
		var p95_index := clampi(int(ceil(float(speeds.size()) * 0.95)) - 1, 0, speeds.size() - 1)
		p95_speed_by_rate[render_hz] = speeds[p95_index]
		p95_angular_speed_by_rate[render_hz] = angular_speeds[p95_index]
		var median := speeds[int(speeds.size() / 2)]
		var angular_median := angular_speeds[int(angular_speeds.size() / 2)]
		if median <= 0.0 or speeds.back() > median * 3.0:
			failures.append("%d Hz linear sampling retained a staircase spike" % render_hz)
		if angular_median <= 0.0 or angular_speeds.back() > angular_median * 3.0:
			failures.append("%d Hz angular sampling retained a staircase spike" % render_hz)
	if float(p95_speed_by_rate[144]) > float(p95_speed_by_rate[60]) * 1.01:
		failures.append("144 Hz interpolated p95 linear motion exceeded the 60 Hz bound")
	if float(p95_angular_speed_by_rate[144]) > float(p95_angular_speed_by_rate[60]) * 1.02:
		failures.append("144 Hz interpolated p95 angular motion exceeded the 60 Hz bound")

func _check_airborne_yaw_limiter(failures: Array[String]) -> void:
	var degrees_per_second := 360.0
	var tick_budget := deg_to_rad(degrees_per_second) / 60.0
	var old_stalled_step := deg_to_rad(degrees_per_second) * (1.0 / 60.0 + 0.003)
	if old_stalled_step <= deg_to_rad(6.05):
		failures.append("airborne yaw regression cadence no longer reproduces the old spike")
	# Render frames below the physics rate must receive the allowance for every elapsed
	# physics tick. The previous implementation granted only 6 degrees per render frame,
	# reducing a nominal 360 degrees/second turn to 120 or 180 degrees/second.
	for render_hz in [20, 30]:
		var yaw := 0.0
		var target := deg_to_rad(179.0)
		var elapsed_ticks := int(60.0 / float(render_hz))
		var frame_count := int(render_hz / 5)
		var previous_tick := 0
		var current_tick := elapsed_ticks
		var remaining_budget := 0.0
		for frame in frame_count:
			remaining_budget = RIDER_POSE_SCRIPT.yaw_budget_after_tick(
				previous_tick,
				current_tick,
				tick_budget,
				remaining_budget
			)
			var change: float = RIDER_POSE_SCRIPT.limited_yaw_change(
				yaw,
				target,
				1.0 / float(render_hz),
				degrees_per_second,
				remaining_budget
			)
			remaining_budget -= absf(change)
			previous_tick = current_tick
			current_tick += elapsed_ticks
			yaw = wrapf(yaw + change, -PI, PI)
		var expected := deg_to_rad(degrees_per_second * float(frame_count) / float(render_hz))
		if absf(yaw - expected) > deg_to_rad(0.001):
			failures.append(
				"%d Hz airborne yaw was %.2f degrees, expected %.2f after elapsed physics ticks" % [
					render_hz,
					rad_to_deg(yaw),
					rad_to_deg(expected),
				]
			)
	var scenarios := [
		{"label": "right_clamped_1", "start": 0.0, "target": 75.0, "sign": 1.0},
		{"label": "left_clamped_1", "start": 0.0, "target": -75.0, "sign": -1.0},
		{"label": "right_clamped_2", "start": 0.0, "target": 75.0, "sign": 1.0},
		{"label": "left_clamped_2", "start": 0.0, "target": -75.0, "sign": -1.0},
		{"label": "right_return", "start": 75.0, "target": 0.0, "sign": -1.0},
		{"label": "left_return", "start": -75.0, "target": 0.0, "sign": 1.0},
		{"label": "positive_seam", "start": 179.0, "target": -179.0, "sign": 1.0},
		{"label": "negative_seam", "start": -179.0, "target": 179.0, "sign": -1.0},
	]
	for render_hz in [60, 120, 144]:
		for scenario in scenarios:
			var yaw := deg_to_rad(float(scenario.start))
			var target := deg_to_rad(float(scenario.target))
			var first_change := 0.0
			var converged := false
			for tick in 24:
				var frames_this_tick := 1
				if render_hz == 120:
					frames_this_tick = 2
				elif render_hz == 144:
					frames_this_tick = [2, 2, 3, 2, 3][tick % 5]
				var spent := 0.0
				for frame in frames_this_tick:
					var delta := 1.0 / float(render_hz)
					if tick == 1 and frame == 0:
						delta += 0.003
					elif tick == 4 and frame == 0:
						delta += 0.020
					var change: float = RIDER_POSE_SCRIPT.limited_yaw_change(
						yaw,
						target,
						delta,
						degrees_per_second,
						tick_budget - spent
					)
					if tick == 0 and frame == 0:
						first_change = change
					yaw = wrapf(yaw + change, -PI, PI)
					spent += absf(change)
				if spent > tick_budget + deg_to_rad(0.0001):
					failures.append("%s %d Hz airborne yaw exceeded one tick budget" % [scenario.label, render_hz])
					break
				if absf(wrapf(target - yaw, -PI, PI)) <= deg_to_rad(0.01):
					converged = true
					break
			if first_change * float(scenario.sign) <= 0.0:
				failures.append("%s %d Hz airborne yaw did not take the shortest arc" % [scenario.label, render_hz])
			if not converged:
				failures.append("%s %d Hz airborne yaw did not converge within 0.4 seconds" % [scenario.label, render_hz])

func _synthetic_physics_transform(tick: int) -> Transform3D:
	var time := float(tick) / 60.0
	# Constant forward travel plus alternating steering exercises position and yaw
	# without changing the fixed-step gameplay state.
	var yaw := 0.24 * sin(time * TAU * 0.75)
	var position := Vector3(2.0 * sin(time * 0.6), 1.6, -13.0 * time)
	return Transform3D(Basis(Vector3.UP, yaw), position)

func _check_network_rider(failures: Array[String]) -> void:
	if not ClassDB.class_exists(&"NetworkRider"):
		failures.append("native class NetworkRider is unavailable")
		return
	var network_rider := ClassDB.instantiate(&"NetworkRider") as Node3D
	if network_rider == null:
		failures.append("NetworkRider could not be instantiated")
		return
	var stance_events: Array[int] = []
	if not network_rider.has_signal(&"stance_changed"):
		failures.append("NetworkRider lacks remote pose stance_changed signal")
	else:
		network_rider.stance_changed.connect(func(_previous, current, _tick, _dive): stance_events.append(int(current)))
	if not bool(network_rider.call("push_snapshot", 10, Vector3.ZERO, Vector3(6, 0, 0), 350.0, STANCE_MOUNTED)):
		failures.append("NetworkRider rejected first valid snapshot")
	if not bool(network_rider.call("push_snapshot", 14, Vector3(4, 0, 0), Vector3(6, 0, 0), 10.0, STANCE_DIVE)):
		failures.append("NetworkRider rejected second valid snapshot")
	var sample := network_rider.call("sample_at", 12.0) as Dictionary
	if absf((sample.get("position", Vector3.ZERO) as Vector3).x - 2.0) > 0.001:
		failures.append("NetworkRider interpolation did not bridge jittered snapshots")
	if int(sample.get("stance_id", -1)) != STANCE_MOUNTED:
		failures.append("NetworkRider switched discrete stance before snapshot boundary")
	var boundary := network_rider.call("sample_at", 14.0) as Dictionary
	if int(boundary.get("stance_id", -1)) != STANCE_DIVE:
		failures.append("NetworkRider did not switch stance at snapshot boundary")
	if not bool(network_rider.call("push_snapshot", 18, Vector3(8, 0, 0), Vector3.ZERO, 10.0, 222)):
		failures.append("NetworkRider rejected unknown transport-compatible stance")
	var unknown := network_rider.call("sample_at", 30.0) as Dictionary
	if int(unknown.get("stance_id", -1)) != 222 or bool(unknown.get("stance_known", true)):
		failures.append("NetworkRider failed to retain unknown stance conservatively")
	if bool(network_rider.call("push_snapshot", 18, Vector3.ZERO, Vector3.ZERO, 0.0, 1)):
		failures.append("NetworkRider accepted stale/equal snapshot tick")
	var position_before := network_rider.position
	var correction := network_rider.call("reconciliation", 18, Vector3.ZERO, Vector3.ZERO) as Dictionary
	if bool(correction.get("snap", true)):
		failures.append("stance-only reconciliation forced a positional snap")
	if not bool(correction.get("stance_mismatch", false)):
		failures.append("NetworkRider reconciliation omitted stance mismatch")
	if int(network_rider.get("stance_id")) != 222 or bool(network_rider.get("stance_known")):
		failures.append("NetworkRider did not apply immediate conservative stance correction")
	if 222 not in stance_events:
		failures.append("NetworkRider stance correction did not notify remote pose")
	if network_rider.position != position_before:
		failures.append("stance reconciliation mutated presentation position")
	network_rider.free()

func _check_peer_session(failures: Array[String]) -> void:
	if not ClassDB.class_exists(&"PeerSession"):
		failures.append("native class PeerSession is unavailable")
		return
	var peer_session := ClassDB.instantiate(&"PeerSession") as Node
	if peer_session == null:
		failures.append("PeerSession could not be instantiated")
		return
	for method in ["configure_session", "generate_session_key", "session_public_key", "key_proof", "configure_secure_session", "make_heartbeat", "make_probe", "make_rider_input", "make_rider_snapshot", "decode_packet", "accept_packet", "accept_packet_with_source", "connect_demo_peer", "send_packet", "query_route", "shutdown"]:
		if not peer_session.has_method(method):
			failures.append("PeerSession lacks %s" % method)
	if not peer_session.has_signal("packet_received") or not peer_session.has_signal("route_updated"):
		failures.append("PeerSession lacks packet or route telemetry signals")
	for method in [
		"dispatch_packet_with_source", "make_shot_command", "resolve_shot_command",
		"make_shot_result", "poll_migration", "make_leave", "clear_lobby_session",
	]:
		if not peer_session.has_method(method):
			failures.append("PeerSession lacks M2 multiplayer method %s" % method)
	peer_session.call("set_insecure_demo_mode", true)
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
			failures.append("PeerSession failed to reject replayed heartbeat")
		var snapshot := peer_session.call(
			"make_rider_snapshot", 2, "00000000-0000-4000-8000-000000000002",
			Vector3(1, 2, 3), Vector3(4, 0, -2), 45.0, STANCE_DIVE
		) as PackedByteArray
		var decoded := peer_session.call("decode_packet", snapshot) as Dictionary
		if decoded.get("type", "") != "rider_snapshot" or decoded.get("position", Vector3.ZERO) != Vector3(1, 2, 3):
			failures.append("PeerSession snapshot codec omitted rider state")
		if str(decoded.get("rider_player_id", "")) != "00000000-0000-4000-8000-000000000002":
			failures.append("PeerSession snapshot omitted the authority-owned rider subject")
		if int(decoded.get("stance_id", -1)) != STANCE_DIVE or not bool(decoded.get("stance_known", false)):
			failures.append("PeerSession snapshot codec omitted known stance")
		var bad_stance := peer_session.call(
			"make_rider_snapshot", 3, "00000000-0000-4000-8000-000000000002",
			Vector3.ZERO, Vector3.ZERO, 0.0, 222
		) as PackedByteArray
		if not bad_stance.is_empty():
			failures.append("PeerSession outbound snapshot accepted unknown local stance")
		var outsider := peer_session.call(
			"make_rider_snapshot", 4, "00000000-0000-4000-8000-000000000099",
			Vector3.ZERO, Vector3.ZERO, 0.0, 1
		) as PackedByteArray
		if not outsider.is_empty():
			failures.append("PeerSession authority snapshot accepted an unknown rider subject")
		var reserved_buttons := peer_session.call("make_rider_input", 4, 0, 0, 4) as PackedByteArray
		if not reserved_buttons.is_empty():
			failures.append("PeerSession accepted nonzero reserved rider-input bits")
	peer_session.free()

func _check_native_apis(horse: CharacterBody3D, rider: CharacterBody3D, failures: Array[String]) -> void:
	for method in ["reset_horse", "set_archetype", "get_archetype_stats", "start_dive_runout", "set_external_simulation_tick", "complete_remount"]:
		if not horse.has_method(method):
			failures.append("HorseController lacks %s" % method)
	for signal_name in [&"gait_changed", &"telemetry_updated", &"runout_started", &"horse_retrievable"]:
		if not horse.has_signal(signal_name):
			failures.append("HorseController lacks %s signal" % signal_name)
	for method in ["advance_tick", "resolve_motion", "apply_external_damage", "observe_death", "settle_observations_through", "end_match", "reset_rider", "get_snapshot_state"]:
		if not rider.has_method(method):
			failures.append("SaddleDiveController lacks %s" % method)
	for unsafe_method in ["record_shot_attempt", "record_accepted_shot", "record_authority_result"]:
		if rider.has_method(unsafe_method):
			failures.append("SaddleDiveController exposes forgeable %s bridge" % unsafe_method)
	for property_name in ["current_tick", "stance_id", "stance_known", "dive_id", "movement_scale", "airtime_seconds", "rider_health", "can_fire", "can_reload"]:
		if rider.get(property_name) == null:
			failures.append("SaddleDiveController lacks %s property" % property_name)
	for signal_name in [&"stance_changed", &"dive_started", &"dive_landed", &"recovery_changed", &"recovery_completed", &"landing_damage_applied", &"gameplay_event", &"dive_telemetry_updated", &"dive_telemetry_finalized", &"rider_died", &"telemetry_updated"]:
		if not rider.has_signal(signal_name):
			failures.append("SaddleDiveController lacks %s signal" % signal_name)

func _exercise_reload_after_remount(
	course: Node,
	horse: CharacterBody3D,
	rider: CharacterBody3D,
	failures: Array[String]
) -> void:
	var controller := course.get_node("Rider/WeaponController")
	var hud := course.get_node("CombatLayer/CombatHUD") as Control
	await _wait_physics_frames(5)
	var initial := controller.call("get_weapon_stats") as Dictionary
	if int(initial.get("ammo_mag", -1)) != 30 or int(initial.get("ammo_reserve", -1)) != 120:
		failures.append("reload regression did not start from Dustwalker 30 | 120")
		return

	# Establish the exact hands-on fixture without a debug ammo setter: fire 13,
	# reload to consume 13 reserve, then empty the 30-round magazine.
	var fired_setup := await _fire_mounted_rounds(controller, 13, 8)
	var setup_reload_accepted := bool(controller.call("request_reload")) if fired_setup == 13 else false
	if fired_setup != 13 or not setup_reload_accepted:
		var setup_stats := controller.call("get_weapon_stats") as Dictionary
		failures.append(
			"could not establish Dustwalker setup: fired=%d ammo=%d|%d reason=%s" % [
				fired_setup,
				int(setup_stats.get("ammo_mag", -1)),
				int(setup_stats.get("ammo_reserve", -1)),
				str(controller.get("last_reject_reason")),
			]
		)
		return
	var setup_start := int(controller.get("current_tick"))
	await _wait_controller_tick(controller, setup_start + 126, 140)
	var loaded := controller.call("get_weapon_stats") as Dictionary
	if int(loaded.get("ammo_mag", -1)) != 30 or int(loaded.get("ammo_reserve", -1)) != 107:
		failures.append("setup reload did not produce Dustwalker 30 | 107")
		return
	var fired_mag := await _fire_mounted_rounds(controller, 30, 8)
	var empty := controller.call("get_weapon_stats") as Dictionary
	if fired_mag != 30 or int(empty.get("ammo_mag", -1)) != 0 or int(empty.get("ammo_reserve", -1)) != 107:
		failures.append("reload regression could not establish exact 0 | 107 fixture")
		return

	var starts: Array[Vector2i] = []
	var progress: Array[float] = []
	var completions: Array[Vector3i] = []
	var rejections: Array[String] = []
	controller.reload_started.connect(func(tick, required):
		starts.append(Vector2i(int(tick), int(required)))
	)
	controller.reload_progressed.connect(func(_tick, value, _active, _required):
		progress.append(float(value))
	)
	controller.reload_completed.connect(func(tick, mag, reserve):
		completions.append(Vector3i(int(tick), int(mag), int(reserve)))
	)
	controller.reload_rejected.connect(func(_tick, reason):
		rejections.append(str(reason))
	)

	await _wait_until_grounded(horse, 90)
	horse.velocity = Vector3(0, 0, -9.0)
	await _press_action_one_tick(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_DIVE:
		failures.append("reload regression could not enter Saddle Dive")
		return
	await _press_action_one_tick(&"combat_reload")
	for _frame in 80:
		if int(rider.get("stance_id")) != STANCE_DIVE:
			break
		await get_tree().physics_frame
	if int(rider.get("stance_id")) != STANCE_PRONE:
		failures.append("reload regression did not reach landing recovery")
		return
	await _press_action_one_tick(&"combat_reload")
	for _frame in 100:
		if int(rider.get("stance_id")) == STANCE_ON_FOOT:
			break
		await get_tree().physics_frame
	for _frame in 150:
		if bool(horse.get("is_retrievable")):
			break
		await get_tree().physics_frame
	if int(rider.get("stance_id")) != STANCE_ON_FOOT or not bool(horse.get("is_retrievable")):
		failures.append("reload regression did not reach retrievable on-foot state")
		return
	rider.global_position = horse.global_position + Vector3(0, 0, 1.0)
	await _pulse_action(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_MOUNTED:
		failures.append("reload regression did not remount")
		return

	# The physical R edge must be acknowledged immediately and complete from
	# native active ticks exactly 126 ticks later.
	await _press_action_one_tick(&"combat_reload")
	await get_tree().process_frame
	var reload_ring := hud.get_node("%ReloadRing") as ProgressBar
	if starts.size() != 1 or starts[0].y != 126:
		failures.append("post-remount physical R did not emit one 126-tick reload start")
	elif not reload_ring.visible or reload_ring.value <= 0.0:
		failures.append("post-remount reload HUD was not visible within one render frame")
	for _frame in 140:
		if not bool(controller.get("is_reloading")):
			break
		await get_tree().physics_frame
	await get_tree().process_frame
	if completions.size() != 1:
		failures.append("post-remount reload did not complete exactly once")
	else:
		var completion := completions[0]
		if completion.x - starts[0].x != 126:
			failures.append("post-remount reload completed after %d ticks, expected 126" % (completion.x - starts[0].x))
		if completion.y != 30 or completion.z != 77:
			failures.append("post-remount reload completed at %d | %d, expected 30 | 77" % [completion.y, completion.z])
	for index in range(1, progress.size()):
		if progress[index] + 0.000001 < progress[index - 1]:
			failures.append("native reload progress regressed")
			break
	if reload_ring.visible:
		failures.append("reload indicator did not clear on native completion")
	if "airborne" not in rejections or "recovering" not in rejections:
		failures.append("reload rejection feedback omitted airborne/recovering reasons")
	var final_stats := controller.call("get_weapon_stats") as Dictionary
	if int(final_stats.get("ammo_mag", -1)) != 30 or int(final_stats.get("ammo_reserve", -1)) != 77:
		failures.append("post-remount reload did not retain exact 30 | 77 ammo")

func _fire_mounted_rounds(controller: Node, count: int, cadence_ticks: int) -> int:
	var accepted := 0
	var attempts := 0
	while accepted < count and attempts < count * 2:
		attempts += 1
		var tick := int(controller.get("current_tick"))
		if bool(controller.call("request_fire", controller.global_position, Vector3.BACK, tick)):
			accepted += 1
			controller.call("resolve_local_miss")
		await _wait_controller_tick(controller, tick + cadence_ticks, cadence_ticks + 4)
	return accepted

func _exercise_native_input(course: Node, horse: CharacterBody3D, failures: Array[String]) -> void:
	await _wait_physics_frames(5)
	if not horse.has_method("set_archetype") or not horse.has_method("get_archetype_stats"):
		failures.append("HorseController lacks M0.5 archetype API")
	else:
		horse.call("set_archetype", 0)
		if int(horse.get("archetype")) != 0:
			failures.append("Courser archetype selection failed")
		var stats := horse.call("get_archetype_stats") as Dictionary
		if float(stats.get("max_vitality", 0.0)) <= 0.0:
			failures.append("archetype stats omitted max_vitality")
		horse.call("set_archetype", 2)

	horse.call("reset_horse")
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
		failures.append("D did not use Godot negative-yaw right-turn convention")
	horse.call("reset_horse")
	await _exercise_native_combat(course, failures)

func _exercise_native_combat(course: Node, failures: Array[String]) -> void:
	var controller := course.get_node_or_null("Rider/WeaponController")
	var target := course.get_node_or_null("TargetBodyNear")
	if controller == null or target == null:
		failures.append("integrated weapon controller or target dummy is missing")
		return
	for method in ["equip_weapon", "request_fire", "request_reload", "preview_dive_direction", "get_weapon_stats", "resolve_local_hit", "resolve_local_miss", "set_rider_context", "advance_to_tick"]:
		if not controller.has_method(method):
			failures.append("MountedWeaponController lacks %s" % method)
	for signal_name in [&"reload_started", &"reload_progressed", &"reload_completed", &"reload_rejected"]:
		if not controller.has_signal(signal_name):
			failures.append("MountedWeaponController lacks %s signal" % signal_name)
	if not failures.is_empty():
		return
	for vector in [
		{"angle": 0.0, "expected": 0.0, "clamped": false},
		{"angle": 45.0, "expected": 45.0, "clamped": false},
		{"angle": -45.0, "expected": -45.0, "clamped": false},
		{"angle": 75.0, "expected": 75.0, "clamped": false},
		{"angle": -75.0, "expected": -75.0, "clamped": false},
		{"angle": 90.0, "expected": 75.0, "clamped": true},
		{"angle": -90.0, "expected": -75.0, "clamped": true},
		{"angle": 180.0, "expected": 75.0, "clamped": true},
	]:
		var radians := deg_to_rad(float(vector.angle))
		var chosen := Vector3(-sin(radians), 0.0, -cos(radians))
		var preview := controller.call(
			"preview_dive_direction",
			Vector3(0, 0, -9.0),
			chosen
		) as Dictionary
		if absf(float(preview.get("clamped_angle_degrees", 999.0)) - float(vector.expected)) > 0.01:
			failures.append("native dive preview disagreed at %.0f degrees" % float(vector.angle))
		if bool(preview.get("direction_was_clamped", false)) != bool(vector.clamped):
			failures.append("native dive preview clamp flag disagreed at %.0f degrees" % float(vector.angle))
	await _wait_physics_frames(20)
	controller.call("equip_weapon", 0)
	var origin: Vector3 = controller.global_position
	var body_zone := target.get_node("BodyZone") as StaticBody3D
	var direction := origin.direction_to(body_zone.global_position)
	var tick := int(controller.get("current_tick"))
	var vitality_before := float(target.get("vitality"))
	if not bool(controller.call("request_fire", origin, direction, tick)):
		failures.append("native mounted rifle rejected valid first shot")
		return
	var distance := origin.distance_to(body_zone.global_position)
	if not bool(controller.call("resolve_local_hit", 101, "body", distance)):
		failures.append("native local authority rejected valid target evidence")
		return
	await get_tree().process_frame
	if float(target.get("vitality")) >= vitality_before:
		failures.append("authority-computed rifle damage did not reach target dummy")

func _exercise_m2(course: Node, horse: CharacterBody3D, rider: CharacterBody3D, failures: Array[String]) -> void:
	var controller := course.get_node("Rider/WeaponController")
	var camera := course.get_node("CameraRig/PitchPivot/SpringArm3D/Camera3D") as Camera3D
	var camera_rig := course.get_node("CameraRig") as Node3D
	var pose := course.get_node("Rider/RiderProxy") as Node3D
	if camera_rig.get("telemetry_source") != rider:
		failures.append("camera FOV telemetry does not follow the logical Rider")
	var events: Array[String] = []
	var telemetry: Array[Dictionary] = []
	var synchronous_landing_fire: Array[bool] = []
	rider.gameplay_event.connect(func(_id, _kind, payload): events.append(str(payload.get("text", ""))))
	rider.dive_telemetry_updated.connect(func(row): telemetry.append((row as Dictionary).duplicate()))
	rider.stance_changed.connect(func(_previous, current, tick, _dive):
		if int(current) == STANCE_PRONE:
			synchronous_landing_fire.append(bool(controller.call(
				"request_fire", controller.global_position, Vector3.BACK, int(tick)
			)))
	)

	# Exact below-threshold path uses the real E action and creates no DiveId.
	await _reset_course_with_input()
	await _wait_until_grounded(horse, 90)
	horse.velocity = Vector3(0, 0, -7.999)
	await _pulse_action(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_ON_FOOT or int(rider.get("dive_id")) != -1:
		failures.append("7999 mm/s E did not ordinary-dismount without DiveId")
	if not bool(horse.get("is_retrievable")):
		failures.append("ordinary dismount horse was not immediately retrievable")
	var raised_y := rider.global_position.y + 2.0
	rider.global_position.y = raised_y
	rider.velocity = Vector3.ZERO
	await _wait_physics_frames(10)
	var gravity_drop := raised_y - rider.global_position.y
	if gravity_drop < 0.1 or gravity_drop > 1.0:
		failures.append("detached rider gravity drop %.3f m was not 22 m/s^2 scale" % gravity_drop)
	await _pulse_action(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_MOUNTED:
		failures.append("stationary E remount did not attach to existing horse")

	# Reach the discrete Gallop handling row and naturally resolve one mounted hit.
	Input.action_press(&"move_forward")
	await _wait_physics_frames(3)
	await _pulse_action(&"gait_up")
	await _pulse_action(&"gait_up")
	await _wait_physics_frames(20)
	var target := course.get_node("TargetBodyNear")
	var body_zone := target.get_node("BodyZone") as StaticBody3D
	var origin: Vector3 = controller.global_position
	var tick := int(controller.get("current_tick"))
	if bool(controller.call("request_fire", origin, origin.direction_to(body_zone.global_position), tick)):
		controller.call("resolve_local_hit", 101, "body", origin.distance_to(body_zone.global_position))
	await get_tree().physics_frame
	Input.action_release(&"move_forward")
	await _wait_physics_frames(10)

	# Inclusive 8 m/s threshold launches a real separate body and leaves the same
	# horse object running out under collision feedback.
	var horse_identity := horse.get_instance_id()
	var horse_launch_position := horse.global_position
	# Align the fixture before pinning aim 90 degrees right, making the regression
	# independent of earlier steering while deterministically exercising the clamp.
	horse.rotation = Vector3.ZERO
	horse.velocity = Vector3.ZERO
	await get_tree().physics_frame
	# reset_follow preserves player-owned yaw while synchronizing the rendered camera.
	camera_rig.set("_world_yaw", deg_to_rad(90.0))
	camera_rig.call("reset_follow")
	horse.velocity = Vector3(0, 0, -9.0)
	await _pulse_action(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_DIVE or int(rider.get("dive_id")) <= 0:
		failures.append(">=8 m/s E did not start Saddle Dive")
		return
	if int(horse.get("control_mode")) != 1:
		failures.append("accepted dive did not start horse runout")
	if rider.get_parent() != horse.get_parent():
		failures.append("Rider is not a root-sibling separate body")

	# One authority-confirmed behind headshot exercises late attribution and event
	# dedup without pretending this forced smoke is a natural-play observation.
	var dive_shot_tick := int(controller.get("current_tick"))
	if bool(controller.call("request_fire", controller.global_position, Vector3.BACK, dive_shot_tick)):
		if not bool(controller.call("resolve_local_hit", 102, "head", 25.0)):
			failures.append("typed local authority dive result was not attributed")
		if bool(controller.call("resolve_local_hit", 102, "head", 25.0)):
			failures.append("duplicate authority result was not deduplicated")
	else:
		failures.append("first legal Saddle Dive shot was rejected")

	var first_visual := await _measure_airborne_pose(
		rider, pose, camera, failures, "right clamped dive"
	)
	if int(rider.get("stance_id")) != STANCE_PRONE:
		failures.append("actual collision arc did not enter landing prone")
	if synchronous_landing_fire != [false]:
		failures.append("landing signal observed an open airborne combat context")
	var airtime := float(rider.get("airtime_seconds"))
	if airtime < 0.7 or airtime > 0.9:
		failures.append("scene Saddle Dive airtime %.3f was outside 0.7-0.9 s" % airtime)
	if float(first_visual.max_delta) > deg_to_rad(6.05):
		failures.append("right clamped airborne visual yaw exceeded 6 degrees per 60 Hz frame")
	if float(first_visual.maximum_yaw) <= 0.0:
		failures.append("right clamped airborne pose turned away from its launch direction")
	if bool(rider.get("can_fire")) or bool(rider.get("can_reload")) or float(rider.get("movement_scale")) != 0.0:
		failures.append("landing prone did not block fire/reload/movement input")
	if bool(controller.call("request_reload")):
		failures.append("reload was accepted during landing recovery")

	var recovery_seen := false
	var ready_seen := false
	for _frame in 100:
		await get_tree().physics_frame
		var stance := int(rider.get("stance_id"))
		if stance == STANCE_RECOVERY:
			recovery_seen = true
			if not is_equal_approx(float(rider.get("movement_scale")), 0.5) or bool(rider.get("can_fire")):
				failures.append("half-speed recovery policy was incorrect")
				break
		if stance == STANCE_ON_FOOT:
			ready_seen = true
			break
	if not recovery_seen or not ready_seen:
		failures.append("normal 0.4 + 0.4 second recovery phases were not observed")

	for _frame in 150:
		if bool(horse.get("is_retrievable")):
			break
		await get_tree().physics_frame
	if horse.get_instance_id() != horse_identity:
		failures.append("horse object identity changed during runout")
	if not bool(horse.get("is_retrievable")) or int(horse.get("control_mode")) != 2:
		failures.append("horse did not stop idle/retrievable after runout")
	if float(horse.get("runout_distance_m")) > 25.001:
		failures.append("horse runout exceeded 25 m cumulative cap")
	var horse_world_offset := horse.global_position - horse_launch_position
	if Vector2(horse_world_offset.x, horse_world_offset.z).length() > 25.001:
		failures.append("horse world position crossed the 25 m runout cap")
	var remount_hint := course.get_node("HUD/Panel/Margin/VBox/RemountHint") as Label
	var affordance_ready := await _wait_for_remount_hint(remount_hint, horse, rider, false)
	if not affordance_ready:
		failures.append("retrievable horse lacked a HUD affordance: %s" % _remount_hint_diagnostic(remount_hint, horse, rider))

	# Test setup moves only the rider into the normal 3 m interaction range; the
	# horse remains where collision-resolved runout stopped.
	rider.global_position = horse.global_position + Vector3(0, 0, 1.0)
	var in_range_ready := await _wait_for_remount_hint(remount_hint, horse, rider, true)
	if not in_range_ready:
		failures.append("in-range horse lacked the E remount prompt: %s" % _remount_hint_diagnostic(remount_hint, horse, rider))
	await _pulse_action(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_MOUNTED:
		failures.append("eligible E remount did not complete")

	# Repeat in the opposite direction after the full land/remount cycle. This
	# catches stale limiter state and both signs of the protocol's 75-degree clamp.
	await _wait_physics_frames(10)
	horse.rotation = Vector3.ZERO
	horse.velocity = Vector3.ZERO
	await get_tree().physics_frame
	camera_rig.set("_world_yaw", deg_to_rad(-90.0))
	camera_rig.call("reset_follow")
	horse.velocity = Vector3(0, 0, -9.0)
	await _pulse_action(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_DIVE:
		failures.append("repeated left-clamped Saddle Dive did not start")
	else:
		var second_visual := await _measure_airborne_pose(
			rider, pose, camera, failures, "left clamped dive"
		)
		if float(second_visual.max_delta) > deg_to_rad(6.05):
			failures.append("left clamped airborne visual yaw exceeded 6 degrees per 60 Hz frame")
		if float(second_visual.minimum_yaw) >= 0.0:
			failures.append("left clamped airborne pose turned away from its launch direction")
	if synchronous_landing_fire != [false, false]:
		failures.append("repeated landing signal observed an open airborne combat context")
	for _frame in 100:
		if int(rider.get("stance_id")) == STANCE_ON_FOOT:
			break
		await get_tree().physics_frame
	for _frame in 150:
		if bool(horse.get("is_retrievable")):
			break
		await get_tree().physics_frame
	rider.global_position = horse.global_position + Vector3(0, 0, 1.0)
	await _pulse_action(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_MOUNTED:
		failures.append("repeated dive did not complete its remount reset")
	if telemetry.size() < 2:
		failures.append("repeated dives emitted fewer than two telemetry rows")
	else:
		for row_index in range(telemetry.size() - 2, telemetry.size()):
			var dive_row := telemetry[row_index] as Dictionary
			if not bool(dive_row.get("direction_was_clamped", false)):
				failures.append("repeated dive did not report a clamped launch direction")
			if absf(absf(float(dive_row.get("clamped_angle_degrees", 0.0))) - 75.0) > 0.01:
				failures.append("repeated dive did not retain the 75-degree launch clamp")
		var row := telemetry.back() as Dictionary
		for key in ["prelaunch_speed_mps", "requested_angle_degrees", "clamped_angle_degrees", "airtime_ticks", "shots_fired", "shots_hit", "damage_dealt", "landing_terrain", "landing_slope_degrees", "landing_outcome", "time_to_remount_ticks"]:
			if not row.has(key):
				failures.append("dive telemetry omitted %s" % key)
	for expected in ["FLYING DISMOUNT", "SADDLE DIVE HEADSHOT", "FULL-GALLOP HIT", "AIRBORNE REVERSAL"]:
		if expected not in events:
			failures.append("deterministic gameplay event missing: %s" % expected)

func _measure_airborne_pose(
	rider: CharacterBody3D,
	pose: Node3D,
	camera: Camera3D,
	failures: Array[String],
	label: String
) -> Dictionary:
	var previous_yaw := pose.rotation.y
	var maximum_delta := 0.0
	var minimum_yaw := previous_yaw
	var maximum_yaw := previous_yaw
	var airborne_frames := 0
	while int(rider.get("stance_id")) == STANCE_DIVE and airborne_frames < 70:
		await get_tree().physics_frame
		var yaw := pose.rotation.y
		maximum_delta = maxf(maximum_delta, absf(wrapf(yaw - previous_yaw, -PI, PI)))
		minimum_yaw = minf(minimum_yaw, yaw)
		maximum_yaw = maxf(maximum_yaw, yaw)
		previous_yaw = yaw
		airborne_frames += 1
		if not rider.global_position.is_finite() or not camera.global_position.is_finite():
			failures.append("%s rider/camera transform became nonfinite" % label)
			break
		if camera.fov < 69.99 or camera.fov > 78.01 or absf(camera.global_rotation.z) > 0.001:
			failures.append("%s introduced camera FOV/roll outside conservative plan" % label)
			break
	return {
		"max_delta": maximum_delta,
		"minimum_yaw": minimum_yaw,
		"maximum_yaw": maximum_yaw,
		"airborne_frames": airborne_frames,
	}

func _wait_for_remount_hint(
	hint: Label,
	horse: CharacterBody3D,
	rider: CharacterBody3D,
	require_in_range: bool
) -> bool:
	for _frame in HUD_SETTLE_PROCESS_FRAMES:
		await get_tree().process_frame
		if not hint.visible:
			continue
		if require_in_range and hint.text == "E — REMOUNT":
			return true
		if not require_in_range and (hint.text.begins_with("HORSE READY") or hint.text == "E — REMOUNT"):
			return true
	return false

func _remount_hint_diagnostic(
	hint: Label,
	horse: CharacterBody3D,
	rider: CharacterBody3D
) -> String:
	return "is_retrievable=%s control_mode=%s stance_id=%s visible=%s label=%s waited_process_frames=%d" % [
		str(bool(horse.get("is_retrievable"))),
		str(int(horse.get("control_mode"))),
		str(int(rider.get("stance_id"))),
		str(hint.visible),
		var_to_str(hint.text),
		HUD_SETTLE_PROCESS_FRAMES,
	]

func _exercise_landing_boundaries(
	course: Node,
	horse: CharacterBody3D,
	rider: CharacterBody3D,
	failures: Array[String]
) -> void:
	var telemetry_by_dive: Dictionary = {}
	var landing_damage_events: Array[Dictionary] = []
	var death_ticks: Array[int] = []
	rider.dive_telemetry_updated.connect(func(row):
		var copy := (row as Dictionary).duplicate()
		telemetry_by_dive[int(copy.get("dive_id", -1))] = copy
	)
	rider.landing_damage_applied.connect(func(dive_id, amount, health_after):
		landing_damage_events.append({
			"dive_id": int(dive_id),
			"amount": int(amount),
			"health_after": int(health_after),
		})
	)
	rider.rider_died.connect(func(tick): death_ticks.append(int(tick)))

	for fixture in [
		{"x": 58.0, "bad": false, "minimum_slope": 29.8, "maximum_slope": 30.0, "recovery": 48},
		{"x": 68.0, "bad": true, "minimum_slope": 30.5, "maximum_slope": 31.5, "recovery": 72},
	]:
		await _reset_course_with_input()
		await _wait_until_grounded(horse, 90)
		horse.global_position = Vector3(float(fixture.x), 1.2, 20.0)
		horse.rotation = Vector3.ZERO
		horse.velocity = Vector3.ZERO
		# A direct fixture teleport invalidates CharacterBody3D's cached floor
		# state. Process it once, then wait for genuine post-move floor contact.
		await get_tree().physics_frame
		await _wait_until_grounded(horse, 30)
		if not horse.is_on_floor():
			failures.append("landing fixture %.0f did not settle on its launch pad" % float(fixture.x))
			continue
		if bool(fixture.bad):
			var damage_tick := int(rider.get("current_tick"))
			if not bool(rider.call("apply_external_damage", damage_tick, 9001, 85)):
				failures.append("stable pre-landing damage observation was rejected")
			if bool(rider.call("apply_external_damage", damage_tick, 9001, 85)):
				failures.append("external damage replay changed rider health")
			if int(rider.get("rider_health")) != 15:
				failures.append("external damage replay guard did not preserve 15 HP setup")
		horse.velocity = Vector3(0, 0, -9.0)
		var damage_events_before := landing_damage_events.size()
		var death_events_before := death_ticks.size()
		await _press_action_one_tick(&"combat_interact")
		var dive_id := int(rider.get("dive_id"))
		if int(rider.get("stance_id")) != STANCE_DIVE or dive_id <= 0:
			failures.append("landing fixture %.0f did not start a real dive" % float(fixture.x))
			continue
		for _frame in 80:
			if int(rider.get("stance_id")) != STANCE_DIVE:
				break
			await get_tree().physics_frame
		if int(rider.get("stance_id")) != STANCE_PRONE:
			failures.append("landing fixture %.0f did not produce post-move contact" % float(fixture.x))
			continue
		var landing_tick := int(rider.get("current_tick"))
		var health_after_contact := int(rider.get("rider_health"))
		if bool(rider.call("resolve_motion", landing_tick)):
			failures.append("duplicate landing contact was accepted")
		if int(rider.get("rider_health")) != health_after_contact:
			failures.append("duplicate landing contact applied damage twice")
		var row := telemetry_by_dive.get(dive_id, {}) as Dictionary
		var slope := float(row.get("landing_slope_degrees", -1.0))
		if slope < float(fixture.minimum_slope) or slope > float(fixture.maximum_slope):
			failures.append("authored landing slope %.3f missed expected fixture band" % slope)
		if str(row.get("landing_outcome", "")) != ("bad" if bool(fixture.bad) else "good"):
			failures.append("strict >30 degree landing outcome was incorrect")
		var expected_damage := 15 if bool(fixture.bad) else 0
		if int(row.get("landing_damage", -1)) != expected_damage:
			failures.append("landing telemetry damage did not match slope outcome")
		if landing_damage_events.size() - damage_events_before != (1 if bool(fixture.bad) else 0):
			failures.append("landing damage signal multiplicity was incorrect")
		if bool(fixture.bad):
			if health_after_contact != 0 or death_ticks.size() - death_events_before != 1:
				failures.append("lethal 15-damage landing did not emit exactly one rider_died")
		for _frame in 90:
			if int(rider.get("stance_id")) == STANCE_ON_FOOT:
				break
			await get_tree().physics_frame
		var recovery_ticks := int(rider.get("current_tick")) - landing_tick
		if recovery_ticks != int(fixture.recovery):
			failures.append("landing recovery lasted %d ticks; expected %d" % [recovery_ticks, int(fixture.recovery)])

	# Kill-plane/reset requests are consumed by the shared clock owner and must
	# not pre-allocate the coordinator's next tick.
	var coordinator := course.get_node("M2Gameplay")
	coordinator.call("request_course_reset")
	await get_tree().physics_frame
	var reset_tick := int(coordinator.get("simulation_tick"))
	if int(rider.get("current_tick")) != reset_tick:
		failures.append("coordinator reset did not use its own absolute tick")
	await get_tree().physics_frame
	if int(rider.get("current_tick")) != reset_tick + 1:
		failures.append("post-reset shared tick replayed or stalled")

func _exercise_bridge_caps(
	course: Node,
	horse: CharacterBody3D,
	rider: CharacterBody3D,
	failures: Array[String]
) -> void:
	var controller := course.get_node("Rider/WeaponController") as Node3D
	for weapon_value in [1, 0, 2]:
		var weapon_id: int = int(weapon_value)
		await _reset_course_with_input()
		await _wait_until_grounded(horse, 90)
		if not bool(controller.call("equip_weapon", weapon_id)):
			failures.append("could not equip weapon %d for integrated cap smoke" % weapon_id)
			continue
		horse.velocity = Vector3(0, 0, -9.0)
		await _press_action_one_tick(&"combat_interact")
		if int(rider.get("stance_id")) != STANCE_DIVE:
			failures.append("weapon %d cap smoke did not enter a real dive" % weapon_id)
			continue
		var cap: int = 1 if weapon_id == 1 else (3 if weapon_id == 0 else 5)
		var cadence: int = 15 if weapon_id == 1 else (8 if weapon_id == 0 else 6)
		var first_tick := int(controller.get("current_tick"))
		var accepted := 0
		for index in cap:
			var target_tick := first_tick + index * cadence
			await _wait_controller_tick(controller, target_tick, 40)
			var shot_tick := int(controller.get("current_tick"))
			if bool(controller.call(
				"request_fire", controller.global_position, Vector3.BACK, shot_tick
			)):
				accepted += 1
				controller.call("resolve_local_miss")
		var blocked_target := first_tick + cap * cadence
		await _wait_controller_tick(controller, blocked_target, 40)
		var blocked_tick := int(controller.get("current_tick"))
		if bool(controller.call(
			"request_fire", controller.global_position, Vector3.BACK, blocked_tick
		)):
			failures.append("weapon %d accepted cap+1 Saddle Dive shot" % weapon_id)
		if str(controller.get("last_reject_reason")) != "dive_shot_cap":
			failures.append("weapon %d cap rejection lost internal reason" % weapon_id)
		if accepted != cap:
			failures.append("weapon %d accepted %d/%d capped shots" % [weapon_id, accepted, cap])
		if bool(controller.call("request_reload")):
			failures.append("weapon %d reloaded during Saddle Dive" % weapon_id)

	# The same integrated authority path must retain ordinary horse-jump air
	# rejection; no script may claim Mounted or manufacture a DiveId.
	await _reset_course_with_input()
	await _wait_until_grounded(horse, 90)
	Input.action_press(&"jump")
	await _wait_physics_frames(2)
	Input.action_release(&"jump")
	for _frame in 30:
		if not horse.is_on_floor() and int(rider.get("stance_id")) == STANCE_MOUNTED_AIRBORNE:
			break
		await get_tree().physics_frame
	var jump_tick := int(controller.get("current_tick"))
	if bool(controller.call(
		"request_fire", controller.global_position, Vector3.FORWARD, jump_tick
	)):
		failures.append("ordinary mounted-airborne shot became legal")
	elif str(controller.get("last_reject_reason")) != "airborne":
		failures.append("ordinary jump did not retain airborne rejection")

func _check_persisted_telemetry(course: Node, failures: Array[String]) -> void:
	var coordinator := course.get_node("M2Gameplay")
	coordinator.call("_close_telemetry_session", "headless_smoke")
	var path := str(coordinator.get("telemetry_log_path"))
	if path.is_empty() or not FileAccess.file_exists(path):
		failures.append("secret-free M2 telemetry session was not persisted")
		return
	var file := FileAccess.open(path, FileAccess.READ)
	if file == null:
		failures.append("persisted M2 telemetry session could not be reopened")
		return
	var finalized_keys: Dictionary = {}
	var finalized_count := 0
	while not file.eof_reached():
		var line := file.get_line()
		if line.is_empty():
			continue
		var parsed: Variant = JSON.parse_string(line)
		if not parsed is Dictionary:
			failures.append("persisted M2 telemetry contains a non-JSONL row")
			continue
		var row := parsed as Dictionary
		if str(row.get("record_type", "")) != "dive_finalized":
			continue
		finalized_count += 1
		var key := "%s:%s:%s" % [
			str(row.get("authority_epoch", 0)),
			str(row.get("actor", "")),
			str(row.get("dive_id", -1)),
		]
		if finalized_keys.has(key):
			failures.append("persisted M2 telemetry duplicated finalized dive %s" % key)
		finalized_keys[key] = true
		for required in [
			"launch_tick", "prelaunch_speed_mmps", "direction_was_clamped",
			"shots_fired", "shots_hit", "landing_tick", "death_within_3s",
			"time_to_remount_ticks", "censor_reason"
		]:
			if not row.has(required):
				failures.append("persisted dive row omitted aggregation field %s" % required)
	var text := FileAccess.get_file_as_string(path).to_lower()
	for forbidden in ["credential", "capability", "oauth", "join_code", "endpoint", "lobby_seed"]:
		if forbidden in text:
			failures.append("persisted M2 telemetry leaked prohibited field %s" % forbidden)
	if finalized_count == 0:
		failures.append("persisted M2 telemetry contains no finalized/censored dive")

func _wait_controller_tick(controller: Node, target_tick: int, maximum_frames: int) -> void:
	for _frame in maximum_frames:
		if int(controller.get("current_tick")) >= target_tick:
			return
		await get_tree().physics_frame

func _reset_course_with_input() -> void:
	await _pulse_action(&"reset_horse")

func _press_action_one_tick(action: StringName) -> void:
	Input.action_press(action)
	# `physics_frame` is emitted before node physics callbacks. Waiting for the
	# next two signals guarantees exactly one coordinator tick saw the pulse.
	await _wait_physics_frames(2)
	Input.action_release(action)

func _pulse_action(action: StringName) -> void:
	Input.action_press(action)
	await _wait_physics_frames(2)
	Input.action_release(action)
	await _wait_physics_frames(2)

func _wait_until_grounded(body: CharacterBody3D, maximum_frames: int) -> void:
	for _frame in maximum_frames:
		if body.is_on_floor():
			return
		await get_tree().physics_frame

func _wait_physics_frames(count: int) -> void:
	for _frame in count:
		await get_tree().physics_frame

func _finish(failures: Array[String]) -> void:
	for action in [&"move_forward", &"move_back", &"steer_left", &"steer_right", &"gait_up", &"combat_fire", &"combat_reload", &"combat_interact", &"reset_horse"]:
		Input.action_release(action)
	if failures.is_empty():
		print("SPURFIRE_GODOT_SMOKE_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error("SMOKE: " + failure)
		get_tree().quit(1)
