extends SceneTree
## godot_review_assets: lays out the requested scenes in a SubViewport of its own world, lights
## them, frames them with a camera, saves one PNG outside the project and prints the counts and
## bounds as JSON after MARKER. Nothing is saved into the project. The request is a JSON file
## named by the first user argument; the server checked its paths and values before writing it.

const MARKER: String = "GODOT_MCP_REVIEW="
## Frames rendered for each per-asset measurement before its render info is read.
const MEASURE_FRAMES: int = 3

var request: Dictionary = {}
var viewport: SubViewport = null
var stage: Node3D = null
var holders: Array[Node3D] = []
var plane_nodes: Array[Node3D] = []
var camera: Camera3D = null
var warnings: Array[String] = []
var asset_reports: Array[Dictionary] = []
var frame_info: Dictionary = {}
## -1 settles and captures the whole picture; 0..n-1 measures that asset alone.
var measuring: int = -1
var wait_frames: int = 0
var finished: bool = false


func _initialize() -> void:
	var args: PackedStringArray = OS.get_cmdline_user_args()
	if args.is_empty():
		_fail("no request file was given")
		return
	var text: String = FileAccess.get_file_as_string(args[0])
	var parsed: Variant = JSON.parse_string(text)
	if not parsed is Dictionary:
		_fail("the request file is not a JSON object")
		return
	request = parsed


func _process(_delta: float) -> bool:
	if finished:
		return true
	# Built on the first frame: the root is not in the tree yet during _initialize, and the
	# layout needs global transforms.
	if viewport == null:
		var problem: String = _build()
		if not problem.is_empty():
			_fail(problem)
			return true
		wait_frames = int(request.get("settle_frames", 8))
		return false
	if wait_frames > 0:
		wait_frames -= 1
		return false
	if measuring == -1:
		frame_info = _render_info()
		var image: Image = viewport.get_texture().get_image()
		if image == null or image.is_empty():
			_fail("the viewport gave no image; a headless Godot does not render")
			return true
		var saved: Error = image.save_png(str(request.get("output", "")))
		if saved != OK:
			_fail("the PNG could not be saved: " + error_string(saved))
			return true
		for plane: Node3D in plane_nodes:
			plane.visible = false
		measuring = 0
	else:
		asset_reports[measuring]["rendered"] = _render_info()
		measuring += 1
	if measuring >= holders.size():
		_finish()
		return true
	for index: int in holders.size():
		holders[index].visible = index == measuring
	wait_frames = MEASURE_FRAMES
	return false


func _build() -> String:
	var width: int = int(request.get("width", 1280))
	var height: int = int(request.get("height", 720))
	viewport = SubViewport.new()
	viewport.name = "GodotMcpReview"
	viewport.size = Vector2i(width, height)
	viewport.own_world_3d = true
	viewport.transparent_bg = false
	viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	# The project's anti-aliasing, as its own window would draw the game.
	viewport.msaa_3d = root.msaa_3d
	viewport.screen_space_aa = root.screen_space_aa
	viewport.use_taa = root.use_taa
	viewport.use_debanding = root.use_debanding
	viewport.scaling_3d_mode = root.scaling_3d_mode
	viewport.scaling_3d_scale = root.scaling_3d_scale
	root.add_child(viewport)
	stage = Node3D.new()
	stage.name = "Stage"
	viewport.add_child(stage)

	var problem: String = _place_assets()
	if not problem.is_empty():
		return problem
	var bounds: AABB = _union()
	problem = _add_environment()
	if not problem.is_empty():
		return problem
	problem = _add_planes(bounds)
	if not problem.is_empty():
		return problem
	return _add_camera(bounds, float(width) / float(height))


