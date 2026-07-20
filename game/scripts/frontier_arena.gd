extends Node3D

const SAND := Color("d9a05b")
const SAND_DARK := Color("c68a49")
const SCRUB := Color("7a8b4f")
const ROCK := Color("b96a4b")
const WOOD := Color("8a5a33")
const RED := Color("c44536")
const CREAM := Color("f5e9d0")

const CACTUS_TALL := preload("res://assets/kenney/nature-kit/cactus_tall.glb")
const CACTUS_SHORT := preload("res://assets/kenney/nature-kit/cactus_short.glb")
const ROCK_LARGE := preload("res://assets/kenney/nature-kit/rock_largeA.glb")
const ROCK_SMALL := preload("res://assets/kenney/nature-kit/rock_smallA.glb")
const TREE := preload("res://assets/kenney/nature-kit/tree_default.glb")
const FENCE := preload("res://assets/kenney/nature-kit/fence_simple.glb")
const SIGN := preload("res://assets/kenney/nature-kit/sign.glb")
const BARREL := preload("res://assets/kenney/survival-kit/barrel.glb")
const SIGNPOST := preload("res://assets/kenney/survival-kit/signpost.glb")

func _ready() -> void:
	_build_environment()
	_build_flat_basin()
	_restyle_measurement_fixtures()
	_build_corral()
	_build_main_street()
	_build_water_tower()
	_build_cactus_flats()
	_build_wash()
	_build_horizon()

func _build_environment() -> void:
	var world := get_node_or_null("WorldEnvironment") as WorldEnvironment
	if world and world.environment:
		var environment := world.environment.duplicate(true) as Environment
		var sky := Sky.new()
		var sky_material := ProceduralSkyMaterial.new()
		sky_material.sky_top_color = Color("3fb6c9")
		sky_material.sky_horizon_color = Color("f2a65a")
		sky_material.ground_horizon_color = Color("f2a65a")
		sky_material.ground_bottom_color = SAND_DARK
		sky_material.sun_angle_max = 6.0
		sky_material.sun_curve = 0.08
		sky.sky_material = sky_material
		environment.background_mode = Environment.BG_SKY
		environment.sky = sky
		environment.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
		environment.ambient_light_energy = 0.65
		environment.fog_enabled = true
		environment.fog_light_color = Color("f2a65a")
		environment.fog_density = 0.0035
		environment.fog_sky_affect = 0.7
		world.environment = environment
	var sun := get_node_or_null("Sun") as DirectionalLight3D
	if sun:
		sun.rotation_degrees = Vector3(-52.0, -32.0, 0.0)
		sun.light_color = Color("ffd9a8")
		sun.light_energy = 1.35
		sun.directional_shadow_max_distance = 120.0

func _build_flat_basin() -> void:
	var ground := StaticBody3D.new()
	ground.name = "FrontierGround"
	ground.position = Vector3(0.0, -0.3, 0.0)
	var mesh_instance := MeshInstance3D.new()
	var mesh := BoxMesh.new()
	mesh.size = Vector3(360.0, 0.5, 360.0)
	mesh.material = _material(SAND)
	mesh_instance.mesh = mesh
	ground.add_child(mesh_instance)
	var collider := CollisionShape3D.new()
	var shape := BoxShape3D.new()
	shape.size = mesh.size
	collider.shape = shape
	ground.add_child(collider)
	add_child(ground)

func _restyle_measurement_fixtures() -> void:
	var course := get_node_or_null("TestCourse")
	if course == null:
		return
	for child in course.get_children():
		var color := SAND_DARK
		if String(child.name).contains("Gate"):
			color = RED
		elif String(child.name).contains("Rough"):
			color = SCRUB
		elif String(child.name).contains("Ramp") or String(child.name).contains("Landing") or String(child.name).contains("Face"):
			color = ROCK
		elif String(child.name).contains("Bridge") or String(child.name).contains("Fence"):
			color = WOOD
		for grandchild in child.get_children():
			if grandchild is MeshInstance3D:
				(grandchild as MeshInstance3D).material_override = _material(color)

func _build_corral() -> void:
	var dressing := _landmark("Corral")
	for index in 12:
		var angle := TAU * float(index) / 12.0
		var fence := FENCE.instantiate() as Node3D
		fence.position = Vector3(cos(angle) * 12.0, 0.0, 25.0 + sin(angle) * 12.0)
		fence.rotation.y = -angle
		dressing.add_child(fence)
	_asset(dressing, SIGN, Vector3(-5.0, 0.0, 13.0), 0.0, "RideSign")
	_box(dressing, "Trough", Vector3(3.2, 0.65, 0.9), Vector3(5.0, 0.32, 20.0), WOOD)

