extends Node
class_name CombatPlayer

signal reload_input_result(accepted: bool, reason: String)
signal network_shot_command(tick: int, command_json: String)

var networked_match := false
var _presentation_input_enabled := false
var _capture_button_blocked := false

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
	if _capture_button_blocked and not Input.is_action_pressed(&"combat_fire"):
		_capture_button_blocked = false
	if not _presentation_input_enabled or _capture_button_blocked:
		return
	if Input.is_action_pressed(&"combat_fire"):
		_fire_once(tick)
	if Input.is_action_just_pressed(&"combat_reload") and rifle:
		var accepted := bool(rifle.call("request_reload"))
		var reason := "" if accepted or controller == null else str(controller.get("last_reject_reason"))
		reload_input_result.emit(accepted, reason)
		if (
			not accepted
			and controller != null
			and not controller.has_signal(&"reload_rejected")
			and combat_hud
			and combat_hud.has_method("show_reload_rejection")
		):
			combat_hud.call("show_reload_rejection", reason)
	if Input.is_action_just_pressed(&"weapon_dustwalker"):
		_select_weapon(0)
	elif Input.is_action_just_pressed(&"weapon_longspur"):
		_select_weapon(1)
	elif Input.is_action_just_pressed(&"weapon_rattler"):
		_select_weapon(2)

func set_networked_match(enabled: bool) -> void:
	networked_match = enabled

func set_presentation_input_enabled(enabled: bool, suppress_button := false) -> void:
	_presentation_input_enabled = enabled
	_capture_button_blocked = enabled and suppress_button

func apply_network_shot_result(payload: Dictionary) -> void:
	if controller and controller.has_method("apply_authority_result_json"):
		controller.call("apply_authority_result_json", str(payload.get("result_json", "")))

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
	var command_json := ""
	if networked_match and controller.has_method("take_pending_shot_command_json"):
		command_json = str(controller.call("take_pending_shot_command_json"))
		if command_json.is_empty():
			return
		network_shot_command.emit(tick, command_json)
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
		if not networked_match and collider and collider.has_meta("target_id") and collider.has_meta("hit_zone"):
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
	if not networked_match and not resolved_hit and controller.has_method("resolve_local_miss"):
		controller.call("resolve_local_miss")
	if effects and effects.has_method("show_tracer"):
		effects.call("show_tracer", origin, endpoint, rifle.get("tracer_color") as Color)
