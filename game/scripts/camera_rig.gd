extends Node3D

@export var target: Node3D
@export_range(4.5, 6.5, 0.1) var chase_distance := 5.5
@export var pivot_height := 2.2
@export var shoulder_offset := 0.4
@export var mouse_sensitivity := 0.0025
@export var stick_speed := 2.2

var _orbit_yaw := 0.0
var _pitch := deg_to_rad(-12.0)
var _speed_fraction := 0.0
var _yaw_rate_degs := 0.0

@onready var pivot: Node3D = $PitchPivot
@onready var arm: SpringArm3D = $PitchPivot/SpringArm3D
@onready var camera: Camera3D = $PitchPivot/SpringArm3D/Camera3D

func _ready() -> void:
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
	arm.spring_length = chase_distance
	camera.position.x = shoulder_offset
	if target:
		global_position = target.global_position + Vector3.UP * pivot_height
		rotation.y = target.global_rotation.y
		if target.has_signal("telemetry_updated"):
			target.telemetry_updated.connect(_on_telemetry)

func _unhandled_input(event: InputEvent) -> void:
	if event.is_action_pressed(&"release_mouse"):
		if Input.mouse_mode == Input.MOUSE_MODE_CAPTURED:
			Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
		else:
			get_tree().quit()
		get_viewport().set_input_as_handled()
		return
	if event is InputEventMouseMotion and Input.mouse_mode == Input.MOUSE_MODE_CAPTURED:
		_orbit_yaw -= event.relative.x * mouse_sensitivity
		_pitch = clampf(_pitch - event.relative.y * mouse_sensitivity, deg_to_rad(-35.0), deg_to_rad(35.0))
	elif event is InputEventMouseButton and event.button_index == MOUSE_BUTTON_LEFT and event.pressed:
		Input.mouse_mode = Input.MOUSE_MODE_CAPTURED

func _process(delta: float) -> void:
	if not target:
		return
	var look := Input.get_vector(&"camera_left", &"camera_right", &"camera_up", &"camera_down")
	_orbit_yaw -= look.x * stick_speed * delta
	_pitch = clampf(_pitch - look.y * stick_speed * delta, deg_to_rad(-35.0), deg_to_rad(35.0))
	var sway := clampf(_yaw_rate_degs / 35.0, -1.0, 1.0) * _speed_fraction * 0.3
	var wanted_position := target.global_position + Vector3.UP * pivot_height + target.global_basis.x * sway
	global_position = global_position.lerp(wanted_position, 1.0 - exp(-8.0 * delta))
	var wanted_yaw := target.global_rotation.y + _orbit_yaw
	rotation = Vector3(0.0, lerp_angle(rotation.y, wanted_yaw, 1.0 - exp(-5.0 * delta)), 0.0)
	pivot.rotation.x = _pitch
	arm.spring_length = move_toward(arm.spring_length, chase_distance, 6.0 * delta)
	var wanted_fov := lerpf(70.0, 78.0, clampf(_speed_fraction, 0.0, 1.0))
	camera.fov = move_toward(camera.fov, wanted_fov, 10.0 * delta)

func _on_telemetry(data: Dictionary) -> void:
	_speed_fraction = float(data.get("speed_fraction", 0.0))
	_yaw_rate_degs = float(data.get("yaw_rate_degs", 0.0))