func _place_assets() -> String:
	var assets: Array = request.get("assets", [])
	var gap: float = float(request.get("gap", 1.0))
	var cursor: float = 0.0
	var laid_out: Array[int] = []
	for index: int in assets.size():
		var asset: Dictionary = assets[index]
		var path: String = str(asset.get("scene", ""))
		var packed: PackedScene = ResourceLoader.load(path, "PackedScene") as PackedScene
		if packed == null:
			return "the scene " + path + " could not be loaded as a PackedScene"
		var instance: Node = packed.instantiate()
		if instance == null:
			return "the scene " + path + " could not be instantiated"
		var holder: Node3D = Node3D.new()
		holder.name = "Asset%d" % index
		holder.rotation_degrees = _vector(asset.get("rotation"), Vector3.ZERO)
		holder.scale = _vector(asset.get("scale"), Vector3.ONE)
		holder.add_child(instance)
		stage.add_child(holder)
		if not instance is Node3D:
			warnings.append(path + ": its root is a " + instance.get_class() + ", not a Node3D; its 3D children are reviewed as they are")
		holders.append(holder)
		if asset.has("position"):
			holder.position = _vector(asset.get("position"), Vector3.ZERO)
		else:
			laid_out.append(index)
		asset_reports.append({"scene": path})
	# A row along X, in the given order, each asset's bounds a gap from the last.
	for index: int in laid_out:
		var box: AABB = _bounds_of(holders[index])
		if box.size == Vector3.ZERO and box.position == Vector3.ZERO:
			holders[index].position = Vector3(cursor, 0.0, 0.0)
			cursor += gap
			continue
		holders[index].position = Vector3(cursor - box.position.x, 0.0, -(box.position.z + box.size.z * 0.5))
		cursor += box.size.x + gap
	if laid_out.size() > 0:
		var shift: float = (cursor - gap) * 0.5
		for index: int in laid_out:
			holders[index].position.x -= shift
	for index: int in holders.size():
		var report: Dictionary = asset_reports[index]
		var counts: Dictionary = _geometry_of(holders[index])
		report.merge(counts)
		var box: AABB = _bounds_of(holders[index])
		report["has_geometry"] = int(counts["mesh_instances"]) > 0
		report["bounds"] = _aabb_json(box)
		report["position"] = _vector_json(holders[index].position)
		report["rotation"] = _vector_json(holders[index].rotation_degrees)
		report["scale"] = _vector_json(holders[index].scale)
		if int(counts["mesh_instances"]) == 0:
			warnings.append(str(report["scene"]) + " has no visible mesh")
	return ""


func _add_environment() -> String:
	var environment: Dictionary = request.get("environment", {})
	var lights_added: int = 0
	var world_added: bool = false
	if environment.has("scene"):
		var path: String = str(environment["scene"])
		var packed: PackedScene = ResourceLoader.load(path, "PackedScene") as PackedScene
		if packed == null:
			return "the environment scene " + path + " could not be loaded as a PackedScene"
		# Not added to the tree, so none of its scripts run; the copies carry no script.
		var source: Node = packed.instantiate()
		var worlds: Array[Node] = source.find_children("*", "WorldEnvironment", true, false)
		if source is WorldEnvironment:
			worlds.push_front(source)
		if worlds.is_empty():
			warnings.append(path + " has no WorldEnvironment; the default environment is used")
		else:
			var world: Node = worlds[0].duplicate(0)
			stage.add_child(world)
			world_added = true
			if worlds.size() > 1:
				warnings.append(path + " has %d WorldEnvironment nodes; the first one, %s, is used" % [worlds.size(), source.get_path_to(worlds[0])])
		if not environment.has("light"):
			var suns: Array[Node] = source.find_children("*", "DirectionalLight3D", true, false)
			if source is DirectionalLight3D:
				suns.push_front(source)
			for sun: Node in suns:
				var copy: DirectionalLight3D = sun.duplicate(0) as DirectionalLight3D
				copy.transform = _chain_transform(sun)
				stage.add_child(copy)
				lights_added += 1
			if suns.is_empty():
				warnings.append(path + " has no DirectionalLight3D; the default light is used")
		source.free()
	if environment.has("resource"):
		var resource_path: String = str(environment["resource"])
		var loaded: Environment = ResourceLoader.load(resource_path, "Environment") as Environment
		if loaded == null:
			return "the environment " + resource_path + " could not be loaded as an Environment"
		var world_node: WorldEnvironment = WorldEnvironment.new()
		world_node.environment = loaded
		stage.add_child(world_node)
		world_added = true
	if not world_added:
		var world_default: WorldEnvironment = WorldEnvironment.new()
		var made: Environment = Environment.new()
		made.background_mode = Environment.BG_COLOR
		made.background_color = Color(0.62, 0.66, 0.7)
		made.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
		made.ambient_light_color = Color(0.78, 0.8, 0.85)
		made.ambient_light_energy = 0.6
		made.tonemap_mode = Environment.TONE_MAPPER_FILMIC
		world_default.environment = made
		stage.add_child(world_default)
	if lights_added == 0:
		var light: Dictionary = environment.get("light", {})
		var sun_node: DirectionalLight3D = DirectionalLight3D.new()
		sun_node.rotation_degrees = Vector3(float(light.get("pitch", -50.0)), float(light.get("yaw", -40.0)), 0.0)
		sun_node.light_energy = float(light.get("energy", 1.0))
		sun_node.light_color = _color(light.get("color"), Color(1, 0.97, 0.92))
		sun_node.shadow_enabled = bool(light.get("shadows", true))
		stage.add_child(sun_node)
	return ""


