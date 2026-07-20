extends Node3D

# Sunset Flats uses one restrained palette across world dressing and presentation.
const SUNSET_ZENITH := Color("3a7ca8")
const SUNSET_HORIZON := Color("f7b267")
const SUN_KEY := Color("ffc27a")
const SAND := Color("d9a05b")
const SAND_LIGHT := Color("e2b06a")
const SAND_DARK := Color("c68a49")
const WASH_BED := Color("a06a3f")
const SCRUB := Color("7a8b4f")
const SCRUB_DRY := Color("a8894f")
const ROCK := Color("b96a4b")
const MESA_MID := Color("c97f5d")
const MESA_FAR := Color("e3a983")
const WOOD := Color("8a5a33")
const WOOD_DARK := Color("5e3d24")
const CREAM := Color("f5e9d0")
const INK := Color("2b1d12")
const BRAND_RED := Color("c44536")
const SLATE := Color("1b222d")
const SCATTER_SEED := 1849

const CACTUS_TALL := preload("res://assets/kenney/nature-kit/cactus_tall.glb")
const CACTUS_SHORT := preload("res://assets/kenney/nature-kit/cactus_short.glb")
const ROCK_LARGE := preload("res://assets/kenney/nature-kit/rock_largeA.glb")
const ROCK_SMALL := preload("res://assets/kenney/nature-kit/rock_smallA.glb")
const TREE := preload("res://assets/kenney/nature-kit/tree_default.glb")
const FENCE := preload("res://assets/kenney/nature-kit/fence_simple.glb")
const SIGN := preload("res://assets/kenney/nature-kit/sign.glb")
const BARREL := preload("res://assets/kenney/survival-kit/barrel.glb")
const SIGNPOST := preload("res://assets/kenney/survival-kit/signpost.glb")

var _materials: Dictionary = {}
var _windmill_rotor: Node3D
var _birds: Array[Node3D] = []
var _bird_phase := 0.0

func _ready() -> void:
	_build_environment()
	_build_flat_basin()
	_restyle_measurement_fixtures()
	_build_trails()
	_build_sun_arch()
	_build_bounty_bell()
	_build_corral()
	_build_main_street()
	_build_water_tower()
	_build_cactus_flats()
	_build_wash()
	_build_horizon()
	_build_rim_berms()
	_build_ground_detail()
	_build_birds()

func _process(delta: float) -> void:
	if _windmill_rotor:
		_windmill_rotor.rotation.z = fmod(_windmill_rotor.rotation.z + delta * 0.4, TAU)
	_bird_phase = fmod(_bird_phase + delta * 0.22, TAU)
	for index in _birds.size():
		var angle := _bird_phase + TAU * float(index) / float(_birds.size())
		var radius := 6.0 + float(index % 3) * 2.3
		var bird := _birds[index]
		bird.position = Vector3(cos(angle) * radius, 18.0 + sin(angle * 2.0) * 0.7, -72.0 + sin(angle) * radius)
		bird.rotation.y = -angle
		bird.rotation.z = sin(angle * 2.0) * 0.22

func _build_environment() -> void:
	var world := get_node_or_null("WorldEnvironment") as WorldEnvironment
	if world and world.environment:
		var environment := world.environment.duplicate(true) as Environment
		var sky := Sky.new()
		var sky_material := ProceduralSkyMaterial.new()
		sky_material.sky_top_color = SUNSET_ZENITH
		sky_material.sky_horizon_color = SUNSET_HORIZON
		sky_material.ground_horizon_color = SUNSET_HORIZON
		sky_material.ground_bottom_color = WOOD
		sky_material.sun_angle_max = 20.0
		sky_material.sun_curve = 0.06
		sky.sky_material = sky_material
		environment.background_mode = Environment.BG_SKY
		environment.sky = sky
		environment.ambient_light_source = Environment.AMBIENT_SOURCE_SKY
		environment.ambient_light_energy = 0.75
		environment.fog_enabled = true
		environment.fog_light_color = Color("e8a273")
		environment.fog_density = 0.0042
		environment.fog_sky_affect = 0.75
		world.environment = environment
	var sun := get_node_or_null("Sun") as DirectionalLight3D
	if sun:
		sun.rotation_degrees = Vector3(-20.0, 128.0, 0.0)
		sun.light_color = SUN_KEY
		sun.light_energy = 1.15
		sun.directional_shadow_max_distance = 120.0

