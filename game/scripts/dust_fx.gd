extends Node3D
class_name SpurfireDustFx

@export var horse: Node3D
@export var rider: Node3D
@export var stride_metres := 1.4

var _speed_fraction := 0.0
var _last_position := Vector3.ZERO
var _distance_since_puff := 0.0
var _was_airborne := false

@onready var hoof_dust: GPUParticles3D = $HoofDust
@onready var landing_dust: GPUParticles3D = $LandingDust

func _ready() -> void:
	if horse:
		_last_position = horse.global_position
		if horse.has_signal(&"telemetry_updated"):
			horse.telemetry_updated.connect(_on_telemetry)
	if rider and rider.has_signal(&"dive_landed"):
		rider.dive_landed.connect(_on_dive_landed)

func _process(_delta: float) -> void:
	if horse == null:
		return
	global_position = horse.global_position + Vector3(0, 0.08, 0.55)
	var distance := Vector2(horse.global_position.x - _last_position.x, horse.global_position.z - _last_position.z).length()
	_last_position = horse.global_position
	_distance_since_puff += distance
	if _speed_fraction > 0.18 and _distance_since_puff >= stride_metres:
		_distance_since_puff = fmod(_distance_since_puff, stride_metres)
		hoof_dust.restart()
		hoof_dust.emitting = true

func _on_telemetry(data: Dictionary) -> void:
	_speed_fraction = clampf(float(data.get("speed_fraction", 0.0)), 0.0, 1.0)
	var airborne := bool(data.get("is_airborne", false))
	if _was_airborne and not airborne:
		_burst_landing()
	_was_airborne = airborne

func _on_dive_landed(_dive_id: int, _bad: bool, _slope: float, _terrain: String) -> void:
	_burst_landing()

func _burst_landing() -> void:
	landing_dust.global_position = rider.global_position if rider else global_position
	landing_dust.restart()
	landing_dust.emitting = true