func _add_planes(bounds: AABB) -> String:
	var planes: Array = request.get("planes", [])
	var center: Vector3 = bounds.get_center()
	var span: float = maxf(maxf(bounds.size.x, bounds.size.z) * 3.0, 10.0)
	for index: int in planes.size():
		var plane: Dictionary = planes[index]
		var water: bool = str(plane.get("kind", "ground")) == "water"
		var mesh: PlaneMesh = PlaneMesh.new()
		var size: Variant = plane.get("size")
		if size is Array and size.size() == 2:
			mesh.size = Vector2(float(size[0]), float(size[1]))
		elif size is float or size is int:
			mesh.size = Vector2(float(size), float(size))
		else:
			mesh.size = Vector2(span, span)
		var node: MeshInstance3D = MeshInstance3D.new()
		node.name = "Plane%d" % index
		node.mesh = mesh
		node.position = Vector3(center.x, float(plane.get("height", 0.0)), center.z)
		if plane.has("material"):
			var material_path: String = str(plane["material"])
			var material: Material = ResourceLoader.load(material_path, "Material") as Material
			if material == null:
				return "the plane material " + material_path + " could not be loaded as a Material"
			node.material_override = material
		else:
			var made: StandardMaterial3D = StandardMaterial3D.new()
			if water:
				made.albedo_color = _color(plane.get("color"), Color(0.35, 0.68, 0.74, 0.75))
				made.roughness = 0.2
			else:
				made.albedo_color = _color(plane.get("color"), Color(0.42, 0.48, 0.34))
				made.roughness = 1.0
			if made.albedo_color.a < 1.0:
				made.transparency = BaseMaterial3D.TRANSPARENCY_ALPHA
			node.material_override = made
		stage.add_child(node)
		plane_nodes.append(node)
	return ""


