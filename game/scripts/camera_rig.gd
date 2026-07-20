extends Node3D
class_name SpurfireCameraRig

signal capture_state_changed(captured: bool)
signal mouse_delta_sampled(delta: Vector2)

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

const STANCE_MOUNTED := 1
const STANCE_ANCHORS := {
	1: 2.2,
	2: 2.2,
	3: 1.9,
	4: 1.2,
	5: 1.6,
	6: 1.7,
}

var _world_yaw := 0.0
var _pitch := deg_to_rad(-12.0)
var _speed_fraction := 0.0
var _yaw_rate_degs := 0.0
var _stance_id := STANCE_MOUNTED
var _current_anchor := 2.2
var _last_raw_target := Transform3D.IDENTITY
var _has_raw_target := false
var _frame_mouse_delta := Vector2.ZERO
var _last_reported_capture := false
var _capture_active := false
var _telemetry_elapsed := 0.0
var _presentation_log: FileAccess

@onready var pivot: Node3D = $PitchPivot
@onready var arm: SpringArm3D = $PitchPivot/SpringArm3D
@onready var camera: Camera3D = $PitchPivot/SpringArm3D/Camera3D

func _ready() -> void:
	# Capture is deliberately user-gesture gated by CaptureGate. This remains
	# reliable after focus/Gatekeeper transitions in packaged desktop builds.
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	physics_interpolation_mode = Node.PHYSICS_INTERPOLATION_MODE_OFF
	pivot.physics_interpolation_mode = Node.PHYSICS_INTERPOLATION_MODE_OFF
	arm.physics_interpolation_mode = Node.PHYSICS_INTERPOLATION_MODE_OFF
	camera.physics_interpolation_mode = Node.PHYSICS_INTERPOLATION_MODE_OFF
	arm.spring_length = chase_distance
	camera.position.x = shoulder_offset
	_current_anchor = pivot_height
	if target:
		_world_yaw = target.global_basis.get_euler().y
		reset_follow()
	var source := telemetry_source if telemetry_source else target
	if source and source.has_signal(&"telemetry_updated"):
		source.telemetry_updated.connect(_on_telemetry)
	_open_presentation_log()
	_emit_capture_if_changed(true)

func _input(event: InputEvent) -> void:
	if event.is_action_pressed(&"release_mouse"):
		if is_captured():
			release_capture("escape")
		get_viewport().set_input_as_handled()
		return
	if event is InputEventMouseMotion and is_captured():
		var motion := event as InputEventMouseMotion
		_world_yaw = wrapf(_world_yaw - motion.relative.x * mouse_sensitivity, -PI, PI)
		_pitch = clampf(
			_pitch - motion.relative.y * mouse_sensitivity,
			deg_to_rad(-35.0),
			deg_to_rad(35.0)
		)
		_frame_mouse_delta += motion.relative
		mouse_delta_sampled.emit(motion.relative)

func request_capture() -> void:
	Input.mouse_mode = Input.MOUSE_MODE_CAPTURED
	_capture_active = true
	_emit_capture_if_changed(true)

func release_capture(reason := "release") -> void:
	Input.mouse_mode = Input.MOUSE_MODE_VISIBLE
	_capture_active = false
	_emit_capture_if_changed(true)
	_store_presentation_record({"event": reason})

func is_captured() -> bool:
	return _capture_active

func _notification(what: int) -> void:
	if what == NOTIFICATION_APPLICATION_FOCUS_OUT and is_captured():
		release_capture("focus_out")
	elif what == NOTIFICATION_PREDELETE and _presentation_log:
		_presentation_log.close()
		_presentation_log = null

