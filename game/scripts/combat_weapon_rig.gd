extends Node3D
class_name CombatWeaponRig

const ART_SCENES := [
	preload("res://scenes/art/weapons/dustwalker_art.tscn"),
	preload("res://scenes/art/weapons/longspur_art.tscn"),
	preload("res://scenes/art/weapons/rattler_art.tscn"),
]

@export var controller: Node
@export var aim_source: Node3D
@export var muzzle: Marker3D
@export_range(0, 2, 1) var weapon_id := 0
@export var tracer_color := Color("ffb84d")
@export var identity_color := Color("b88752")
@export var stock_color := Color("6f3e24")
@onready var flash: MeshInstance3D = %MuzzleFlash

var _flash_frames := 0
var _tick := 0
var _visual_kick_degrees := 0.0
var _base_rotation_x := 0.0

func _ready() -> void:
	_base_rotation_x = rotation.x
	flash.visible = false
	_apply_identity()
	_install_verified_art()
	if controller != null:
		bind_controller(controller)

func bind_controller(value: Node) -> void:
	controller = value
	if controller != null and controller.has_signal(&"shot_fired") and not controller.is_connected(&"shot_fired", _on_shot_fired):
		controller.connect(&"shot_fired", _on_shot_fired)

func equip() -> void:
	if controller != null and controller.has_method("equip_weapon"):
		controller.call("equip_weapon", weapon_id)

func set_weapon(id: int) -> void:
	weapon_id = clampi(id, 0, 2)
	_install_verified_art()
	equip()

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

func _process(delta: float) -> void:
	_visual_kick_degrees = move_toward(_visual_kick_degrees, 0.0, 100.0 * delta)
	rotation.x = _base_rotation_x - deg_to_rad(_visual_kick_degrees)

func _physics_process(_delta: float) -> void:
	if _flash_frames > 0:
		_flash_frames -= 1
		flash.visible = _flash_frames > 0

func _on_shot_fired(_shot_tick: Variant, fired_weapon_id: Variant) -> void:
	if int(fired_weapon_id) != weapon_id:
		return
	flash.visible = true
	_flash_frames = 2
	_visual_kick_degrees = 6.0
	_spawn_gunsmoke()

func _install_verified_art() -> void:
	var fallback_muzzle := get_node_or_null("Muzzle") as Marker3D
	if flash.get_parent() != fallback_muzzle:
		flash.reparent(fallback_muzzle, false)
	var previous := get_node_or_null("WeaponArt")
	if previous:
		remove_child(previous)
		previous.queue_free()
	for path in ["Receiver", "Stock", "Barrel", "Grip"]:
		var fallback := get_node_or_null(path) as VisualInstance3D
		if fallback:
			fallback.visible = false
	var art := ART_SCENES[clampi(weapon_id, 0, 2)].instantiate() as Node3D
	art.name = "WeaponArt"
	add_child(art)
	muzzle = art.get_node("Muzzle") as Marker3D
	flash.reparent(muzzle, false)
	flash.position = Vector3.ZERO

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
	var band := MeshInstance3D.new()
	band.name = "AccentReceiverBand"
	band.position = Vector3(0.0, 0.05, -0.18)
	var band_mesh := BoxMesh.new()
	band_mesh.size = Vector3(0.205, 0.075, 0.16)
	var band_material := StandardMaterial3D.new()
	band_material.albedo_color = identity_color
	band_material.roughness = 0.72
	band_mesh.material = band_material
	band.mesh = band_mesh
	add_child(band)

func _spawn_gunsmoke() -> void:
	var previous := muzzle.get_node_or_null("Gunsmoke")
	if previous:
		previous.queue_free()
	var puff := Node3D.new()
	puff.name = "Gunsmoke"
	muzzle.add_child(puff)
	var material := StandardMaterial3D.new()
	material.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
	material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	material.albedo_color = Color(0.82, 0.75, 0.65, 0.42)
	for index in 3:
		var quad := MeshInstance3D.new()
		var mesh := QuadMesh.new()
		mesh.size = Vector2(0.18 + float(index) * 0.08, 0.18 + float(index) * 0.08)
		mesh.material = material
		quad.mesh = mesh
		quad.position = Vector3(float(index - 1) * 0.08, float(index) * 0.05, -float(index) * 0.05)
		quad.rotation.y = float(index) * 1.05
		puff.add_child(quad)
	var tween := create_tween()
	tween.set_parallel(true)
	tween.tween_property(puff, "position", Vector3(0.18, 0.3, 0.08), 0.5)
	tween.tween_property(puff, "scale", Vector3.ONE * 1.8, 0.5)
	tween.tween_property(material, "albedo_color:a", 0.0, 0.5)
	tween.chain().tween_callback(puff.queue_free)