func _add_camera(bounds: AABB, aspect: float) -> String:
	var spec: Dictionary = request.get("camera", {})
	camera = Camera3D.new()
	camera.name = "ReviewCamera"
	camera.projection = Camera3D.PROJECTION_ORTHOGONAL
	camera.size = 10.0
	camera.near = 0.05
	camera.far = 4000.0
	var basis: Basis = Basis.from_euler(Vector3(deg_to_rad(-30.0), deg_to_rad(-35.0), 0.0))
	var copied: bool = false
	var copied_position: Vector3 = Vector3.ZERO
	if spec.has("scene"):
		var path: String = str(spec["scene"])
		var packed: PackedScene = ResourceLoader.load(path, "PackedScene") as PackedScene
		if packed == null:
			return "the camera scene " + path + " could not be loaded as a PackedScene"
		var source: Node = packed.instantiate()
		var found: Camera3D = null
		if spec.has("node"):
			found = source.get_node_or_null(NodePath(str(spec["node"]))) as Camera3D
			if found == null:
				source.free()
				return "the camera scene " + path + " has no Camera3D at " + str(spec["node"])
		else:
			var cameras: Array[Node] = source.find_children("*", "Camera3D", true, false)
			if source is Camera3D:
				cameras.push_front(source)
			for candidate: Node in cameras:
				if found == null or (candidate as Camera3D).current:
					found = candidate as Camera3D
			if found == null:
				source.free()
				return "the camera scene " + path + " has no Camera3D"
			if cameras.size() > 1:
				warnings.append(path + " has %d cameras; %s is used" % [cameras.size(), source.get_path_to(found)])
		camera.projection = found.projection
		camera.size = found.size
		camera.fov = found.fov
		camera.keep_aspect = found.keep_aspect
		camera.near = found.near
		camera.far = found.far
		camera.h_offset = found.h_offset
		camera.v_offset = found.v_offset
		camera.attributes = found.attributes
		camera.environment = found.environment
		var placed: Transform3D = _chain_transform(found)
		basis = placed.basis.orthonormalized()
		copied_position = placed.origin
		copied = true
		source.free()
	if spec.has("projection"):
		camera.projection = Camera3D.PROJECTION_PERSPECTIVE if str(spec["projection"]) == "perspective" else Camera3D.PROJECTION_ORTHOGONAL
	if spec.has("yaw") or spec.has("pitch"):
		var euler: Vector3 = basis.get_euler()
		var pitch: float = deg_to_rad(float(spec["pitch"])) if spec.has("pitch") else euler.x
		var yaw: float = deg_to_rad(float(spec["yaw"])) if spec.has("yaw") else euler.y
		basis = Basis.from_euler(Vector3(pitch, yaw, 0.0))
	if spec.has("fov"):
		camera.fov = float(spec["fov"])
	if spec.has("size"):
		camera.size = float(spec["size"])
	var margin: float = float(spec.get("margin", 0.05))
	var target: Vector3 = _vector(spec.get("target"), bounds.get_center())
	# Fit by default, unless a size (orthographic) or a distance or copied place (perspective) is set.
	var keep_place: bool = copied and bool(spec.get("keep_position", false))
	var fit: bool = bool(spec.get("fit", not spec.has("size") and not (copied and camera.projection == Camera3D.PROJECTION_ORTHOGONAL) and not spec.has("distance")))
	var corners: Array[Vector3] = _corners(bounds)
	var to_camera: Basis = basis.inverse()
	var low: Vector3 = Vector3.INF
	var high: Vector3 = -Vector3.INF
	for corner: Vector3 in corners:
		var local: Vector3 = to_camera * (corner - target)
		low = low.min(local)
		high = high.max(local)
	var position: Vector3
	if keep_place:
		position = copied_position
	elif camera.projection == Camera3D.PROJECTION_ORTHOGONAL:
		var view_center: Vector3 = Vector3((low.x + high.x) * 0.5, (low.y + high.y) * 0.5, 0.0) if fit and not spec.has("target") else Vector3.ZERO
		if fit:
			var width: float = (high.x - low.x)
			var height: float = (high.y - low.y)
			var fitted: float = maxf(height, width / aspect) if camera.keep_aspect == Camera3D.KEEP_HEIGHT else maxf(height * aspect, width)
			camera.size = maxf(fitted, 0.01) / maxf(1.0 - 2.0 * margin, 0.1)
		var back: float = float(spec.get("distance", maxf(high.z, 0.0) + maxf(camera.size, 1.0) + 1.0))
		position = target + basis * Vector3(view_center.x, view_center.y, back)
		camera.near = 0.05
		camera.far = maxf(back - low.z, 1.0) + 10.0
	else:
		var distance: float = float(spec.get("distance", 10.0))
		if fit:
			var vertical: float = tan(deg_to_rad(camera.fov) * 0.5)
			var horizontal: float = vertical * aspect
			if camera.keep_aspect == Camera3D.KEEP_WIDTH:
				horizontal = vertical
				vertical = horizontal / aspect
			var scale: float = maxf(1.0 - 2.0 * margin, 0.1)
			distance = 0.0
			for corner: Vector3 in corners:
				var local_corner: Vector3 = to_camera * (corner - target)
				distance = maxf(distance, absf(local_corner.x) / (horizontal * scale) + local_corner.z)
				distance = maxf(distance, absf(local_corner.y) / (vertical * scale) + local_corner.z)
			distance = maxf(distance, camera.near * 2.0)
		position = target + basis * Vector3(0.0, 0.0, distance)
		camera.far = maxf(camera.far, distance - low.z + 10.0)
	camera.transform = Transform3D(basis, position)
	stage.add_child(camera)
	camera.current = true
	return ""


