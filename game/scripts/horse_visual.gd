extends Node

@export var leg_swing_degrees := 24.0
@export var body_bob_metres := 0.08
@export var sidestep_lean_degrees := 6.0

const COAT_COLORS := [Color("c99755"), Color("5e4438"), Color("9b5d35")]

var _speed_fraction := 0.0
var _lateral_fraction := 0.0
var _archetype := 2
var _phase := 0.0
var _body_base_y := 0.0
var _chest_base_y := 0.0
var _coat_material: StandardMaterial3D

@onready var horse: Node3D = get_parent()
@onready var body: MeshInstance3D = horse.get_node("BodyProxy")
@onready var chest: MeshInstance3D = horse.get_node("ChestProxy")
@onready var front_left: MeshInstance3D = horse.get_node("FrontLeftLeg")
@onready var front_right: MeshInstance3D = horse.get_node("FrontRightLeg")
@onready var rear_left: MeshInstance3D = horse.get_node("RearLeftLeg")
@onready var rear_right: MeshInstance3D = horse.get_node("RearRightLeg")

func _ready() -> void:
	_body_base_y = body.position.y
	_chest_base_y = chest.position.y
	_coat_material = StandardMaterial3D.new()
	_coat_material.roughness = 0.85
	for node_name in ["BodyProxy", "ChestProxy", "NeckProxy", "HeadProxy", "LeftEar", "RightEar", "FrontLeftLeg", "FrontRightLeg", "RearLeftLeg", "RearRightLeg", "TailProxy", "ForwardMarker"]:
		var mesh := horse.get_node_or_null(node_name) as MeshInstance3D
		if mesh:
			mesh.material_override = _coat_material
	_apply_archetype_color()
	if horse.has_signal("telemetry_updated"):
		horse.telemetry_updated.connect(_on_telemetry)

func _process(delta: float) -> void:
	var cadence: float = lerpf(2.0, 10.0, _speed_fraction)
	_phase = fmod(_phase + delta * cadence, TAU)
	var amplitude: float = deg_to_rad(leg_swing_degrees) * maxf(_speed_fraction, absf(_lateral_fraction) * 0.45)
	var diagonal_a: float = sin(_phase) * amplitude
	var diagonal_b: float = sin(_phase + PI) * amplitude
	front_left.rotation.x = diagonal_a
	rear_right.rotation.x = diagonal_a
	front_right.rotation.x = diagonal_b
	rear_left.rotation.x = diagonal_b
	var bob: float = absf(sin(_phase * 2.0)) * body_bob_metres * _speed_fraction
	body.position.y = _body_base_y + bob
	chest.position.y = _chest_base_y + bob * 0.8
	var target_lean := deg_to_rad(-sidestep_lean_degrees * _lateral_fraction)
	body.rotation.z = lerp_angle(body.rotation.z, target_lean, 1.0 - exp(-10.0 * delta))
	chest.rotation.z = lerp_angle(chest.rotation.z, target_lean * 0.8, 1.0 - exp(-10.0 * delta))

func _on_telemetry(data: Dictionary) -> void:
	_speed_fraction = clampf(float(data.get("speed_fraction", 0.0)), 0.0, 1.0)
	_lateral_fraction = clampf(float(data.get("lateral_speed_mps", 0.0)) / 1.2, -1.0, 1.0)
	var next_archetype := clampi(int(data.get("archetype", 2)), 0, 2)
	if next_archetype != _archetype:
		_archetype = next_archetype
		_apply_archetype_color()

func _apply_archetype_color() -> void:
	if _coat_material:
		_coat_material.albedo_color = COAT_COLORS[_archetype]
