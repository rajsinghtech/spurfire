extends Node3D
class_name CombatTargetDummy

@export var target_id := 1
@export var max_vitality := 100.0
@export var controller: Node
@onready var body_mesh: MeshInstance3D = %BodyMesh
@onready var head_mesh: MeshInstance3D = %HeadMesh
@onready var vitality_bar: Label3D = %Vitality

var vitality := 100.0
var _flash_remaining := 0.0
var _respawn_remaining := 0.0
var _base_rotation := Vector3.ZERO

func _ready() -> void:
	vitality = max_vitality
	_update_vitality_label()
	_base_rotation = rotation
	%BodyZone.set_meta("target_id", target_id)
	%HeadZone.set_meta("target_id", target_id)
	if controller != null:
		bind_controller(controller)

func bind_controller(value: Node) -> void:
	controller = value
	if controller != null and controller.has_signal(&"hit_confirmed") and not controller.is_connected(&"hit_confirmed", _on_hit_confirmed):
		controller.connect(&"hit_confirmed", _on_hit_confirmed)

# Cosmetic mirror only: damage is accepted exclusively from the native controller's result signal.
func _on_hit_confirmed(hit_target_id: Variant, hit_zone: Variant, damage: Variant) -> void:
	if int(hit_target_id) != target_id or _respawn_remaining > 0.0:
		return
	vitality = maxf(0.0, vitality - float(damage))
	_update_vitality_label()
	_flash_remaining = 0.12
	_set_emission(Color("ffd166") if str(hit_zone).to_lower() == "head" else Color.WHITE)
	if vitality <= 0.0:
		rotation.z = deg_to_rad(78.0)
		_respawn_remaining = 5.0

func _process(delta: float) -> void:
	if _flash_remaining > 0.0:
		_flash_remaining -= delta
		if _flash_remaining <= 0.0:
			_set_emission(Color.BLACK)
	if _respawn_remaining > 0.0:
		_respawn_remaining -= delta
		if _respawn_remaining <= 0.0:
			vitality = max_vitality
			_update_vitality_label()
			rotation = _base_rotation
			visible = true

func _update_vitality_label() -> void:
	vitality_bar.text = "%d / %d" % [ceili(vitality), ceili(max_vitality)]

func _set_emission(color: Color) -> void:
	for mesh_instance in [body_mesh, head_mesh]:
		var material := mesh_instance.get_active_material(0) as StandardMaterial3D
		if material != null:
			material.emission_enabled = color != Color.BLACK
			material.emission = color
			material.emission_energy_multiplier = 1.8