func _finish() -> void:
	var totals: Dictionary = {"triangles": 0, "surfaces": 0, "mesh_instances": 0}
	for report: Dictionary in asset_reports:
		for key: String in totals.keys():
			totals[key] = int(totals[key]) + int(report[key])
	totals["bounds"] = _aabb_json(_union())
	totals["rendered"] = frame_info
	var euler: Vector3 = camera.basis.get_euler()
	var answer: Dictionary = {
		"image": str(request.get("output", "")),
		"width": viewport.size.x,
		"height": viewport.size.y,
		"assets": asset_reports,
		"totals": totals,
		"camera": {
			"projection": "perspective" if camera.projection == Camera3D.PROJECTION_PERSPECTIVE else "orthogonal",
			"position": _vector_json(camera.position),
			"yaw": snappedf(rad_to_deg(euler.y), 0.001),
			"pitch": snappedf(rad_to_deg(euler.x), 0.001),
			"size": snappedf(camera.size, 0.001),
			"fov": snappedf(camera.fov, 0.001),
			"near": snappedf(camera.near, 0.001),
			"far": snappedf(camera.far, 0.001),
		},
		"renderer": str(ProjectSettings.get_setting("rendering/renderer/rendering_method", "")),
		"warnings": warnings,
	}
	print(MARKER, JSON.stringify(answer))
	finished = true
	quit()


func _fail(problem: String) -> void:
	print(MARKER, JSON.stringify({"error": problem, "warnings": warnings}))
	finished = true
	quit(1)


func _render_info() -> Dictionary:
	var rid: RID = viewport.get_viewport_rid()
	var info: Dictionary = {}
	var kinds: Dictionary = {"": RenderingServer.VIEWPORT_RENDER_INFO_TYPE_VISIBLE, "shadow_": RenderingServer.VIEWPORT_RENDER_INFO_TYPE_SHADOW}
	for prefix: String in kinds.keys():
		var kind: int = kinds[prefix]
		info[prefix + "objects"] = RenderingServer.viewport_get_render_info(rid, kind, RenderingServer.VIEWPORT_RENDER_INFO_OBJECTS_IN_FRAME)
		info[prefix + "primitives"] = RenderingServer.viewport_get_render_info(rid, kind, RenderingServer.VIEWPORT_RENDER_INFO_PRIMITIVES_IN_FRAME)
		info[prefix + "draw_calls"] = RenderingServer.viewport_get_render_info(rid, kind, RenderingServer.VIEWPORT_RENDER_INFO_DRAW_CALLS_IN_FRAME)
	return info


## Triangles, surfaces and mesh instances of the visible geometry under node, as authored:
## a MultiMesh counts once per visible instance.
func _geometry_of(node: Node) -> Dictionary:
	var triangles: int = 0
	var surfaces: int = 0
	var instances: int = 0
	var nodes: Array[Node] = node.find_children("*", "GeometryInstance3D", true, false)
	for found: Node in nodes:
		var visual: GeometryInstance3D = found as GeometryInstance3D
		if not visual.is_visible_in_tree():
			continue
		var mesh: Mesh = null
		var copies: int = 1
		if visual is MeshInstance3D:
			mesh = (visual as MeshInstance3D).mesh
		elif visual is MultiMeshInstance3D:
			var multi: MultiMesh = (visual as MultiMeshInstance3D).multimesh
			if multi != null:
				mesh = multi.mesh
				copies = multi.visible_instance_count if multi.visible_instance_count >= 0 else multi.instance_count
		if mesh == null:
			continue
		instances += 1
		for surface: int in mesh.get_surface_count():
			surfaces += copies
			triangles += _surface_triangles(mesh, surface) * copies
	return {"triangles": triangles, "surfaces": surfaces, "mesh_instances": instances}