func _build_flat_basin() -> void:
	var broad_ground := get_node_or_null("TestCourse/BroadGround")
	if broad_ground:
		broad_ground.free()
	var ground := StaticBody3D.new()
	ground.name = "FrontierGround"
	ground.position.y = -0.3
	var visual := MeshInstance3D.new()
	visual.name = "Mosaic"
	visual.mesh = _ground_mosaic_mesh()
	visual.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	ground.add_child(visual)
	var collider := CollisionShape3D.new()
	var shape := BoxShape3D.new()
	shape.size = Vector3(360.0, 0.5, 360.0)
	collider.shape = shape
	ground.add_child(collider)
	add_child(ground)

func _ground_mosaic_mesh() -> ArrayMesh:
	var surface := SurfaceTool.new()
	surface.begin(Mesh.PRIMITIVE_TRIANGLES)
	var cells := 24
	var cell_size := 360.0 / float(cells)
	for z in cells:
		for x in cells:
			var hash := posmod(x * 37 + z * 71 + SCATTER_SEED, 20)
			var color := SAND if hash < 12 else (SAND_DARK if hash < 17 else SAND_LIGHT)
			if x < 9 and posmod(hash, 9) == 0:
				color = SAND.lerp(SCRUB, 0.22)
			var x0 := -180.0 + float(x) * cell_size
			var z0 := -180.0 + float(z) * cell_size
			_add_surface_quad(surface, Vector3(x0, 0.25, z0), Vector3(x0 + cell_size, 0.25, z0), Vector3(x0 + cell_size, 0.25, z0 + cell_size), Vector3(x0, 0.25, z0 + cell_size), color)
	var material := StandardMaterial3D.new()
	material.vertex_color_use_as_albedo = true
	material.roughness = 0.94
	surface.set_material(material)
	return surface.commit()

func _add_surface_quad(surface: SurfaceTool, a: Vector3, b: Vector3, c: Vector3, d: Vector3, color: Color) -> void:
	for point in [a, b, c, a, c, d]:
		surface.set_color(color)
		surface.set_normal(Vector3.UP)
		surface.add_vertex(point)

func _restyle_measurement_fixtures() -> void:
	var course := get_node_or_null("TestCourse")
	if course == null:
		return
	for child in course.get_children():
		var color: Variant = null
		if String(child.name).contains("Gate"):
			color = BRAND_RED
		elif String(child.name).contains("Rough"):
			color = SCRUB
		elif String(child.name).contains("Ramp") or String(child.name).contains("Landing") or String(child.name).contains("Face"):
			color = ROCK
		elif String(child.name).contains("Bridge") or String(child.name).contains("Fence"):
			color = WOOD
		if color == null:
			continue
		for grandchild in child.get_children():
			if grandchild is MeshInstance3D:
				(grandchild as MeshInstance3D).material_override = _material(color)

func _build_trails() -> void:
	var trails := _landmark("TrailRibbons")
	_visual_box(trails, "MainTrail", Vector3(8.0, 0.025, 104.0), Vector3(0.0, 0.015, -30.0), SAND_DARK)
	_visual_box(trails, "MainTrailCore", Vector3(3.2, 0.03, 104.0), Vector3(0.0, 0.03, -30.0), WASH_BED)
	_visual_box(trails, "CorralTrail", Vector3(30.0, 0.025, 5.0), Vector3(14.0, 0.015, 24.0), SAND_DARK)

func _build_sun_arch() -> void:
	var arch := _landmark("SunArch")
	_box(arch, "LeftPost", Vector3(0.45, 5.5, 0.45), Vector3(-5.5, 2.75, 14.0), WOOD_DARK)
	_box(arch, "RightPost", Vector3(0.45, 5.5, 0.45), Vector3(5.5, 2.75, 14.0), WOOD_DARK)
	_box(arch, "Crossbeam", Vector3(11.5, 0.45, 0.45), Vector3(0.0, 5.25, 14.0), WOOD_DARK)
	_asset(arch, SIGN, Vector3(0.0, 4.15, 14.0), PI * 0.5, "SunsetFlatsSign")

