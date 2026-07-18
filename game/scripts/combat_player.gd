extends Node
class_name CombatPlayer

@export var rider: CharacterBody3D
@export var horse: CharacterBody3D
@export var controller: Node3D
@export var rifle: Node3D
@export var aim_camera: Camera3D
@export var effects: Node3D
@export var combat_hud: Control

func _ready() -> void:
	if rifle and rifle.has_method("bind_controller"):
		rifle.call("bind_controller", controller)
		rifle.set("aim_source", aim_camera)
		rifle.call("equip")
	if combat_hud and combat_hud.has_method("bind_controller"):
		# CombatInput may become ready before the later CanvasLayer sibling.
		combat_hud.call_deferred("bind_controller", controller)

## Called exactly once by M2Gameplay after movement context is installed for the
## shared absolute tick and before rider/horse collision resolution.
func process_combat_tick(tick: int) -> void:
	if Input.is_action_pressed(&"combat_fire"):
		_fire_once(tick)
	if Input.is_action_just_pressed(&"combat_reload") and rifle:
		rifle.call("request_reload")
	if Input.is_action_just_pressed(&"weapon_dustwalker"):
		_select_weapon(0)
	elif Input.is_action_just_pressed(&"weapon_longspur"):
		_select_weapon(1)
	elif Input.is_action_just_pressed(&"weapon_rattler"):
		_select_weapon(2)

func _select_weapon(id: int) -> void:
	if rifle and rifle.has_method("set_weapon"):
		rifle.call("set_weapon", id)

func _fire_once(tick: int) -> void:
	if controller == null or rifle == null or aim_camera == null or rider == null:
		return
	if not bool(rifle.call("request_fire", tick)):
		return
	var origin := controller.get("last_shot_origin") as Vector3
	var direction := (controller.get("last_shot_direction") as Vector3).normalized()
	var stats := controller.call("get_weapon_stats") as Dictionary
	var max_range := float(stats.get("hitscan_clamp_m", 120.0))
	var endpoint := origin + direction * max_range
	var query := PhysicsRayQueryParameters3D.create(origin, endpoint)
	query.collide_with_areas = true
	query.collide_with_bodies = true
	var excluded: Array[RID] = [rider.get_rid()]
	if horse:
		excluded.append(horse.get_rid())
	query.exclude = excluded
	var hit := rider.get_world_3d().direct_space_state.intersect_ray(query)
	var resolved_hit := false
	if not hit.is_empty():
		endpoint = hit.position
		var collider := hit.collider as Object
		if collider and collider.has_meta("target_id") and collider.has_meta("hit_zone"):
			var distance := origin.distance_to(endpoint)
			resolved_hit = bool(controller.call(
				"resolve_local_hit",
				int(collider.get_meta("target_id")),
				String(collider.get_meta("hit_zone")),
				distance
			))
		if effects and effects.has_method("show_impact"):
			effects.call(
				"show_impact",
				endpoint,
				hit.normal,
				String(collider.get_meta("hit_zone", "body")) == "head" if collider else false
			)
	if not resolved_hit and controller.has_method("resolve_local_miss"):
		controller.call("resolve_local_miss")
	if effects and effects.has_method("show_tracer"):
		effects.call("show_tracer", origin, endpoint)