func _process(delta: float) -> void:
	_emit_capture_if_changed()
	if not target or not is_finite(delta) or delta <= 0.0:
		return
	var raw_target := target.global_transform
	if not _transform_is_finite(raw_target):
		return
	if _is_teleport(raw_target):
		target.reset_physics_interpolation()
		_snap_to(raw_target)
	var sampled := sample_target_transform()
	if not _transform_is_finite(sampled):
		return
	_last_raw_target = raw_target
	_has_raw_target = true

	# Camera orientation is exclusively player-controlled. Never rotate the
	# view toward horse heading; doing so can move mounted aim unexpectedly.
	if is_captured():
		var look := Input.get_vector(&"camera_left", &"camera_right", &"camera_up", &"camera_down")
		if look.length_squared() > 0.0001:
			_world_yaw = wrapf(_world_yaw - look.x * stick_speed * delta, -PI, PI)
			_pitch = clampf(_pitch - look.y * stick_speed * delta, deg_to_rad(-35.0), deg_to_rad(35.0))

	var wanted_anchor := float(STANCE_ANCHORS.get(_stance_id, pivot_height))
	_current_anchor = lerpf(_current_anchor, wanted_anchor, 1.0 - exp(-maxf(anchor_response, 0.01) * delta))
	var sway := clampf(_yaw_rate_degs / 35.0, -1.0, 1.0) * _speed_fraction * 0.3
	var wanted_position := sampled.origin + Vector3.UP * _current_anchor + sampled.basis.x * sway
	global_position = global_position.lerp(wanted_position, 1.0 - exp(-8.0 * delta))
	rotation = Vector3(0.0, lerp_angle(rotation.y, _world_yaw, 1.0 - exp(-5.0 * delta)), 0.0)
	pivot.rotation.x = _pitch
	arm.spring_length = move_toward(arm.spring_length, chase_distance, 6.0 * delta)
	var wanted_fov := lerpf(70.0, 78.0, clampf(_speed_fraction, 0.0, 1.0))
	camera.fov = move_toward(camera.fov, wanted_fov, 10.0 * delta)
	_telemetry_elapsed += delta
	if _telemetry_elapsed >= 0.1:
		_telemetry_elapsed = 0.0
		_store_presentation_record({
			"event": "camera_sample",
			"captured": is_captured(),
			"mouse_dx": snappedf(_frame_mouse_delta.x, 0.01),
			"mouse_dy": snappedf(_frame_mouse_delta.y, 0.01),
			"yaw_degrees": snappedf(rad_to_deg(_world_yaw), 0.01),
			"pitch_degrees": snappedf(rad_to_deg(_pitch), 0.01),
		})
		_frame_mouse_delta = Vector2.ZERO

func sample_target_transform() -> Transform3D:
	if target == null:
		return Transform3D.IDENTITY
	return target.get_global_transform_interpolated()

static func interpolate_render_transform(previous: Transform3D, current: Transform3D, fraction: float) -> Transform3D:
	return previous.interpolate_with(current, clampf(fraction, 0.0, 1.0))

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
	rotation = Vector3(0.0, _world_yaw, 0.0)
	reset_physics_interpolation()

func _is_teleport(raw_target: Transform3D) -> bool:
	if not _has_raw_target:
		return false
	var yaw_delta := absf(wrapf(raw_target.basis.get_euler().y - _last_raw_target.basis.get_euler().y, -PI, PI))
	return raw_target.origin.distance_to(_last_raw_target.origin) > teleport_guard_metres or yaw_delta > deg_to_rad(teleport_guard_degrees)

static func _transform_is_finite(value: Transform3D) -> bool:
	return value.origin.is_finite() and value.basis.is_finite()

func _on_telemetry(data: Dictionary) -> void:
	_speed_fraction = float(data.get("speed_fraction", 0.0))
	_yaw_rate_degs = float(data.get("yaw_rate_degs", 0.0))
	_stance_id = int(data.get("stance_id", _stance_id))

func _emit_capture_if_changed(force := false) -> void:
	var captured := is_captured()
	if force or captured != _last_reported_capture:
		_last_reported_capture = captured
		capture_state_changed.emit(captured)
		_store_presentation_record({"event": "capture_changed", "captured": captured})

func _open_presentation_log() -> void:
	var logs_path := ProjectSettings.globalize_path("user://logs")
	if DirAccess.make_dir_recursive_absolute(logs_path) != OK:
		return
	_presentation_log = FileAccess.open("user://logs/presentation-latest.jsonl", FileAccess.WRITE)
	_store_presentation_record({
		"event": "session_start",
		"schema_version": 1,
		"build_commit": str(ProjectSettings.get_setting("application/config/build_commit", "development")),
	})

func _store_presentation_record(record: Dictionary) -> void:
	if _presentation_log == null:
		return
	record["record_type"] = "presentation"
	record["monotonic_ms"] = Time.get_ticks_msec()
	_presentation_log.store_line(JSON.stringify(record))
	_presentation_log.flush()