func _build_bounty_bell() -> void:
	var bell := _landmark("BountyBell")
	_box(bell, "LeftPost", Vector3(0.28, 3.6, 0.28), Vector3(-8.2, 1.8, -2.0), WOOD_DARK)
	_box(bell, "RightPost", Vector3(0.28, 3.6, 0.28), Vector3(-5.8, 1.8, -2.0), WOOD_DARK)
	_box(bell, "Beam", Vector3(2.8, 0.3, 0.3), Vector3(-7.0, 3.5, -2.0), WOOD_DARK)
	_cylinder(bell, "Bell", 0.36, 0.2, 0.55, Vector3(-7.0, 2.9, -2.0), Color("c9a227"), true)

func _build_corral() -> void:
	var dressing := _landmark("Corral")
	for index in 11:
		var angle := TAU * float(index + 1) / 12.0
		var fence := FENCE.instantiate() as Node3D
		fence.position = Vector3(cos(angle) * 12.0, 0.0, 25.0 + sin(angle) * 12.0)
		fence.rotation.y = -angle
		dressing.add_child(fence)
	_asset(dressing, SIGN, Vector3(-5.0, 0.0, 13.0), 0.0, "RideSign")
	_box(dressing, "Trough", Vector3(3.2, 0.65, 0.9), Vector3(5.0, 0.32, 20.0), WOOD)
	_cylinder(dressing, "HayBaleA", 0.62, 0.62, 0.85, Vector3(6.5, 0.62, 31.0), SCRUB_DRY, true, Vector3(0, 0, PI * 0.5))
	_cylinder(dressing, "HayBaleB", 0.62, 0.62, 0.85, Vector3(8.0, 0.62, 31.4), SCRUB_DRY, true, Vector3(0, 0, PI * 0.5))
	_build_windmill(dressing)

func _build_windmill(parent: Node3D) -> void:
	var windmill := Node3D.new()
	windmill.name = "Windmill"
	windmill.position = Vector3(14.0, 0.0, 34.0)
	parent.add_child(windmill)
	for side in [-1.0, 1.0]:
		for depth in [-1.0, 1.0]:
			var leg := _box(windmill, "Leg", Vector3(0.18, 7.2, 0.18), Vector3(side * 1.1, 3.6, depth * 0.7), WOOD_DARK)
			leg.rotation.z = deg_to_rad(-side * 8.0)
	_windmill_rotor = Node3D.new()
	_windmill_rotor.name = "Rotor"
	_windmill_rotor.position = Vector3(0.0, 7.5, -0.85)
	_windmill_rotor.rotation.y = PI * 0.5
	windmill.add_child(_windmill_rotor)
	for index in 8:
		var blade := _visual_box(_windmill_rotor, "Blade", Vector3(0.11, 2.9, 0.05), Vector3(0.0, 1.35, 0.0), CREAM)
		blade.rotation.z = TAU * float(index) / 8.0
	_cylinder(_windmill_rotor, "Hub", 0.28, 0.28, 0.3, Vector3.ZERO, BRAND_RED, false, Vector3(PI * 0.5, 0, 0))

func _build_main_street() -> void:
	var street := _landmark("MainStreet")
	for side: float in [-1.0, 1.0]:
		for index in 4:
			var z := 12.0 - float(index) * 20.0
			var width := 8.0 + float(index % 2) * 2.0
			var facade_color := WOOD if index % 3 != 2 else ROCK
			_box(street, "Storefront", Vector3(width, 5.5, 8.0), Vector3(side * 11.0, 2.75, z), facade_color)
			_box(street, "FalseFront", Vector3(width + 0.6, 2.0, 0.35), Vector3(side * 11.0, 5.4, z + 4.0), CREAM if index % 2 == 0 else WOOD_DARK)
			var front_x: float = side * (11.0 - 0.01)
			_visual_box(street, "Door", Vector3(0.04, 2.25, 1.25), Vector3(front_x, 1.15, z + 1.4), INK)
			_visual_box(street, "Window", Vector3(0.04, 1.25, 1.65), Vector3(front_x, 2.35, z - 1.2), INK)
			_visual_box(street, "WindowTrim", Vector3(0.06, 1.5, 1.9), Vector3(front_x + side * 0.01, 2.35, z - 1.2), CREAM)
			_visual_box(street, "WindowGlass", Vector3(0.07, 1.18, 1.58), Vector3(front_x + side * 0.02, 2.35, z - 1.2), SLATE)
			if index % 2 == 1:
				_visual_box(street, "Awning", Vector3(1.1, 0.14, width * 0.72), Vector3(side * 6.9, 3.2, z), BRAND_RED)
			_asset(street, BARREL, Vector3(side * 6.2, 0.0, z + 2.5), 0.0, "StreetBarrel")
	_asset(street, SIGNPOST, Vector3(7.0, 0.0, -20.0), 0.0, "StreetSignpost")

