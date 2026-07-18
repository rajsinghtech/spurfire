extends Node3D
class_name SpurfireCameraRig

@export var target: Node3D
@export var telemetry_source: Node
@export_range(4.5, 6.5, 0.1) var chase_distance := 5.5
@export var pivot_height := 2.2
@export var shoulder_offset := 0.4
@export var mouse_sensitivity := 0.0025
@export var stick_speed := 2.2
@export var teleport_guard_metres := 8.0
@export var teleport_guard_degrees := 120.0
@export var anchor_response := 5.0

const STANCE_ANCHORS := {
	1: 2.2, # mounted
	2: 2.2, # ordinary mounted jump
	3: 1.9, # Saddle Dive airborne
	4: 1.2, # landing prone
	5: 1.6, # recovery
	6: 1.7, # on foot
}

var _orbit_yaw := 0.0
var _pitch := deg_to_rad(-12.0)
var _speed_fraction := 0.0
var _yaw_rate_degs := 0.0
var _stance_id := 1
var _current_anchor := 2.2
var _last_raw_target := Transform3D.IDENTITY
var _has_raw_target := false

@onready var pivot: Node3D = $PitchPivot
@onready var arm: SpringArm3D = $PitchPivot/SpringArm3D
@onready var camera: Camera3D = $PitchPivot/SpringArm3D/Camera3D

func _ready() -> void:
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
	# The rig is already render-driven from an interpolated target; prevent a
	# second interpolation pass on the camera hierarchy itself.
	physics_interpolation_mode = Node.PHYSICS_INTERPOLATION_MODE_OFF
	pivot.physics_interpolation_mode = Node.PHYSICS_INTERPOLATION_MODE_OFF
	arm.physics_interpolation_mode = Node.PHYSICS_INTERPOLATION_MODE_OFF
	camera.physics_interpolation_mode = Node.PHYSICS_INTERPOLATION_MODE_OFF
	arm.spring_length = chase_distance
	camera.position.x = shoulder_offset
	_current_anchor = pivot_height
	if target:
		reset_follow()
	var source := telemetry_source if telemetry_source else target
	if source and source.has_signal(&"telemetry_updated"):
		source.telemetry_updated.connect(_on_telemetry)

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
	if not target or not is_finite(delta) or delta <= 0.0:
		return
	var raw_target := target.global_transform
	if not _transform_is_finite(raw_target):
		return
	if _is_teleport(raw_target):
		# Intentional resets must not blend across unrelated positions. Reset the
		# target history before asking Godot for its render-time transform.
		target.reset_physics_interpolation()
		_snap_to(raw_target)
	var sampled := sample_target_transform()
	if not _transform_is_finite(sampled):
		return
	_last_raw_target = raw_target
	_has_raw_target = true

	var look := Input.get_vector(&"camera_left", &"camera_right", &"camera_up", &"camera_down")
	_orbit_yaw -= look.x * stick_speed * delta
	_pitch = clampf(_pitch - look.y * stick_speed * delta, deg_to_rad(-35.0), deg_to_rad(35.0))
	var wanted_anchor := float(STANCE_ANCHORS.get(_stance_id, pivot_height))
	_current_anchor = lerpf(
		_current_anchor,
		wanted_anchor,
		1.0 - exp(-maxf(anchor_response, 0.01) * delta)
	)
	var sway := clampf(_yaw_rate_degs / 35.0, -1.0, 1.0) * _speed_fraction * 0.3
	var wanted_position := sampled.origin + Vector3.UP * _current_anchor + sampled.basis.x * sway
	global_position = global_position.lerp(wanted_position, 1.0 - exp(-8.0 * delta))
	var wanted_yaw := sampled.basis.get_euler().y + _orbit_yaw
	rotation = Vector3(0.0, lerp_angle(rotation.y, wanted_yaw, 1.0 - exp(-5.0 * delta)), 0.0)
	pivot.rotation.x = _pitch
	arm.spring_length = move_toward(arm.spring_length, chase_distance, 6.0 * delta)
	var wanted_fov := lerpf(70.0, 78.0, clampf(_speed_fraction, 0.0, 1.0))
	camera.fov = move_toward(camera.fov, wanted_fov, 10.0 * delta)

## Godot 4.7 supplies a render-time transform while physics interpolation is
## enabled. The 60 Hz authoritative transform remains untouched.
func sample_target_transform() -> Transform3D:
	if target == null:
		return Transform3D.IDENTITY
	return target.get_global_transform_interpolated()

## Pure sampling seam used by the deterministic 60/120/144 Hz smoke harness.
static func interpolate_render_transform(
	previous: Transform3D,
	current: Transform3D,
	fraction: float
) -> Transform3D:
	return previous.interpolate_with(current, clampf(fraction, 0.0, 1.0))

## Call after an intentional reset/teleport so interpolation history cannot
## smear the rider across the course.
func reset_follow() -> void:
	if target == null:
		return
	target.reset_physics_interpolation()
	var raw_target := target.global_transform
	_last_raw_target = raw_target
	_has_raw_target = true
	_current_anchor = float(STANCE_ANCHORS.get(_stance_id, pivot_height))
	_snap_to(raw_target)

func _snap_to(sampled: Transform3D) -> void:
	global_position = sampled.origin + Vector3.UP * _current_anchor
	rotation = Vector3(0.0, sampled.basis.get_euler().y + _orbit_yaw, 0.0)
	reset_physics_interpolation()

func _is_teleport(raw_target: Transform3D) -> bool:
	if not _has_raw_target:
		return false
	var yaw_delta := absf(wrapf(
		raw_target.basis.get_euler().y - _last_raw_target.basis.get_euler().y,
		-PI,
		PI
	))
	return (
		raw_target.origin.distance_to(_last_raw_target.origin) > teleport_guard_metres
		or yaw_delta > deg_to_rad(teleport_guard_degrees)
	)

static func _transform_is_finite(value: Transform3D) -> bool:
	return value.origin.is_finite() and value.basis.is_finite()

func _on_telemetry(data: Dictionary) -> void:
	_speed_fraction = float(data.get("speed_fraction", 0.0))
	_yaw_rate_degs = float(data.get("yaw_rate_degs", 0.0))
	_stance_id = int(data.get("stance_id", _stance_id))