func _build_main_street() -> void:
	var street := _landmark("MainStreet")
	for side in [-1.0, 1.0]:
		for index in 4:
			var z := 12.0 - float(index) * 20.0
			var width := 8.0 + float(index % 2) * 2.0
			_box(street, "Storefront", Vector3(width, 5.5, 8.0), Vector3(side * 11.0, 2.75, z), WOOD if index % 2 == 0 else ROCK)
			_box(street, "FalseFront", Vector3(width + 0.6, 2.0, 0.35), Vector3(side * 11.0, 5.4, z + 4.0), CREAM)
			_asset(street, BARREL, Vector3(side * 6.2, 0.0, z + 2.5), 0.0, "StreetBarrel")
	_asset(street, SIGNPOST, Vector3(7.0, 0.0, -20.0), 0.0, "StreetSignpost")

func _build_water_tower() -> void:
	var tower := _landmark("WaterTower")
	for x in [-2.2, 2.2]:
		for z in [-2.2, 2.2]:
			_box(tower, "TowerLeg", Vector3(0.35, 10.0, 0.35), Vector3(x, 5.0, -72.0 + z), WOOD)
	var tank := MeshInstance3D.new()
	tank.name = "Tank"
	tank.position = Vector3(0.0, 11.0, -72.0)
	var cylinder := CylinderMesh.new()
	cylinder.top_radius = 4.2
	cylinder.bottom_radius = 4.2
	cylinder.height = 4.0
	cylinder.radial_segments = 16
	cylinder.material = _material(RED)
	tank.mesh = cylinder
	tower.add_child(tank)
	var roof := MeshInstance3D.new()
	roof.position = Vector3(0.0, 13.5, -72.0)
	var cone := CylinderMesh.new()
	cone.top_radius = 0.0
	cone.bottom_radius = 4.8
	cone.height = 1.6
	cone.radial_segments = 16
	cone.material = _material(CREAM)
	roof.mesh = cone
	tower.add_child(roof)

func _build_cactus_flats() -> void:
	var flats := _landmark("CactusFlats")
	var positions := [Vector3(-32, 0, 18), Vector3(-38, 0, 4), Vector3(-28, 0, -12), Vector3(-42, 0, -28), Vector3(-24, 0, -46)]
	for index in positions.size():
		_asset(flats, CACTUS_TALL if index % 2 == 0 else CACTUS_SHORT, positions[index], float(index) * 0.7, "Cactus")
		_asset(flats, ROCK_LARGE if index % 2 == 0 else ROCK_SMALL, positions[index] + Vector3(3, 0, 2), float(index), "Rock")

func _build_wash() -> void:
	var wash := _landmark("DryWash")
	_box(wash, "WashBed", Vector3(18.0, 0.05, 92.0), Vector3(28.0, 0.03, -18.0), SCRUB)
	for index in 5:
		var position := Vector3(22.0 + float(index % 2) * 12.0, 0.0, 18.0 - float(index) * 20.0)
		_asset(wash, TREE, position, float(index), "DeadTree")

func _build_horizon() -> void:
	var horizon := _landmark("TerracottaMesas")
	var positions := [Vector3(-145, 8, -125), Vector3(-80, 12, -165), Vector3(25, 9, -170), Vector3(105, 14, -145), Vector3(155, 7, -70)]
	for index in positions.size():
		var base: Vector3 = positions[index]
		_box(horizon, "MesaBase", Vector3(35, 16, 24), base, ROCK)
		_box(horizon, "MesaCap", Vector3(22, 9, 20), base + Vector3(0, 12, 0), SAND_DARK)

func _landmark(label: String) -> Node3D:
	var node := Node3D.new()
	node.name = label
	add_child(node)
	return node

func _asset(parent: Node3D, scene: PackedScene, position: Vector3, yaw: float, label: String) -> void:
	var node := scene.instantiate() as Node3D
	node.name = label
	node.position = position
	node.rotation.y = yaw
	parent.add_child(node)

func _box(parent: Node3D, label: String, size: Vector3, position: Vector3, color: Color) -> void:
	var instance := MeshInstance3D.new()
	instance.name = label
	instance.position = position
	var mesh := BoxMesh.new()
	mesh.size = size
	mesh.material = _material(color)
	instance.mesh = mesh
	parent.add_child(instance)

func _material(color: Color) -> StandardMaterial3D:
	var material := StandardMaterial3D.new()
	material.albedo_color = color
	material.roughness = 0.92
	return material