func _build_water_tower() -> void:
	var tower := _landmark("WaterTower")
	for x in [-2.2, 2.2]:
		for z in [-2.2, 2.2]:
			_box(tower, "TowerLeg", Vector3(0.35, 11.5, 0.35), Vector3(x, 5.75, -72.0 + z), WOOD_DARK)
	for y in [3.2, 7.4]:
		for side in [-1.0, 1.0]:
			var brace := _box(tower, "CrossBrace", Vector3(0.14, 5.2, 0.14), Vector3(side * 2.2, y, -72.0), WOOD)
			brace.rotation.x = deg_to_rad(38.0 * side)
	_cylinder(tower, "TankBody", 4.2, 4.2, 3.6, Vector3(0.0, 13.2, -72.0), WOOD_DARK, true)
	_cylinder(tower, "TankBand", 4.25, 4.25, 1.05, Vector3(0.0, 13.2, -72.0), BRAND_RED, true)
	_cylinder(tower, "Roof", 0.0, 4.8, 1.6, Vector3(0.0, 15.8, -72.0), CREAM, true)
	_cylinder(tower, "Finial", 0.12, 0.18, 0.8, Vector3(0.0, 17.0, -72.0), CREAM, true)

func _build_cactus_flats() -> void:
	var flats := _landmark("CactusFlats")
	var positions := [Vector3(-32, 0, 18), Vector3(-38, 0, 4), Vector3(-28, 0, -12), Vector3(-42, 0, -28), Vector3(-24, 0, -46)]
	for index in positions.size():
		_asset(flats, CACTUS_TALL if index % 2 == 0 else CACTUS_SHORT, positions[index], float(index) * 0.7, "Cactus")
		_asset(flats, ROCK_LARGE if index % 2 == 0 else ROCK_SMALL, positions[index] + Vector3(3, 0, 2), float(index), "Rock")
	var elder := CACTUS_TALL.instantiate() as Node3D
	elder.name = "ElderCactus"
	elder.position = Vector3(-36.0, 0.0, -8.0)
	elder.scale = Vector3.ONE * 2.2
	flats.add_child(elder)
	_add_asset_collisions(elder)

func _build_wash() -> void:
	var wash := _landmark("DryWash")
	_box(wash, "WashBed", Vector3(18.0, 0.05, 92.0), Vector3(28.0, 0.03, -18.0), SAND_DARK)
	for index in 9:
		var z := 20.0 - float(index) * 11.0
		var x := 28.0 + sin(float(index) * 1.35) * 5.5
		_visual_box(wash, "Meander", Vector3(9.0, 0.03, 14.0), Vector3(x, 0.07, z), WASH_BED)
		_asset(wash, ROCK_SMALL, Vector3(x + (-7.0 if index % 2 == 0 else 7.0), 0.0, z), float(index), "BankRock")
	for index in 7:
		var position := Vector3(22.0 + float(index % 2) * 12.0, 0.0, 24.0 - float(index) * 17.0)
		var tree := _asset(wash, TREE, position, float(index), "DryTree")
		_tint_asset(tree, SCRUB_DRY)

