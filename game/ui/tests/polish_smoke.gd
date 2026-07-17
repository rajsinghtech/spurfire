extends Node

class FakeHorse:
	extends Node
	signal archetype_changed(old: int, new: int)
	var archetype := 0
	var calls: Array[int] = []

	func set_archetype(id: int) -> void:
		if id < 0 or id > 2:
			return
		var old := archetype
		archetype = id
		calls.append(id)
		archetype_changed.emit(old, id)

	func get_archetype_stats() -> Dictionary:
		return {"walk_mps": 1.8, "trot_mps": 4.2, "gallop_mps": 12.0, "sprint_mps": 13.5, "accel_0_to_gallop_s": 5.0, "turn_walk_deg_s": 120.0, "turn_gallop_deg_s": 45.0, "drift_deg_s": 90.0, "jump_apex_m": 1.2, "jump_airtime_s": 0.5, "terrain_scrub": 0.9, "terrain_mud": 0.75, "terrain_riverbed": 0.95, "terrain_recovery_s": 2.5, "max_vitality": 320.0, "stagger_threshold": 90.0, "sidestep_mps": 0.8, "sidestep_ramp_s": 0.35}

func _ready() -> void:
	var failures: Array[String] = []
	var fake := FakeHorse.new()
	add_child(fake)

	var selector_scene := load("res://ui/archetype_selector.tscn") as PackedScene
	var selector := selector_scene.instantiate()
	selector.horse = fake
	add_child(selector)
	if not selector.has_method("select_archetype") or not selector.has_method("set_horse"):
		failures.append("selector required methods missing")
	selector.select_archetype(1)
	if fake.archetype != 1 or fake.calls != [1]:
		failures.append("selector did not call set_archetype(1) exactly once")
	for required_path in ["%CourserButton", "%WarhorseButton", "%MustangButton", "%AttributePanel"]:
		if selector.get_node_or_null(required_path) == null:
			failures.append("selector missing node %s" % required_path)

	var panel := selector.get_node("%AttributePanel")
	if not panel.has_method("set_archetype") or not panel.has_method("set_exact_stats_visible"):
		failures.append("attribute panel required methods missing")
	for required_path in ["%SpeedBar", "%AccelerationBar", "%TurningBar", "%JumpBar", "%VitalityBar", "%Description"]:
		if panel.get_node_or_null(required_path) == null:
			failures.append("attribute panel missing node %s" % required_path)
	panel.set_archetype(2)
	if panel.archetype_id != 2:
		failures.append("attribute panel did not switch to Mustang")

	var feedback_scene := load("res://ui/stylized_feedback.tscn") as PackedScene
	var feedback := feedback_scene.instantiate()
	add_child(feedback)
	if not feedback.has_method("apply_telemetry") or not feedback.has_method("set_horse"):
		failures.append("feedback required methods missing")
	feedback.apply_telemetry({"speed_mps": 14.0, "sprint_mps": 16.5, "lateral_speed_mps": 0.0, "surface": "scrub", "is_airborne": false})

	if failures.is_empty():
		print("SPURFIRE_POLISH_SMOKE_OK")
		get_tree().quit(0)
	else:
		for failure in failures:
			push_error("POLISH: " + failure)
		get_tree().quit(1)
