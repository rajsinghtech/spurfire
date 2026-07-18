extends Node

const REQUIRED_ACTIONS := [
	&"move_forward", &"move_back", &"steer_left", &"steer_right",
	&"gait_up", &"gait_down", &"hard_brake", &"jump", &"reset_horse", &"scoreboard",
	&"combat_fire", &"combat_reload", &"combat_interact"
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
	"NetworkLayer/Panel/Margin/Label", "NetworkLayer/RosterPanel/Margin/VBox/Rows"
]
const STANCE_MOUNTED := 1
const STANCE_MOUNTED_AIRBORNE := 2
const STANCE_DIVE := 3
const STANCE_PRONE := 4
const STANCE_RECOVERY := 5
const STANCE_ON_FOOT := 6

func _ready() -> void:
	var failures: Array[String] = []
	_check_input_map(failures)
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
	await _exercise_native_input(course, horse, failures)
	await _exercise_m2(course, horse, rider, failures)
	await _exercise_landing_boundaries(course, horse, rider, failures)
	await _exercise_bridge_caps(course, horse, rider, failures)
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
	for method in ["configure_session", "make_heartbeat", "make_probe", "make_rider_input", "make_rider_snapshot", "decode_packet", "accept_packet", "connect_rustscale", "send_packet", "query_route", "shutdown"]:
		if not peer_session.has_method(method):
			failures.append("PeerSession lacks %s" % method)
	if not peer_session.has_signal("packet_received") or not peer_session.has_signal("route_updated"):
		failures.append("PeerSession lacks packet or route telemetry signals")
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
			"make_rider_snapshot", 2, Vector3(1, 2, 3), Vector3(4, 0, -2), 45.0, STANCE_DIVE
		) as PackedByteArray
		var decoded := peer_session.call("decode_packet", snapshot) as Dictionary
		if decoded.get("type", "") != "rider_snapshot" or decoded.get("position", Vector3.ZERO) != Vector3(1, 2, 3):
			failures.append("PeerSession snapshot codec omitted rider state")
		if int(decoded.get("stance_id", -1)) != STANCE_DIVE or not bool(decoded.get("stance_known", false)):
			failures.append("PeerSession snapshot codec omitted known stance")
		var bad_stance := peer_session.call(
			"make_rider_snapshot", 3, Vector3.ZERO, Vector3.ZERO, 0.0, 222
		) as PackedByteArray
		if not bad_stance.is_empty():
			failures.append("PeerSession outbound snapshot accepted unknown local stance")
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
	for method in ["equip_weapon", "request_fire", "request_reload", "get_weapon_stats", "resolve_local_hit", "resolve_local_miss", "set_rider_context", "advance_to_tick"]:
		if not controller.has_method(method):
			failures.append("MountedWeaponController lacks %s" % method)
	if not failures.is_empty():
		return
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

	var previous_pose_yaw := pose.rotation.y
	var max_pose_delta := 0.0
	var airborne_frames := 0
	while int(rider.get("stance_id")) == STANCE_DIVE and airborne_frames < 70:
		await get_tree().physics_frame
		var yaw_delta := absf(wrapf(pose.rotation.y - previous_pose_yaw, -PI, PI))
		max_pose_delta = maxf(max_pose_delta, yaw_delta)
		previous_pose_yaw = pose.rotation.y
		airborne_frames += 1
		if not rider.global_position.is_finite() or not camera.global_position.is_finite():
			failures.append("rider/camera transform became nonfinite during dive")
			break
		if camera.fov < 69.99 or camera.fov > 78.01 or absf(camera.global_rotation.z) > 0.001:
			failures.append("M2 introduced camera FOV/roll outside conservative plan")
			break
	if int(rider.get("stance_id")) != STANCE_PRONE:
		failures.append("actual collision arc did not enter landing prone")
	if synchronous_landing_fire != [false]:
		failures.append("landing signal observed an open airborne combat context")
	var airtime := float(rider.get("airtime_seconds"))
	if airtime < 0.7 or airtime > 0.9:
		failures.append("scene Saddle Dive airtime %.3f was outside 0.7-0.9 s" % airtime)
	if max_pose_delta > deg_to_rad(6.05):
		failures.append("airborne visual yaw exceeded 6 degrees per 60 Hz frame")
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
	await get_tree().process_frame
	var remount_hint := course.get_node("HUD/Panel/Margin/VBox/RemountHint") as Label
	if not remount_hint.visible or (not remount_hint.text.begins_with("HORSE READY") and remount_hint.text != "E — REMOUNT"):
		failures.append("retrievable horse lacked a HUD affordance")

	# Test setup moves only the rider into the normal 3 m interaction range; the
	# horse remains where collision-resolved runout stopped.
	rider.global_position = horse.global_position + Vector3(0, 0, 1.0)
	await get_tree().process_frame
	if not remount_hint.visible or remount_hint.text != "E — REMOUNT":
		failures.append("in-range horse lacked the E remount prompt")
	await _pulse_action(&"combat_interact")
	if int(rider.get("stance_id")) != STANCE_MOUNTED:
		failures.append("eligible E remount did not complete")
	if telemetry.is_empty():
		failures.append("per-dive telemetry emitted no rows")
	else:
		var row := telemetry.back() as Dictionary
		for key in ["prelaunch_speed_mps", "requested_angle_degrees", "clamped_angle_degrees", "airtime_ticks", "shots_fired", "shots_hit", "damage_dealt", "landing_terrain", "landing_slope_degrees", "landing_outcome", "time_to_remount_ticks"]:
			if not row.has(key):
				failures.append("dive telemetry omitted %s" % key)
	for expected in ["FLYING DISMOUNT", "SADDLE DIVE HEADSHOT", "FULL-GALLOP HIT", "AIRBORNE REVERSAL"]:
		if expected not in events:
			failures.append("deterministic gameplay event missing: %s" % expected)

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
		horse.velocity = Vector3(0, 0, -9.0)
		await get_tree().physics_frame
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
	await get_tree().physics_frame
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

func _wait_controller_tick(controller: Node, target_tick: int, maximum_frames: int) -> void:
	for _frame in maximum_frames:
		if int(controller.get("current_tick")) >= target_tick:
			return
		await get_tree().physics_frame

func _reset_course_with_input() -> void:
	await _pulse_action(&"reset_horse")

func _press_action_one_tick(action: StringName) -> void:
	Input.action_press(action)
	await get_tree().physics_frame
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