func _build_horizon() -> void:
	var horizon := _landmark("TerracottaMesas")
	var near := _landmark_under(horizon, "Near")
	var mid := _landmark_under(horizon, "Mid")
	var far := _landmark_under(horizon, "Far")
	var near_positions := [Vector3(-145, 8, -125), Vector3(-80, 12, -165), Vector3(25, 9, -170), Vector3(105, 14, -145), Vector3(155, 7, -70)]
	for base in near_positions:
		_box(near, "MesaBase", Vector3(35, 16, 24), base, ROCK)
		_box(near, "MesaCap", Vector3(22, 9, 20), base + Vector3(0, 12, 0), SAND_DARK)
	for base in [Vector3(-205, 11, -105), Vector3(-120, 9, -205), Vector3(85, 10, -215), Vector3(205, 12, -95)]:
		_box(mid, "Mesa", Vector3(54, 18, 28), base, MESA_MID)
	for base in [Vector3(-245, 10, -35), Vector3(-210, 12, -175), Vector3(-35, 9, -252), Vector3(150, 11, -220), Vector3(250, 9, -45)]:
		_visual_box(far, "FarMesa", Vector3(70, 17, 24), base, MESA_FAR)
	var hero := _landmark("HeroMesa")
	_box(hero, "HeroBase", Vector3(52, 20, 30), Vector3(0, 10, -175), MESA_MID)
	_box(hero, "HeroCapLeft", Vector3(18, 9, 25), Vector3(-15, 24, -175), ROCK)
	_box(hero, "HeroCapRight", Vector3(18, 12, 25), Vector3(15, 25.5, -175), ROCK)
	_box(hero, "ArchLintel", Vector3(14, 6, 25), Vector3(0, 27, -175), ROCK)

func _build_rim_berms() -> void:
	var rim := _landmark("RimBerms")
	for index in 24:
		if index in [2, 8, 14, 20]:
			continue
		var angle := TAU * float(index) / 24.0
		var radius := 172.0
		var position := Vector3(cos(angle) * radius, 1.1, sin(angle) * radius)
		var berm := _box(rim, "Berm", Vector3(13.0, 2.2, 5.0), position, SAND_DARK if index % 2 == 0 else ROCK)
		berm.rotation.y = -angle

func _build_ground_detail() -> void:
	var detail := _landmark("GroundDetail")
	var rng := RandomNumberGenerator.new()
	rng.seed = SCATTER_SEED
	var pebble_mesh := SphereMesh.new()
	pebble_mesh.radius = 0.45
	pebble_mesh.height = 0.55
	pebble_mesh.radial_segments = 6
	pebble_mesh.rings = 3
	pebble_mesh.material = _material(ROCK)
	var pebble_multi := MultiMesh.new()
	pebble_multi.transform_format = MultiMesh.TRANSFORM_3D
	pebble_multi.mesh = pebble_mesh
	pebble_multi.instance_count = 120
	for index in pebble_multi.instance_count:
		var point := _scatter_point(rng, index)
		var scale := rng.randf_range(0.25, 0.65)
		pebble_multi.set_instance_transform(index, Transform3D(Basis.from_scale(Vector3(scale, scale * 0.7, scale)), point))
	var pebbles := MultiMeshInstance3D.new()
	pebbles.name = "Pebbles"
	pebbles.multimesh = pebble_multi
	pebbles.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	detail.add_child(pebbles)
	var tuft_mesh := PrismMesh.new()
	tuft_mesh.size = Vector3(0.42, 0.65, 0.18)
	tuft_mesh.material = _material(SCRUB_DRY)
	var tuft_multi := MultiMesh.new()
	tuft_multi.transform_format = MultiMesh.TRANSFORM_3D
	tuft_multi.mesh = tuft_mesh
	tuft_multi.instance_count = 150
	for index in tuft_multi.instance_count:
		var point := _scatter_point(rng, index + 120)
		var basis := Basis(Vector3.UP, rng.randf_range(0.0, TAU)).scaled(Vector3.ONE * rng.randf_range(0.65, 1.35))
		tuft_multi.set_instance_transform(index, Transform3D(basis, point + Vector3(0, 0.3, 0)))
	var tufts := MultiMeshInstance3D.new()
	tufts.name = "ScrubTufts"
	tufts.multimesh = tuft_multi
	tufts.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	detail.add_child(tufts)

