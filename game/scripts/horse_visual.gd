extends Node

@export var leg_swing_degrees := 24.0
@export var body_bob_metres := 0.08
@export var sidestep_lean_degrees := 8.0

const COAT_COLORS := [Color("c99755"), Color("5e4438"), Color("9b5d35")]
const ACCENT_COLORS := [Color("35d5c5"), Color("ef8a45"), Color("ef5aaa")]
const GAIT_CADENCE_HZ := [0.7, 1.9, 3.4, 5.2]
const ARCHETYPE_BODY_SCALE := [Vector3(0.92, 0.96, 1.0), Vector3(1.18, 1.08, 1.04), Vector3.ONE]
const ARCHETYPE_LEG_SCALE := [Vector3(0.92, 1.08, 0.92), Vector3(1.12, 0.95, 1.12), Vector3.ONE]
const LANDING_CROUCH_METRES := 0.06
const LANDING_RECOVERY_SECONDS := 0.12

var _speed_fraction := 0.0
var _lateral_fraction := 0.0
var _yaw_rate_fraction := 0.0
var _archetype := 2
var _gait := 0
var _phase := 0.0
var _body_base_y := 0.0
var _chest_base_y := 0.0
var _coat_material: StandardMaterial3D
var _accent_material: StandardMaterial3D
var _cream_material: StandardMaterial3D
var _dark_material: StandardMaterial3D
var _was_airborne := false
var _landing_remaining := 0.0

@onready var horse: Node3D = get_parent()
@onready var body: MeshInstance3D = horse.get_node("BodyProxy")
@onready var chest: MeshInstance3D = horse.get_node("ChestProxy")
@onready var head: MeshInstance3D = horse.get_node("HeadProxy")
@onready var neck: MeshInstance3D = horse.get_node("NeckProxy")
@onready var tail: MeshInstance3D = horse.get_node("TailProxy")
@onready var front_left: MeshInstance3D = horse.get_node("FrontLeftLeg")
@onready var front_right: MeshInstance3D = horse.get_node("FrontRightLeg")
@onready var rear_left: MeshInstance3D = horse.get_node("RearLeftLeg")
@onready var rear_right: MeshInstance3D = horse.get_node("RearRightLeg")

func _ready() -> void:
	_body_base_y = body.position.y
	_chest_base_y = chest.position.y
	_coat_material = _material(COAT_COLORS[_archetype])
	_accent_material = _material(ACCENT_COLORS[_archetype])
	_cream_material = _material(Color("f5e9d0"))
	_dark_material = _material(Color("5e3d24"))
	for node_name in ["BodyProxy", "ChestProxy", "NeckProxy", "HeadProxy", "LeftEar", "RightEar", "FrontLeftLeg", "FrontRightLeg", "RearLeftLeg", "RearRightLeg", "TailProxy", "ForwardMarker"]:
		var mesh := horse.get_node_or_null(node_name) as MeshInstance3D
		if mesh:
			mesh.material_override = _coat_material
	_ensure_signature_meshes()
	_apply_archetype_visuals.call_deferred()
	if horse.has_signal("telemetry_updated"):
		horse.telemetry_updated.connect(_on_telemetry)

func _process(delta: float) -> void:
	var cadence: float = float(GAIT_CADENCE_HZ[clampi(_gait, 0, GAIT_CADENCE_HZ.size() - 1)])
	_phase = fmod(_phase + delta * cadence * TAU, TAU)
	var amplitude := deg_to_rad(leg_swing_degrees) * maxf(_speed_fraction, absf(_lateral_fraction) * 0.45)
	var diagonal_a := sin(_phase) * amplitude
	var diagonal_b := sin(_phase + PI) * amplitude
	front_left.rotation.x = diagonal_a
	rear_right.rotation.x = diagonal_a
	front_right.rotation.x = diagonal_b
	rear_left.rotation.x = diagonal_b
	var bob := absf(sin(_phase * 2.0)) * body_bob_metres * _speed_fraction
	if _gait == 3:
		bob += sin(_phase * 2.0) * 0.025 * _speed_fraction
	_landing_remaining = maxf(0.0, _landing_remaining - delta)
	var crouch := LANDING_CROUCH_METRES * (_landing_remaining / LANDING_RECOVERY_SECONDS)
	body.position.y = _body_base_y + bob - crouch
	chest.position.y = _chest_base_y + bob * 0.8 - crouch
	var steering_read := clampf(_lateral_fraction + _yaw_rate_fraction * _speed_fraction, -1.0, 1.0)
	var target_lean := deg_to_rad(-sidestep_lean_degrees * steering_read)
	var blend := 1.0 - exp(-10.0 * delta)
	body.rotation.z = lerp_angle(body.rotation.z, target_lean, blend)
	chest.rotation.z = lerp_angle(chest.rotation.z, target_lean * 0.8, blend)
	head.rotation.y = lerp_angle(head.rotation.y, deg_to_rad(-12.0 * steering_read), blend)
	neck.rotation.y = lerp_angle(neck.rotation.y, deg_to_rad(-7.0 * steering_read), blend)
	tail.rotation.y = lerp_angle(tail.rotation.y, deg_to_rad(18.0 * _yaw_rate_fraction) + sin(_phase) * 0.08, blend)

