extends Node
class_name M2Gameplay

signal simulation_tick_advanced(tick: int)

@export var rider: CharacterBody3D
@export var horse: CharacterBody3D
@export var aim_camera: Camera3D
@export var weapon_controller: Node3D
@export var combat_input: Node
@export var replication: Node

var simulation_tick := 0
var _last_stance_id := 1
var _stance_changed_this_tick := false

func _ready() -> void:
	process_physics_priority = -100
	if weapon_controller:
		weapon_controller.shot_accepted.connect(_on_shot_accepted)
		weapon_controller.shot_resolved.connect(_on_shot_resolved)
	if rider:
		_last_stance_id = int(rider.get("stance_id"))
		rider.stance_changed.connect(_on_stance_changed)

func _physics_process(_delta: float) -> void:
	if rider == null or horse == null or weapon_controller == null:
		return
	simulation_tick += 1
	_stance_changed_this_tick = false

	if Input.is_action_just_pressed(&"reset_horse"):
		rider.call("reset_rider", simulation_tick)
		horse.call("set_external_simulation_tick", simulation_tick)
		_advance_replication(true)
		simulation_tick_advanced.emit(simulation_tick)
		return

	var chosen_direction := -aim_camera.global_basis.z if aim_camera else -rider.global_basis.z
	var move_input := Vector2(
		Input.get_axis(&"steer_left", &"steer_right"),
		Input.get_axis(&"move_back", &"move_forward")
	)
	var weapon_id := int(weapon_controller.get("weapon_id"))
	rider.call(
		"advance_tick",
		simulation_tick,
		Input.is_action_just_pressed(&"combat_interact"),
		chosen_direction,
		move_input,
		weapon_id
	)
	_install_combat_context()
	if combat_input and combat_input.has_method("process_combat_tick"):
		combat_input.call("process_combat_tick", simulation_tick)

	# Collision feedback is intentionally after fire/reload. This preserves the
	# locked pre-contact shot / post-contact recovery ordering.
	horse.call("set_external_simulation_tick", simulation_tick)
	rider.call("resolve_motion", simulation_tick)
	var final_stance := int(rider.get("stance_id"))
	if final_stance != _last_stance_id:
		_stance_changed_this_tick = true
		_last_stance_id = final_stance
	_advance_replication(_stance_changed_this_tick)
	simulation_tick_advanced.emit(simulation_tick)

func _install_combat_context() -> void:
	var stats := horse.call("get_archetype_stats") as Dictionary
	weapon_controller.call(
		"set_rider_context",
		simulation_tick,
		int(rider.get("stance_id")),
		int(rider.get("dive_id")),
		int(horse.get("current_gait")),
		float(horse.get("speed_mps")),
		float(stats.get("gallop_mps", 13.0)),
		float(horse.get("yaw_rate_degrees")),
		false,
		Input.is_action_pressed(&"combat_aim"),
		false
	)

func _advance_replication(stance_changed: bool) -> void:
	if replication and replication.has_method("advance_shared_tick"):
		replication.call("advance_shared_tick", simulation_tick, stance_changed)

func _on_stance_changed(_previous_id: int, current_id: int, _tick: int, _dive_id: int) -> void:
	if current_id != _last_stance_id:
		_stance_changed_this_tick = true

func _on_shot_accepted(tick: int, weapon_id: int, accepted_shot_index: int, _dive_id: int) -> void:
	rider.call("record_accepted_shot", tick, weapon_id, accepted_shot_index)

func _on_shot_resolved(
	tick: int,
	outcome: String,
	hit_zone: String,
	damage: int,
	resolved_direction: Vector3
) -> void:
	rider.call("record_authority_result", {
		"tick": tick,
		"outcome": outcome,
		"hit_zone": hit_zone,
		"damage": damage,
		"target_id": -1,
		"resolved_direction": resolved_direction,
	})