func _scatter_point(rng: RandomNumberGenerator, salt: int) -> Vector3:
	var zones := [Vector2(-52, 30), Vector2(-58, -28), Vector2(55, 42), Vector2(66, -42), Vector2(-95, -78), Vector2(92, -92)]
	var zone: Vector2 = zones[salt % zones.size()]
	return Vector3(zone.x + rng.randf_range(-17.0, 17.0), 0.05, zone.y + rng.randf_range(-17.0, 17.0))

func _build_birds() -> void:
	var flock := _landmark("CirclingBirds")
	for index in 5:
		var bird := Node3D.new()
		bird.name = "Bird"
		var left := _visual_box(bird, "LeftWing", Vector3(0.55, 0.035, 0.16), Vector3(-0.23, 0, 0), SLATE)
		left.rotation.z = -0.22
		var right := _visual_box(bird, "RightWing", Vector3(0.55, 0.035, 0.16), Vector3(0.23, 0, 0), SLATE)
		right.rotation.z = 0.22
		flock.add_child(bird)
		_birds.append(bird)

func _landmark(label: String) -> Node3D:
	var node := Node3D.new()
	node.name = label
	add_child(node)
	return node

func _landmark_under(parent: Node3D, label: String) -> Node3D:
	var node := Node3D.new()
	node.name = label
	parent.add_child(node)
	return node

func _asset(parent: Node3D, scene: PackedScene, position: Vector3, yaw: float, label: String) -> Node3D:
	var node := scene.instantiate() as Node3D
	node.name = label
	node.position = position
	node.rotation.y = yaw
	parent.add_child(node)
	_add_asset_collisions(node)
	return node

func _add_asset_collisions(node: Node3D) -> void:
	for descendant in node.find_children("*", "MeshInstance3D", true, false):
		var mesh_instance := descendant as MeshInstance3D
		if mesh_instance and mesh_instance.mesh:
			mesh_instance.create_trimesh_collision()

func _tint_asset(node: Node3D, color: Color) -> void:
	for descendant in node.find_children("*", "MeshInstance3D", true, false):
		(descendant as MeshInstance3D).material_override = _material(color)

func _box(parent: Node3D, label: String, size: Vector3, position: Vector3, color: Color) -> StaticBody3D:
	var body := StaticBody3D.new()
	body.name = label
	body.position = position
	var instance := MeshInstance3D.new()
	var mesh := BoxMesh.new()
	mesh.size = size
	mesh.material = _material(color)
	instance.mesh = mesh
	body.add_child(instance)
	var collider := CollisionShape3D.new()
	var shape := BoxShape3D.new()
	shape.size = size
	collider.shape = shape
	body.add_child(collider)
	parent.add_child(body)
	return body

func _visual_box(parent: Node3D, label: String, size: Vector3, position: Vector3, color: Color) -> MeshInstance3D:
	var instance := MeshInstance3D.new()
	instance.name = label
	instance.position = position
	var mesh := BoxMesh.new()
	mesh.size = size
	mesh.material = _material(color)
	instance.mesh = mesh
	instance.cast_shadow = GeometryInstance3D.SHADOW_CASTING_SETTING_OFF
	parent.add_child(instance)
	return instance

func _cylinder(parent: Node3D, label: String, top_radius: float, bottom_radius: float, height: float, position: Vector3, color: Color, collision: bool, rotation := Vector3.ZERO) -> MeshInstance3D:
	var instance := MeshInstance3D.new()
	instance.name = label
	instance.position = position
	instance.rotation = rotation
	var mesh := CylinderMesh.new()
	mesh.top_radius = top_radius
	mesh.bottom_radius = bottom_radius
	mesh.height = height
	mesh.radial_segments = 12
	mesh.material = _material(color)
	instance.mesh = mesh
	parent.add_child(instance)
	if collision:
		instance.create_trimesh_collision()
	return instance

func _material(color: Color) -> StandardMaterial3D:
	var key := color.to_html()
	if _materials.has(key):
		return _materials[key] as StandardMaterial3D
	var material := StandardMaterial3D.new()
	material.albedo_color = color
	material.roughness = 0.92
	_materials[key] = material
	return material