func _on_telemetry(data: Dictionary) -> void:
	_speed_fraction = clampf(float(data.get("speed_fraction", 0.0)), 0.0, 1.0)
	_lateral_fraction = clampf(float(data.get("lateral_speed_mps", 0.0)) / 1.2, -1.0, 1.0)
	_yaw_rate_fraction = clampf(float(data.get("yaw_rate_degs", 0.0)) / 60.0, -1.0, 1.0)
	_gait = clampi(int(data.get("gait", 0)), 0, 3)
	var airborne := bool(data.get("is_airborne", false))
	if _was_airborne and not airborne:
		_landing_remaining = LANDING_RECOVERY_SECONDS
	_was_airborne = airborne
	var next_archetype := clampi(int(data.get("archetype", 2)), 0, 2)
	if next_archetype != _archetype:
		_archetype = next_archetype
		_apply_archetype_visuals()

func _ensure_signature_meshes() -> void:
	_add_box(horse, "SaddleBlanket", Vector3(1.18, 0.12, 1.35), Vector3(0, 1.52, 0.15), _accent_material)
	_add_box(horse, "ManeTuft", Vector3(0.18, 0.68, 0.48), Vector3(0, 2.18, -1.05), _dark_material)
	_add_box(horse, "ForeheadPlate", Vector3(0.68, 0.12, 0.52), Vector3(0, 2.43, -1.96), _dark_material)
	_add_box(horse, "PintoPatch", Vector3(0.58, 0.48, 0.12), Vector3(0.42, 1.2, -1.21), _cream_material)
	_add_box(horse, "PintoPatchRight", Vector3(0.58, 0.48, 0.12), Vector3(-0.42, 1.2, -1.21), _cream_material)
	_add_box(head, "FaceBlaze", Vector3(0.18, 0.38, 0.04), Vector3(0, 0.05, -0.49), _cream_material)
	for index in 3:
		_add_box(neck, "ManeRidge%d" % index, Vector3(0.15, 0.26, 0.22), Vector3(0, 0.2 - float(index) * 0.3, 0.2), _dark_material)
	for entry in [[front_left, "FrontLeftHoof"], [front_right, "FrontRightHoof"], [rear_left, "RearLeftHoof"], [rear_right, "RearRightHoof"]]:
		_add_box(entry[0], entry[1], Vector3(0.30, 0.18, 0.34), Vector3(0, -0.58, -0.04), _dark_material)
		_add_box(entry[0], "%sFeather" % entry[1], Vector3(0.28, 0.24, 0.30), Vector3(0, -0.43, 0), _cream_material)

func _apply_archetype_visuals() -> void:
	_coat_material.albedo_color = COAT_COLORS[_archetype]
	_accent_material.albedo_color = ACCENT_COLORS[_archetype]
	body.scale = ARCHETYPE_BODY_SCALE[_archetype]
	chest.scale = ARCHETYPE_BODY_SCALE[_archetype]
	for leg in [front_left, front_right, rear_left, rear_right]:
		leg.scale = ARCHETYPE_LEG_SCALE[_archetype]
	neck.scale = Vector3(1.0, 1.0, 1.0) if _archetype != 1 else Vector3(1.2, 1.18, 1.2)
	var mane := horse.get_node_or_null("ManeTuft") as MeshInstance3D
	var plate := horse.get_node_or_null("ForeheadPlate") as MeshInstance3D
	var patch := horse.get_node_or_null("PintoPatch") as MeshInstance3D
	var patch_right := horse.get_node_or_null("PintoPatchRight") as MeshInstance3D
	if mane:
		mane.visible = _archetype == 2
	if plate:
		plate.visible = _archetype == 1
	if patch:
		patch.visible = _archetype == 2
	if patch_right:
		patch_right.visible = _archetype == 2
	for entry in [[front_left, "FrontLeftHoofFeather"], [front_right, "FrontRightHoofFeather"], [rear_left, "RearLeftHoofFeather"], [rear_right, "RearRightHoofFeather"]]:
		var feather := (entry[0] as Node).get_node_or_null(entry[1]) as MeshInstance3D
		if feather:
			feather.visible = _archetype == 1

func _add_box(parent: Node3D, label: String, size: Vector3, position: Vector3, material: Material) -> void:
	if parent.has_node(label):
		return
	var instance := MeshInstance3D.new()
	instance.name = label
	instance.position = position
	var mesh := BoxMesh.new()
	mesh.size = size
	mesh.material = material
	instance.mesh = mesh
	parent.add_child.call_deferred(instance)

func _material(color: Color) -> StandardMaterial3D:
	var material := StandardMaterial3D.new()
	material.albedo_color = color
	material.roughness = 0.85
	return material
