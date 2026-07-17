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
	"Sun", "KillResetZone", "HorseSpawn", "HUD"
]

func _ready() -> void:
	var failures: Array[String] = []
	for action in REQUIRED_ACTIONS:
		if not InputMap.has_action(action):
			failures.append("missing InputMap action: %s" % action)
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

func _wait_physics_frames(count: int) -> void:
	for _frame in range(count):
		await get_tree().physics_frame

func _finish(failures: Array[String]) -> void:
	Input.action_release(&"move_forward")
	Input.action_release(&"steer_right")
	if failures.is_empty():
		print("SPURFIRE_GODOT_SMOKE_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error("SMOKE: " + failure)
		get_tree().quit(1)
