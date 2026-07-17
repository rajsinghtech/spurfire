extends Node

@export var leg_swing_degrees := 24.0
@export var body_bob_metres := 0.08

var _speed_fraction := 0.0
var _gait := 0
var _phase := 0.0
var _body_base_y := 0.0
var _chest_base_y := 0.0

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
	if horse.has_signal("telemetry_updated"):
		horse.telemetry_updated.connect(_on_telemetry)

func _process(delta: float) -> void:
	var cadence: float = lerpf(2.0, 10.0, _speed_fraction)
	_phase = fmod(_phase + delta * cadence, TAU)
	var amplitude: float = deg_to_rad(leg_swing_degrees) * _speed_fraction
	var diagonal_a: float = sin(_phase) * amplitude
	var diagonal_b: float = sin(_phase + PI) * amplitude
	front_left.rotation.x = diagonal_a
	rear_right.rotation.x = diagonal_a
	front_right.rotation.x = diagonal_b
	rear_left.rotation.x = diagonal_b
	var bob: float = absf(sin(_phase * 2.0)) * body_bob_metres * _speed_fraction
	body.position.y = _body_base_y + bob
	chest.position.y = _chest_base_y + bob * 0.8

func _on_telemetry(data: Dictionary) -> void:
	_speed_fraction = clampf(float(data.get("speed_fraction", 0.0)), 0.0, 1.0)
	_gait = int(data.get("gait", 0))
