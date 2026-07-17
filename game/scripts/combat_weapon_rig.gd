extends Node3D
class_name CombatWeaponRig

@export var controller: Node
@export var aim_source: Node3D
@export var muzzle: Marker3D
@export var weapon_id: StringName = &"dustwalker"
@export var tracer_color := Color("ffb84d")
@export var identity_color := Color("b88752")
@export var stock_color := Color("6f3e24")
@onready var flash: MeshInstance3D = %MuzzleFlash

var _flash_frames := 0
var _tick := 0

func _ready() -> void:
	flash.visible = false
	_apply_identity()
	if muzzle == null:
		muzzle = %Muzzle
	if controller != null:
		bind_controller(controller)

func bind_controller(value: Node) -> void:
	controller = value
	if controller != null and controller.has_signal(&"shot_fired") and not controller.is_connected(&"shot_fired", _on_shot_fired):
		controller.connect(&"shot_fired", _on_shot_fired)

func equip() -> void:
	if controller != null and controller.has_method("equip_weapon"):
		controller.call("equip_weapon", weapon_id)

func request_fire(tick: int = -1) -> Variant:
	if controller == null or not controller.has_method("request_fire"):
		return false
	_tick = tick if tick >= 0 else Engine.get_physics_frames()
	var source := aim_source if aim_source != null else self
	var direction := -source.global_basis.z.normalized()
	return controller.call("request_fire", muzzle.global_position, direction, _tick)

func request_reload() -> Variant:
	if controller != null and controller.has_method("request_reload"):
		return controller.call("request_reload")
	return false

func _physics_process(_delta: float) -> void:
	if _flash_frames > 0:
		_flash_frames -= 1
		flash.visible = _flash_frames > 0

func _on_shot_fired(_shot_tick: Variant, fired_weapon_id: Variant) -> void:
	if str(fired_weapon_id).to_lower() != str(weapon_id).to_lower():
		return
	flash.visible = true
	_flash_frames = 2

func _apply_identity() -> void:
	for path in ["Receiver", "Stock"]:
		var part := get_node_or_null(path) as MeshInstance3D
		if part == null:
			continue
		var material := part.get_active_material(0) as StandardMaterial3D
		if material != null:
			material = material.duplicate() as StandardMaterial3D
			material.albedo_color = identity_color if path == "Receiver" else stock_color
			part.set_surface_override_material(0, material)
