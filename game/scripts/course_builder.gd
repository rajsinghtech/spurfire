extends Node3D

const COLOR_GROUND := Color(0.25, 0.29, 0.25)
const COLOR_LANE := Color(0.38, 0.39, 0.4)
const COLOR_ROUGH := Color(0.20, 0.17, 0.15)
const COLOR_ORANGE := Color(0.95, 0.45, 0.08)

func _ready() -> void:
	# Broad ground and 8 x 60 m timing straight.
	_box("BroadGround", Vector3(120, 0.5, 140), Vector3(0, -0.25, 25), COLOR_GROUND)
	_box("FlatStraight", Vector3(8, 0.04, 60), Vector3(0, 0.02, -10), COLOR_LANE, false)
	_gate("StartGate", Vector3(0, 0, 20))
	_gate("FinishGate", Vector3(0, 0, -40))
	for z in range(-40, 21, 10):
		_box("SpeedMarker_%d" % z, Vector3(8.5, 0.025, 0.12), Vector3(0, 0.05, z), COLOR_ORANGE, false)

	# Six-post slalom, offset alternately across a dedicated lane.
	for index in 6:
		var x := 17.0 + (3.0 if index % 2 == 0 else -3.0)
		_cylinder("SlalomPost_%d" % index, 0.18, 1.5, Vector3(x, 0.75, 20 - index * 10), COLOR_ORANGE)

	# Rough terrain surface; collider metadata and group are consumed by native code.
	var rough := _box("RoughStrip", Vector3(8, 0.08, 20), Vector3(-15, 0.04, -5), COLOR_ROUGH)
	rough.add_to_group(&"rough")
	rough.set_meta(&"rough_terrain", true)
	_grid(Vector3(-15, 0.09, -5), Vector2(8, 20), COLOR_ORANGE)

	# Slope block: representative 15°, 25°, and unclimbable 45° faces.
	_ramp("Ramp15", Vector3(7, 0.5, 14), Vector3(28, 1.8, 12), -15.0, Color(0.34, 0.34, 0.38))
	_ramp("Ramp25", Vector3(7, 0.5, 14), Vector3(38, 3.15, 12), -25.0, Color(0.38, 0.34, 0.34))
	_ramp("Face45", Vector3(7, 0.6, 10), Vector3(48, 3.55, 12), -45.0, Color(0.42, 0.25, 0.22))
	_ramp("Descent25", Vector3(7, 0.5, 14), Vector3(38, 3.15, -8), 25.0, Color(0.38, 0.34, 0.34))

	# Jump lane: three 1 m fences, plus an elevated ledge/drop.
	for index in 3:
		_fence("JumpFence_%d" % index, Vector3(-30, 0, 14 - index * 12), 7.0)
	_box("DropLedge", Vector3(8, 2, 12), Vector3(-30, 1, -28), Color(0.31, 0.32, 0.35))

	# Narrow bridge with solid rails for collision/camera tests.
	_box("BridgeDeck", Vector3(3, 0.35, 25), Vector3(13, 1.0, -25), Color(0.34, 0.30, 0.24))
	for x in [11.35, 14.65]:
		_box("BridgeRail", Vector3(0.22, 1.2, 25), Vector3(x, 1.7, -25), COLOR_ORANGE)

	# Painted 15 m turn-circle approximation.
	for index in 48:
		var angle := TAU * float(index) / 48.0
		_cylinder("TurnCircle_%d" % index, 0.09, 0.025, Vector3(35 + cos(angle) * 15, 0.04, -30 + sin(angle) * 15), Color(0.25, 0.7, 0.9), false)

	# Two highly visible safe reset pads. Marker3D children are reset anchors.
	_reset_pad("SpawnPad", Vector3(0, 0.06, 25))
	_reset_pad("CourseResetPad", Vector3(13, 0.06, 2))

func _material(color: Color) -> StandardMaterial3D:
	var material := StandardMaterial3D.new()
	material.albedo_color = color
	material.roughness = 0.9
	return material

func _box(label: String, size: Vector3, position: Vector3, color: Color, collision := true) -> StaticBody3D:
	var body := StaticBody3D.new()
	body.name = label
	body.position = position
	var mesh_instance := MeshInstance3D.new()
	var mesh := BoxMesh.new()
	mesh.size = size
	mesh.material = _material(color)
	mesh_instance.mesh = mesh
	body.add_child(mesh_instance)
	if collision:
		var collider := CollisionShape3D.new()
		var shape := BoxShape3D.new()
		shape.size = size
		collider.shape = shape
		body.add_child(collider)
	add_child(body)
	return body

func _cylinder(label: String, radius: float, height: float, position: Vector3, color: Color, collision := true) -> StaticBody3D:
	var body := StaticBody3D.new()
	body.name = label
	body.position = position
	var mesh_instance := MeshInstance3D.new()
	var mesh := CylinderMesh.new()
	mesh.top_radius = radius
	mesh.bottom_radius = radius
	mesh.height = height
	mesh.material = _material(color)
	mesh_instance.mesh = mesh
	body.add_child(mesh_instance)
	if collision:
		var collider := CollisionShape3D.new()
		var shape := CylinderShape3D.new()
		shape.radius = radius
		shape.height = height
		collider.shape = shape
		body.add_child(collider)
	add_child(body)
	return body

func _ramp(label: String, size: Vector3, position: Vector3, degrees: float, color: Color) -> void:
	var ramp := _box(label, size, position, color)
	ramp.rotation.x = deg_to_rad(degrees)

func _fence(label: String, position: Vector3, width: float) -> void:
	_box(label + "Rail", Vector3(width, 0.18, 0.18), position + Vector3(0, 1.0, 0), Color(0.78, 0.72, 0.58))
	for x in [-width * 0.5, width * 0.5]:
		_box(label + "Post", Vector3(0.18, 1.3, 0.18), position + Vector3(x, 0.65, 0), Color(0.78, 0.72, 0.58))

func _gate(label: String, position: Vector3) -> void:
	for x in [-4.25, 4.25]:
		_box(label + "Post", Vector3(0.16, 2.5, 0.16), position + Vector3(x, 1.25, 0), COLOR_ORANGE)
	_box(label + "Top", Vector3(8.7, 0.16, 0.16), position + Vector3(0, 2.5, 0), COLOR_ORANGE)

func _reset_pad(label: String, position: Vector3) -> void:
	var pad := _cylinder(label, 1.5, 0.08, position, Color(0.15, 0.8, 0.55), false)
	var anchor := Marker3D.new()
	anchor.name = label + "Anchor"
	anchor.position.y = 1.2
	anchor.add_to_group(&"respawn_anchor")
	pad.add_child(anchor)

func _grid(center: Vector3, dimensions: Vector2, color: Color) -> void:
	for x in range(-4, 5):
		_box("RoughGridX", Vector3(0.025, 0.015, dimensions.y), center + Vector3(x, 0, 0), color, false)
	for z in range(-10, 11, 2):
		_box("RoughGridZ", Vector3(dimensions.x, 0.015, 0.025), center + Vector3(0, 0, z), color, false)