func _surface_triangles(mesh: Mesh, surface: int) -> int:
	var count: int = 0
	var primitive: int = Mesh.PRIMITIVE_TRIANGLES
	if mesh is ArrayMesh:
		var array_mesh: ArrayMesh = mesh as ArrayMesh
		primitive = array_mesh.surface_get_primitive_type(surface)
		count = array_mesh.surface_get_array_index_len(surface)
		if count == 0:
			count = array_mesh.surface_get_array_len(surface)
	else:
		var arrays: Array = mesh.surface_get_arrays(surface)
		var indices: Variant = arrays[Mesh.ARRAY_INDEX]
		var vertices: Variant = arrays[Mesh.ARRAY_VERTEX]
		if indices is PackedInt32Array and not (indices as PackedInt32Array).is_empty():
			count = (indices as PackedInt32Array).size()
		elif vertices is PackedVector3Array:
			count = (vertices as PackedVector3Array).size()
	if primitive == Mesh.PRIMITIVE_TRIANGLES:
		return int(count / 3.0)
	if primitive == Mesh.PRIMITIVE_TRIANGLE_STRIP:
		return maxi(count - 2, 0)
	return 0


## World bounds of the visible geometry under node; an empty AABB at its origin without any.
func _bounds_of(node: Node3D) -> AABB:
	var box: AABB = AABB()
	var started: bool = false
	for found: Node in node.find_children("*", "VisualInstance3D", true, false):
		var visual: VisualInstance3D = found as VisualInstance3D
		if not visual.is_visible_in_tree() or visual is Light3D or visual is ReflectionProbe:
			continue
		var local: AABB = visual.get_aabb()
		var placed: AABB = visual.global_transform * local
		box = placed if not started else box.merge(placed)
		started = true
	if not started:
		return AABB(node.global_position, Vector3.ZERO)
	return box


func _union() -> AABB:
	var box: AABB = AABB()
	var started: bool = false
	for index: int in holders.size():
		var one: AABB = _bounds_of(holders[index])
		box = one if not started else box.merge(one)
		started = true
	return box


func _corners(box: AABB) -> Array[Vector3]:
	var corners: Array[Vector3] = []
	for index: int in 8:
		corners.append(box.get_endpoint(index))
	return corners


## The transform of node relative to the root of a scene that is not in the tree.
func _chain_transform(node: Node) -> Transform3D:
	var placed: Transform3D = Transform3D.IDENTITY
	var current: Node = node
	while current != null:
		if current is Node3D:
			placed = (current as Node3D).transform * placed
		current = current.get_parent()
	return placed


func _vector(value: Variant, fallback: Vector3) -> Vector3:
	if value is Array and (value as Array).size() == 3:
		return Vector3(float(value[0]), float(value[1]), float(value[2]))
	if value is float or value is int:
		return Vector3(float(value), float(value), float(value))
	return fallback


func _color(value: Variant, fallback: Color) -> Color:
	if value is String:
		return Color.from_string(value, fallback)
	if value is Array and ((value as Array).size() == 3 or (value as Array).size() == 4):
		var alpha: float = float(value[3]) if (value as Array).size() == 4 else 1.0
		return Color(float(value[0]), float(value[1]), float(value[2]), alpha)
	return fallback


func _vector_json(value: Vector3) -> Array:
	return [snappedf(value.x, 0.001), snappedf(value.y, 0.001), snappedf(value.z, 0.001)]


func _aabb_json(box: AABB) -> Dictionary:
	return {"min": _vector_json(box.position), "max": _vector_json(box.end), "size": _vector_json(box.size)}
