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
			var horse := course.get_node_or_null("Horse")
			if horse and not horse.has_method("reset_horse"):
				failures.append("HorseController lacks reset_horse()")
			if horse and not horse.has_signal("gait_changed"):
				failures.append("HorseController lacks gait_changed signal")
			if horse and not horse.has_signal("telemetry_updated"):
				failures.append("HorseController lacks telemetry_updated signal")
	if failures.is_empty():
		print("SPURFIRE_GODOT_SMOKE_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error("SMOKE: " + failure)
		get_tree().quit(1)
