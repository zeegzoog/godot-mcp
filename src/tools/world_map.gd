extends SceneTree
## godot_world_map: a top-down map of many scene files (streamed zones, say) placed at their saved
## transforms, with a summary of each scene, without running the game.
##
## The request is a JSON file named by the first user argument; the server checked its paths and
## values before writing it. The script works in three phases so that memory stays bounded by the
## scenes overlapping one tile, not by the whole world:
## 1. survey: each scene is loaded alone, measured and freed;
## 2. tiles: an orthographic camera looks straight down at one tile at a time; only the scenes
##    whose bounds touch the tile are instanced, the tile is read back into the map, and they
##    are freed again;
## 3. overlay: the map is drawn once more with each scene's bounds and file name.
## The map is saved as a PNG and the answer as JSON, both outside the project. Nothing is saved
## into the project.

## Frames a newly placed scene gets before it is measured (CSG and other deferred builders).
const SURVEY_FRAMES: int = 1
## Frames each tile is rendered before it is read back.
const TILE_FRAMES: int = 3
## Frames the overlay is rendered before it is read back.
const OVERLAY_FRAMES: int = 2
## Extra rounds of OVERLAY_FRAMES when the overlay comes back empty.
const OVERLAY_RETRIES: int = 10
## Pixels drawn around each tile and cut away.
const TILE_MARGIN_PX: int = 32
## Extra rounds of TILE_FRAMES when a tile reads back the same as the one before it.
const TILE_RETRIES: int = 5
## The largest side of a tile viewport, in pixels.
const MAX_TILE_PX: int = 2048
## Scenes instanced at once when the tile size is chosen automatically.
const MAX_LOADED: int = 9
const MAX_SIDE: int = 4096
const MAX_TOP_LEVEL: int = 40
## The longer side of the inline preview, in pixels.
const PREVIEW_SIDE: int = 1024
const BACKGROUND: Color = Color(0.11, 0.13, 0.17)
const COLLISION_COLOR: Color = Color(0.86, 0.62, 0.32)

enum Phase { SURVEY, TILES, OVERLAY, DONE }

var request: Dictionary = {}
var scenes: Array = []
var reports: Array[Dictionary] = []
## Per scene: the AABB the tiles test against (geometry, or collision in the collision view).
var render_bounds: Array[AABB] = []
var warnings: Array[String] = []
var phase: Phase = Phase.SURVEY
var wait_frames: int = 0
var index: int = 0
var survey_holder: Node3D = null
var survey_started_ms: int = 0

var viewport: SubViewport = null
var stage: Node3D = null
var camera: Camera3D = null
var loaded: Array[Node] = []

var view: String = "geometry"
var rect: Rect2 = Rect2()
var ppm: float = 1.0
var map: Image = null
var map_size: Vector2i = Vector2i.ZERO
var tile_metres: float = 0.0
var tile_px: int = 0
var tiles: Array[Rect2] = []
var tile_index: int = 0
var max_loaded_seen: int = 0
var overlay_viewport: SubViewport = null
var overlay_retries: int = 0
## Tiles (numbered from 1) that still read back black after their retries.
var undrawn_tiles: Array[int] = []
var drawing_problems: Array[String] = []
var tile_retries: int = 0
var last_tile_data: PackedByteArray = PackedByteArray()
var last_tile_had_scenes: bool = false
var camera_low: float = 0.0
var camera_high: float = 0.0
var last_drawn: int = -1


func _initialize() -> void:
	var args: PackedStringArray = OS.get_cmdline_user_args()
	if args.is_empty():
		_fail("no request file was given")
		return
	var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(args[0]))
	if not parsed is Dictionary:
		_fail("the request file is not a JSON object")
		return
	request = parsed
	scenes = request.get("scenes", [])
	view = str(request.get("view", "geometry"))


func _process(_delta: float) -> bool:
	if phase == Phase.DONE:
		return true
	if viewport == null:
		_make_viewport()
	if wait_frames > 0:
		# macOS stops drawing a covered or minimized window, and its viewports with it: when no
		# frame was drawn since the last tick, draw one, so a readback never gets an empty texture.
		if Engine.get_frames_drawn() == last_drawn:
			RenderingServer.force_draw(false, 0.0)
		last_drawn = Engine.get_frames_drawn()
		wait_frames -= 1
		return false
	match phase:
		Phase.SURVEY:
			_survey_step()
		Phase.TILES:
			_tile_step()
		Phase.OVERLAY:
			_overlay_step()
	return phase == Phase.DONE


# --- survey -------------------------------------------------------------------------------------


func _survey_step() -> void:
	if survey_holder != null:
		_measure(survey_holder, int(survey_holder.get_meta("scene_index")))
		survey_holder.free()
		survey_holder = null
		index += 1
	while index < scenes.size():
		print("GODOT_MCP_WORLD_MAP_PROGRESS survey %d/%d" % [index + 1, scenes.size()])
		survey_started_ms = Time.get_ticks_msec()
		var holder: Node3D = _instance(index)
		if holder != null:
			survey_holder = holder
			wait_frames = SURVEY_FRAMES
			return
		index += 1
	var problem: String = _plan_tiles()
	if not problem.is_empty():
		_fail(problem)
		return
	_start_tiles()


## Loads scene i, prepares it for a still picture and adds it to the stage. Null (with the
## scene's report filled in) when it does not load.
func _instance(i: int) -> Node3D:
	var path: String = str(scenes[i])
	var packed: PackedScene = ResourceLoader.load(path, "PackedScene") as PackedScene
	if packed == null:
		_report_failure(i, path, "could not be loaded as a PackedScene")
		return null
	var instance: Node = packed.instantiate()
	if instance == null:
		_report_failure(i, path, "could not be instantiated")
		return null
	_quiet(instance)
	var holder: Node3D = Node3D.new()
	holder.name = "Scene%d" % i
	holder.set_meta("scene_index", i)
	# Scripts still get _ready, but nothing processes, so the picture is the saved state.
	holder.process_mode = Node.PROCESS_MODE_DISABLED
	holder.add_child(instance)
	stage.add_child(holder)
	if camera != null:
		camera.make_current()
	return holder


func _report_failure(i: int, path: String, problem: String) -> void:
	while reports.size() <= i:
		reports.append({})
		render_bounds.append(AABB())
	reports[i] = {"scene": path, "error": problem}
	render_bounds[i] = AABB()


## Removes what would change the picture of the whole map or of the frame: environments,
## cameras that would take the view, 2D layers and controls drawn over the viewport.
func _quiet(instance: Node) -> void:
	var drop: Array[Node] = []
	for node: Node in _with_descendants(instance):
		if node is WorldEnvironment and node != instance:
			drop.append(node)
		elif node is Camera3D:
			(node as Camera3D).current = false
		elif node is CanvasLayer:
			(node as CanvasLayer).visible = false
		elif node is CanvasItem and not node.get_parent() is CanvasItem:
			(node as CanvasItem).visible = false
	for node: Node in drop:
		node.get_parent().remove_child(node)
		node.free()
	if instance is WorldEnvironment:
		(instance as WorldEnvironment).environment = null


func _measure(holder: Node3D, i: int) -> void:
	var instance: Node = holder.get_child(0)
	var path: String = str(scenes[i])
	var report: Dictionary = {"scene": path}
	var root: Dictionary = {"name": str(instance.name), "type": instance.get_class()}
	if instance is Node3D:
		var placed: Node3D = instance as Node3D
		root["position"] = _vector_json(placed.position)
		root["rotation"] = _vector_json(placed.rotation_degrees)
	report["root"] = root
	var top_level: Array[String] = []
	for child: Node in instance.get_children():
		if top_level.size() < MAX_TOP_LEVEL:
			top_level.append(str(child.name))
	report["top_level"] = top_level
	if instance.get_child_count() > MAX_TOP_LEVEL:
		report["top_level_more"] = instance.get_child_count() - MAX_TOP_LEVEL

	var mesh_instances: int = 0
	var multimesh_instances: int = 0
	var other_geometry: int = 0
	var collision_objects: int = 0
	var collision_shapes: int = 0
	var instanced: int = 0
	var layers: int = 0
	var geometry: AABB = AABB()
	var has_geometry_box: bool = false
	var collision: AABB = AABB()
	var has_collision_box: bool = false
	var unbounded_collision: bool = false
	for node: Node in _with_descendants(instance):
		if node != instance and not node.scene_file_path.is_empty():
			instanced += 1
		if node is GeometryInstance3D:
			var visual: GeometryInstance3D = node as GeometryInstance3D
			if not visual.is_visible_in_tree() or not _draws(visual):
				continue
			if visual is MeshInstance3D:
				mesh_instances += 1
			elif visual is MultiMeshInstance3D:
				multimesh_instances += 1
			else:
				other_geometry += 1
			var box: AABB = visual.global_transform * visual.get_aabb()
			geometry = box if not has_geometry_box else geometry.merge(box)
			has_geometry_box = true
			if visual is CSGShape3D and (visual as CSGShape3D).use_collision and (visual as CSGShape3D).is_root_shape():
				collision_objects += 1
				collision_shapes += 1
				layers |= (visual as CSGShape3D).collision_layer
				collision = box if not has_collision_box else collision.merge(box)
				has_collision_box = true
		elif node is GridMap:
			var grid: GridMap = node as GridMap
			var cells: Array[Vector3i] = grid.get_used_cells()
			if cells.is_empty():
				continue
			var grid_box: AABB = _grid_bounds(grid, cells)
			if grid.visible:
				other_geometry += 1
				geometry = grid_box if not has_geometry_box else geometry.merge(grid_box)
				has_geometry_box = true
			if grid.collision_layer != 0:
				collision_objects += 1
				collision_shapes += 1
				layers |= grid.collision_layer
				collision = grid_box if not has_collision_box else collision.merge(grid_box)
				has_collision_box = true
		elif node is CollisionObject3D:
			collision_objects += 1
			layers |= (node as CollisionObject3D).collision_layer
		elif node is CollisionShape3D:
			var shape_node: CollisionShape3D = node as CollisionShape3D
			if shape_node.disabled or shape_node.shape == null or not shape_node.get_parent() is CollisionObject3D:
				continue
			collision_shapes += 1
			if shape_node.shape is WorldBoundaryShape3D:
				unbounded_collision = true
				continue
			var shape_box: AABB = shape_node.global_transform * _shape_aabb(shape_node.shape)
			collision = shape_box if not has_collision_box else collision.merge(shape_box)
			has_collision_box = true
		elif node is CollisionPolygon3D:
			var polygon: CollisionPolygon3D = node as CollisionPolygon3D
			if polygon.disabled or polygon.polygon.is_empty() or not polygon.get_parent() is CollisionObject3D:
				continue
			collision_shapes += 1
			var polygon_box: AABB = polygon.global_transform * _polygon_aabb(polygon)
			collision = polygon_box if not has_collision_box else collision.merge(polygon_box)
			has_collision_box = true

	report["mesh_instances"] = mesh_instances
	report["multimesh_instances"] = multimesh_instances
	if other_geometry > 0:
		report["other_geometry"] = other_geometry
	report["collision_objects"] = collision_objects
	report["collision_shapes"] = collision_shapes
	report["instanced_scenes"] = instanced
	var layer_list: Array[int] = []
	for bit: int in 32:
		if layers & (1 << bit):
			layer_list.append(bit + 1)
	report["collision_layers"] = layer_list
	report["bounds"] = _aabb_json(geometry) if has_geometry_box else null
	report["collision_bounds"] = _aabb_json(collision) if has_collision_box else null
	if unbounded_collision:
		report["unbounded_collision"] = true
	report["has_geometry"] = has_geometry_box
	report["has_collision"] = collision_shapes > 0
	report["load_ms"] = Time.get_ticks_msec() - survey_started_ms
	while reports.size() <= i:
		reports.append({})
		render_bounds.append(AABB())
	reports[i] = report
	var bounds: AABB = AABB()
	var known: bool = false
	if view == "collision":
		known = has_collision_box
		bounds = collision
	else:
		known = has_geometry_box
		bounds = geometry
	if not known:
		# Still drawn as a point, so the scene shows on the map where its root stands.
		bounds = AABB((instance as Node3D).global_position if instance is Node3D else Vector3.ZERO, Vector3.ZERO)
	render_bounds[i] = bounds
	report["_known"] = known


func _draws(visual: GeometryInstance3D) -> bool:
	if visual is MeshInstance3D:
		return (visual as MeshInstance3D).mesh != null
	if visual is MultiMeshInstance3D:
		var multi: MultiMesh = (visual as MultiMeshInstance3D).multimesh
		return multi != null and multi.mesh != null and multi.instance_count > 0 and multi.visible_instance_count != 0
	if visual is CSGShape3D:
		return (visual as CSGShape3D).is_root_shape()
	return visual.get_aabb().has_volume() or visual.get_aabb().size != Vector3.ZERO


func _grid_bounds(grid: GridMap, cells: Array[Vector3i]) -> AABB:
	var low: Vector3i = cells[0]
	var high: Vector3i = cells[0]
	for cell: Vector3i in cells:
		low = low.min(cell)
		high = high.max(cell)
	var half: Vector3 = grid.cell_size * 0.5
	var box: AABB = AABB(grid.map_to_local(low) - half, Vector3.ZERO)
	box = box.expand(grid.map_to_local(high) + half)
	return grid.global_transform * box


func _shape_aabb(shape: Shape3D) -> AABB:
	if shape is BoxShape3D:
		var size: Vector3 = (shape as BoxShape3D).size
		return AABB(-size * 0.5, size)
	if shape is SphereShape3D:
		var radius: float = (shape as SphereShape3D).radius
		return AABB(-Vector3.ONE * radius, Vector3.ONE * radius * 2.0)
	if shape is CapsuleShape3D:
		var capsule: CapsuleShape3D = shape as CapsuleShape3D
		return AABB(Vector3(-capsule.radius, -capsule.height * 0.5, -capsule.radius), Vector3(capsule.radius * 2.0, capsule.height, capsule.radius * 2.0))
	if shape is CylinderShape3D:
		var cylinder: CylinderShape3D = shape as CylinderShape3D
		return AABB(Vector3(-cylinder.radius, -cylinder.height * 0.5, -cylinder.radius), Vector3(cylinder.radius * 2.0, cylinder.height, cylinder.radius * 2.0))
	if shape is HeightMapShape3D:
		var height_map: HeightMapShape3D = shape as HeightMapShape3D
		var width: float = float(maxi(height_map.map_width - 1, 0))
		var depth: float = float(maxi(height_map.map_depth - 1, 0))
		var low: float = height_map.get_min_height()
		var high: float = height_map.get_max_height()
		return AABB(Vector3(-width * 0.5, low, -depth * 0.5), Vector3(width, high - low, depth))
	if shape is ConcavePolygonShape3D:
		return _points_aabb((shape as ConcavePolygonShape3D).get_faces())
	if shape is ConvexPolygonShape3D:
		return _points_aabb((shape as ConvexPolygonShape3D).points)
	if shape is SeparationRayShape3D:
		var ray: float = (shape as SeparationRayShape3D).length
		return AABB(Vector3.ZERO, Vector3(0.0, 0.0, ray))
	var debug: ArrayMesh = shape.get_debug_mesh()
	return debug.get_aabb() if debug != null else AABB()


func _points_aabb(points: PackedVector3Array) -> AABB:
	if points.is_empty():
		return AABB()
	var box: AABB = AABB(points[0], Vector3.ZERO)
	for point: Vector3 in points:
		box = box.expand(point)
	return box


func _polygon_aabb(polygon: CollisionPolygon3D) -> AABB:
	var box: AABB = AABB()
	var started: bool = false
	var half: float = polygon.depth * 0.5
	for point: Vector2 in polygon.polygon:
		for z: float in [-half, half]:
			var corner: Vector3 = Vector3(point.x, point.y, z)
			box = AABB(corner, Vector3.ZERO) if not started else box.expand(corner)
			started = true
	return box


# --- tiles --------------------------------------------------------------------------------------


func _plan_tiles() -> String:
	var known_any: bool = false
	var union: Rect2 = Rect2()
	for i: int in reports.size():
		if reports[i].has("error") or not bool(reports[i].get("_known", false)):
			continue
		var flat: Rect2 = _flat(render_bounds[i])
		union = flat if not known_any else union.merge(flat)
		known_any = true
	var given: Variant = request.get("rect")
	if given is Array and (given as Array).size() == 4:
		rect = Rect2(float(given[0]), float(given[1]), float(given[2]) - float(given[0]), float(given[3]) - float(given[1]))
	elif known_any:
		var margin: float = maxf(maxf(union.size.x, union.size.y) * 0.02, 1.0)
		rect = union.grow(margin)
	else:
		return "none of the scenes has %s to draw, so the map has no extent; pass rect to draw the area anyway" % ("collision" if view == "collision" else "visible geometry")
	if request.has("pixels_per_metre"):
		ppm = float(request["pixels_per_metre"])
	else:
		ppm = float(request.get("default_side", 2048)) / maxf(rect.size.x, rect.size.y)
	map_size = Vector2i(maxi(int(ceil(rect.size.x * ppm)), 1), maxi(int(ceil(rect.size.y * ppm)), 1))
	if map_size.x > MAX_SIDE or map_size.y > MAX_SIDE:
		return "the map would be %d x %d pixels, more than %d on a side; lower pixels_per_metre (at most %.3f for this rect) or narrow rect" % [map_size.x, map_size.y, MAX_SIDE, float(MAX_SIDE) / maxf(rect.size.x, rect.size.y)]

	if request.has("tile_metres"):
		tile_metres = float(request["tile_metres"])
		if tile_metres * ppm > MAX_TILE_PX:
			tile_metres = MAX_TILE_PX / ppm
			warnings.append("tile_metres was lowered to %.1f so a tile stays within %d pixels" % [tile_metres, MAX_TILE_PX])
	else:
		tile_metres = minf(1024.0 / ppm, maxf(rect.size.x, rect.size.y))
		while tile_metres * ppm > 64.0 and _max_overlap(tile_metres) > MAX_LOADED:
			tile_metres *= 0.5
	tile_px = maxi(int(ceil(tile_metres * ppm)), 1)
	# Whole pixels per tile, so the tiles meet exactly.
	tile_metres = tile_px / ppm
	var columns: int = int(ceil(rect.size.x / tile_metres))
	var rows: int = int(ceil(rect.size.y / tile_metres))
	for row: int in rows:
		for column: int in columns:
			tiles.append(Rect2(rect.position + Vector2(column, row) * tile_metres, Vector2(tile_metres, tile_metres)))
	map = Image.create_empty(map_size.x, map_size.y, false, Image.FORMAT_RGBA8)
	map.fill(BACKGROUND)
	return ""


func _max_overlap(size: float) -> int:
	var columns: int = int(ceil(rect.size.x / size))
	var rows: int = int(ceil(rect.size.y / size))
	var most: int = 0
	for row: int in rows:
		for column: int in columns:
			var tile: Rect2 = Rect2(rect.position + Vector2(column, row) * size, Vector2(size, size))
			most = maxi(most, _scenes_in(tile).size())
	return most


func _scenes_in(tile: Rect2) -> Array[int]:
	var found: Array[int] = []
	for i: int in reports.size():
		if reports[i].has("error"):
			continue
		var flat: Rect2 = _flat(render_bounds[i])
		# Inclusive, so a flat or point scene on the edge of a tile is still drawn.
		if flat.position.x <= tile.end.x and flat.end.x >= tile.position.x and flat.position.y <= tile.end.y and flat.end.y >= tile.position.y:
			found.append(i)
	return found


func _start_tiles() -> void:
	phase = Phase.TILES
	tile_index = 0
	# Each tile is drawn with a margin that is cut away, so screen-space effects (refraction,
	# depth fades) near its edge see the neighbouring ground instead of nothing.
	viewport.size = Vector2i(tile_px + 2 * TILE_MARGIN_PX, tile_px + 2 * TILE_MARGIN_PX)
	_add_lighting()
	camera = Camera3D.new()
	camera.name = "MapCamera"
	camera.projection = Camera3D.PROJECTION_ORTHOGONAL
	camera.keep_aspect = Camera3D.KEEP_HEIGHT
	camera.size = float(viewport.size.y) / ppm
	# Looking down -Y with the top of the picture towards -Z.
	camera.basis = Basis.from_euler(Vector3(-PI * 0.5, 0.0, 0.0))
	stage.add_child(camera)
	var placed: bool = false
	for i: int in reports.size():
		if reports[i].has("error"):
			continue
		camera_low = render_bounds[i].position.y if not placed else minf(camera_low, render_bounds[i].position.y)
		camera_high = render_bounds[i].end.y if not placed else maxf(camera_high, render_bounds[i].end.y)
		placed = true
	camera.near = 0.05
	camera.far = maxf(camera_high - camera_low, 1.0) + 10.0
	# Shaders that animate with TIME stand still, so neighbouring tiles agree.
	Engine.time_scale = 0.0
	camera.make_current()
	viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	_load_tile()


func _load_tile() -> void:
	while tile_index < tiles.size():
		var tile: Rect2 = tiles[tile_index]
		var inside: Array[int] = _scenes_in(tile.grow(TILE_MARGIN_PX / ppm))
		# An empty tile is rendered too, so its background matches the tone-mapped one of the rest.
		max_loaded_seen = maxi(max_loaded_seen, inside.size())
		print("GODOT_MCP_WORLD_MAP_PROGRESS tile %d/%d" % [tile_index + 1, tiles.size()])
		for i: int in inside:
			var holder: Node3D = _instance(i)
			if holder == null:
				continue
			if view == "collision":
				_show_collision(holder)
			loaded.append(holder)
		# The same height, near and far for every tile: a shader that reads depth (shallow water,
		# say) would otherwise change from tile to tile and leave seams.
		var center: Vector2 = tile.get_center()
		camera.position = Vector3(center.x, camera_high + 5.0, center.y)
		camera.make_current()
		wait_frames = TILE_FRAMES
		return
	_start_overlay()


func _tile_step() -> void:
	var image: Image = viewport.get_texture().get_image()
	if image == null or image.is_empty():
		_fail("the viewport gave no image; a headless Godot does not render")
		return
	# Opaque whatever alpha the renderer left in the target (Compatibility may leave 0).
	image.convert(Image.FORMAT_RGB8)
	_simulate_undrawn(image)
	# A black readback is a target nothing was drawn into (the background is never black): the
	# window could not draw, as when the display sleeps. Draw again, then give up on the tile.
	var undrawn: bool = _undrawn(image)
	if undrawn and tile_retries < TILE_RETRIES:
		tile_retries += 1
		RenderingServer.force_draw(false, 0.0)
		wait_frames = TILE_FRAMES
		return
	if undrawn:
		undrawn_tiles.append(tile_index + 1)
	# A readback identical to the last tile's, with scenes in either, is a texture that was not
	# drawn again after the camera moved: draw and wait once more before taking it.
	var data: PackedByteArray = image.get_data()
	var has_scenes: bool = not loaded.is_empty()
	if (has_scenes or last_tile_had_scenes) and data == last_tile_data and tile_retries < TILE_RETRIES:
		tile_retries += 1
		RenderingServer.force_draw(false, 0.0)
		wait_frames = TILE_FRAMES
		return
	if tile_retries >= TILE_RETRIES and data == last_tile_data and not undrawn:
		warnings.append("tile %d drew the same picture as the tile before it after %d retries; that part of the map may be wrong" % [tile_index + 1, TILE_RETRIES])
	last_tile_data = data
	last_tile_had_scenes = has_scenes
	tile_retries = 0
	image.convert(Image.FORMAT_RGBA8)
	var tile: Rect2 = tiles[tile_index]
	var at: Vector2i = Vector2i(roundi((tile.position.x - rect.position.x) * ppm), roundi((tile.position.y - rect.position.y) * ppm))
	map.blit_rect(image, Rect2i(TILE_MARGIN_PX, TILE_MARGIN_PX, tile_px, tile_px), at)
	for holder: Node in loaded:
		holder.free()
	loaded.clear()
	tile_index += 1
	_load_tile()


func _add_lighting() -> void:
	var world: WorldEnvironment = WorldEnvironment.new()
	var made: Environment = Environment.new()
	made.background_mode = Environment.BG_COLOR
	made.background_color = BACKGROUND
	made.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	made.ambient_light_color = Color(0.8, 0.82, 0.88)
	made.ambient_light_energy = 0.4
	made.tonemap_mode = Environment.TONE_MAPPER_FILMIC
	world.environment = made
	stage.add_child(world)
	# High and from the north-west, without shadows: a tile does not hold the casters next to it.
	var sun: DirectionalLight3D = DirectionalLight3D.new()
	sun.rotation_degrees = Vector3(-60.0, -135.0, 0.0)
	sun.light_energy = 0.8
	sun.shadow_enabled = false
	stage.add_child(sun)


## Hides the visible geometry under holder and adds solid stand-ins for its collision shapes.
func _show_collision(holder: Node3D) -> void:
	var material: StandardMaterial3D = StandardMaterial3D.new()
	material.albedo_color = COLLISION_COLOR
	material.roughness = 1.0
	material.cull_mode = BaseMaterial3D.CULL_DISABLED
	var lines: StandardMaterial3D = StandardMaterial3D.new()
	lines.albedo_color = COLLISION_COLOR
	lines.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	var stand_ins: Array[MeshInstance3D] = []
	for node: Node in _with_descendants(holder):
		if node is CSGShape3D:
			# Hiding a child shape would take it out of the combined shape: only roots change.
			var csg: CSGShape3D = node as CSGShape3D
			if csg.is_root_shape():
				if csg.use_collision:
					csg.material_override = material
				else:
					csg.visible = false
			continue
		if node is GeometryInstance3D:
			(node as GeometryInstance3D).visible = false
		elif node is GridMap:
			var grid: GridMap = node as GridMap
			if grid.collision_layer == 0:
				grid.visible = false
		elif node is CollisionShape3D:
			var shape_node: CollisionShape3D = node as CollisionShape3D
			if shape_node.disabled or shape_node.shape == null or not shape_node.get_parent() is CollisionObject3D:
				continue
			var mesh: Mesh = _shape_mesh(shape_node.shape)
			var solid: bool = mesh != null
			if mesh == null:
				mesh = shape_node.shape.get_debug_mesh()
			if mesh == null:
				continue
			var stand_in: MeshInstance3D = MeshInstance3D.new()
			stand_in.mesh = mesh
			stand_in.material_override = material if solid else lines
			stand_in.transform = shape_node.global_transform
			stand_ins.append(stand_in)
	# The holder sits at the origin, so a global transform is also the stand-in's own.
	for stand_in: MeshInstance3D in stand_ins:
		holder.add_child(stand_in)


func _shape_mesh(shape: Shape3D) -> Mesh:
	if shape is BoxShape3D:
		var box: BoxMesh = BoxMesh.new()
		box.size = (shape as BoxShape3D).size
		return box
	if shape is SphereShape3D:
		var sphere: SphereMesh = SphereMesh.new()
		sphere.radius = (shape as SphereShape3D).radius
		sphere.height = sphere.radius * 2.0
		return sphere
	if shape is CapsuleShape3D:
		var capsule: CapsuleMesh = CapsuleMesh.new()
		capsule.radius = (shape as CapsuleShape3D).radius
		capsule.height = (shape as CapsuleShape3D).height
		return capsule
	if shape is CylinderShape3D:
		var cylinder: CylinderMesh = CylinderMesh.new()
		cylinder.top_radius = (shape as CylinderShape3D).radius
		cylinder.bottom_radius = cylinder.top_radius
		cylinder.height = (shape as CylinderShape3D).height
		return cylinder
	if shape is WorldBoundaryShape3D:
		var plane: PlaneMesh = PlaneMesh.new()
		plane.size = Vector2(1.0, 1.0) * maxf(rect.size.x, rect.size.y) * 4.0
		return plane
	if shape is ConcavePolygonShape3D:
		var faces: PackedVector3Array = (shape as ConcavePolygonShape3D).get_faces()
		if faces.is_empty():
			return null
		return _triangles(faces)
	if shape is HeightMapShape3D:
		return _height_map_mesh(shape as HeightMapShape3D)
	return null


func _triangles(faces: PackedVector3Array) -> ArrayMesh:
	var arrays: Array = []
	arrays.resize(Mesh.ARRAY_MAX)
	arrays[Mesh.ARRAY_VERTEX] = faces
	var normals: PackedVector3Array = PackedVector3Array()
	normals.resize(faces.size())
	for face: int in int(faces.size() / 3.0):
		var a: Vector3 = faces[face * 3]
		var normal: Vector3 = (faces[face * 3 + 1] - a).cross(faces[face * 3 + 2] - a).normalized()
		if normal.y < 0.0:
			normal = -normal
		for corner: int in 3:
			normals[face * 3 + corner] = normal
	arrays[Mesh.ARRAY_NORMAL] = normals
	var mesh: ArrayMesh = ArrayMesh.new()
	mesh.add_surface_from_arrays(Mesh.PRIMITIVE_TRIANGLES, arrays)
	return mesh


func _height_map_mesh(shape: HeightMapShape3D) -> ArrayMesh:
	var width: int = shape.map_width
	var depth: int = shape.map_depth
	var data: PackedFloat32Array = shape.map_data
	if width < 2 or depth < 2 or data.size() < width * depth:
		return null
	var faces: PackedVector3Array = PackedVector3Array()
	var origin: Vector2 = Vector2(-(width - 1) * 0.5, -(depth - 1) * 0.5)
	for z: int in depth - 1:
		for x: int in width - 1:
			var a: Vector3 = Vector3(origin.x + x, data[z * width + x], origin.y + z)
			var b: Vector3 = Vector3(origin.x + x + 1, data[z * width + x + 1], origin.y + z)
			var c: Vector3 = Vector3(origin.x + x, data[(z + 1) * width + x], origin.y + z + 1)
			var d: Vector3 = Vector3(origin.x + x + 1, data[(z + 1) * width + x + 1], origin.y + z + 1)
			faces.append_array(PackedVector3Array([a, b, c, b, d, c]))
	return _triangles(faces)


# --- overlay ------------------------------------------------------------------------------------


func _start_overlay() -> void:
	var problem: String = _check_drawn()
	if not problem.is_empty():
		_fail(problem)
		return
	for node: Node in stage.get_children():
		node.free()
	viewport.render_target_update_mode = SubViewport.UPDATE_DISABLED
	if not bool(request.get("labels", true)):
		_finish(map)
		return
	phase = Phase.OVERLAY
	overlay_viewport = SubViewport.new()
	overlay_viewport.name = "GodotMcpWorldMapOverlay"
	overlay_viewport.size = map_size
	overlay_viewport.transparent_bg = false
	overlay_viewport.disable_3d = true
	overlay_viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
	root.add_child(overlay_viewport)
	var picture: TextureRect = TextureRect.new()
	picture.texture = ImageTexture.create_from_image(map)
	picture.size = Vector2(map_size)
	overlay_viewport.add_child(picture)
	var marks: Control = Control.new()
	marks.size = Vector2(map_size)
	marks.draw.connect(_draw_marks.bind(marks))
	overlay_viewport.add_child(marks)
	wait_frames = OVERLAY_FRAMES


func _draw_marks(marks: Control) -> void:
	var font: Font = ThemeDB.fallback_font
	for i: int in reports.size():
		var report: Dictionary = reports[i]
		if report.has("error"):
			continue
		var flat: Rect2 = _flat(render_bounds[i])
		var box: Rect2 = Rect2((flat.position - rect.position) * ppm, flat.size * ppm)
		var missing: bool = not bool(report.get("has_collision", false)) or not bool(report.get("has_geometry", false))
		var line: Color = Color(1.0, 0.35, 0.3, 0.75) if missing else Color(1.0, 1.0, 1.0, 0.35)
		if box.size.x < 2.0 and box.size.y < 2.0:
			marks.draw_circle(box.position, 3.0, line)
		else:
			marks.draw_rect(box, line, false, 1.0)
		var label: String = str(report["scene"]).get_file().get_basename()
		if not bool(report.get("has_collision", false)):
			label += " (no collision)"
		elif not bool(report.get("has_geometry", false)):
			label += " (no geometry)"
		var size: int = clampi(int(minf(box.size.x, 160.0) / 9.0), 9, 16)
		var at: Vector2 = box.position + Vector2(3.0, float(size) + 2.0)
		var text: Color = Color(line, minf(line.a + 0.25, 1.0))
		marks.draw_string_outline(font, at, label, HORIZONTAL_ALIGNMENT_LEFT, -1.0, size, 3, Color(0, 0, 0, 0.6))
		marks.draw_string(font, at, label, HORIZONTAL_ALIGNMENT_LEFT, -1.0, size, text)


func _overlay_step() -> void:
	var image: Image = overlay_viewport.get_texture().get_image()
	if image == null or image.is_empty():
		_fail("the overlay viewport gave no image; a headless Godot does not render")
		return
	image.convert(Image.FORMAT_RGB8)
	if _undrawn(image):
		if overlay_retries < OVERLAY_RETRIES:
			overlay_retries += 1
			wait_frames = OVERLAY_FRAMES
			return
		warnings.append("the labels could not be drawn (the overlay rendered nothing); the map is returned without them")
		_finish(map)
		return
	_finish(image)


## Whether the image is what a viewport nothing was drawn into gives: every sampled pixel black.
func _undrawn(image: Image) -> bool:
	return _uniform(image) and image.get_pixel(0, 0).get_luminance() <= 0.0


## Whether every sampled pixel of the image has the same colour.
func _uniform(image: Image) -> bool:
	var first: Color = image.get_pixel(0, 0)
	var step: Vector2i = Vector2i(maxi(int(image.get_width() / 32.0), 1), maxi(int(image.get_height() / 32.0), 1))
	for y: int in range(0, image.get_height(), step.y):
		for x: int in range(0, image.get_width(), step.x):
			if not image.get_pixel(x, y).is_equal_approx(first):
				return false
	return true


## Test hook: test_undrawn "all" in the request makes every tile read back black, "first" only the
## first one, as a window that cannot draw does. The server never sets it; only the end-to-end
## tests, which run this script directly, do.
func _simulate_undrawn(image: Image) -> void:
	var mode: String = str(request.get("test_undrawn", ""))
	if mode == "all" or (mode == "first" and tile_index == 0):
		image.fill(Color.BLACK)


## Refuses a map Godot did not draw, and names the tiles it did not draw.
func _check_drawn() -> String:
	if not undrawn_tiles.is_empty() and undrawn_tiles.size() == tiles.size():
		return "Godot drew none of the %d tiles: every one read back black after %d retries. The window could not draw, as when the display is asleep or the session is locked" % [tiles.size(), TILE_RETRIES]
	if not undrawn_tiles.is_empty():
		var listed: PackedStringArray = PackedStringArray()
		for number: int in undrawn_tiles:
			listed.append(str(number))
		var line: String = "tiles %s of %d were not drawn (black after %d retries); those parts of the map are missing" % [", ".join(listed), tiles.size(), TILE_RETRIES]
		warnings.append(line)
		drawing_problems.append(line)
	var expected: bool = false
	for i: int in reports.size():
		var drawn_key: String = "has_collision" if view == "collision" else "has_geometry"
		if bool(reports[i].get(drawn_key, false)) and _flat(render_bounds[i]).intersects(rect, true):
			expected = true
			break
	if expected and _uniform(map):
		return "the map came out one colour although scenes with %s lie in its rect; Godot drew nothing of them" % ("collision" if view == "collision" else "visible geometry")
	return ""


# --- answer -------------------------------------------------------------------------------------


func _finish(image: Image) -> void:
	# The map is opaque: an RGB PNG, so no viewer shows a stray alpha as an empty picture.
	image.convert(Image.FORMAT_RGB8)
	var saved: Error = image.save_png(str(request.get("output", "")))
	if saved != OK:
		_fail("the PNG could not be saved: " + error_string(saved))
		return
	# A small JPEG the server can return inline: a full map of a large world does not fit a reply.
	var preview: Image = image.duplicate() as Image
	var longest: int = maxi(preview.get_width(), preview.get_height())
	if longest > PREVIEW_SIDE:
		var factor: float = float(PREVIEW_SIDE) / float(longest)
		preview.resize(maxi(roundi(preview.get_width() * factor), 1), maxi(roundi(preview.get_height() * factor), 1), Image.INTERPOLATE_CUBIC)
	preview.convert(Image.FORMAT_RGB8)
	var preview_size: Vector2i = preview.get_size()
	if not str(request.get("preview", "")).is_empty():
		var preview_saved: Error = preview.save_jpg(str(request["preview"]), 0.85)
		if preview_saved != OK:
			warnings.append("the preview JPEG could not be saved: " + error_string(preview_saved))
	var no_collision: int = 0
	var no_geometry: int = 0
	var failed: int = 0
	var problems: Array[String] = drawing_problems.duplicate()
	for report: Dictionary in reports:
		report.erase("_known")
		if report.has("error"):
			failed += 1
			problems.append("%s: %s" % [report["scene"], report["error"]])
			continue
		var collision: bool = bool(report.get("has_collision", false))
		var geometry: bool = bool(report.get("has_geometry", false))
		if not collision:
			no_collision += 1
		if not geometry:
			no_geometry += 1
		if not collision and not geometry:
			problems.append("%s: no collision and no visible geometry" % report["scene"])
		elif not collision:
			problems.append("%s: no collision (visible geometry only)" % report["scene"])
		elif not geometry:
			problems.append("%s: no visible geometry (collision only)" % report["scene"])
	var answer: Dictionary = {
		"image": {
			"width": map_size.x,
			"height": map_size.y,
			"pixels_per_metre": snappedf(ppm, 0.0001),
			"rect": [snappedf(rect.position.x, 0.001), snappedf(rect.position.y, 0.001), snappedf(rect.end.x, 0.001), snappedf(rect.end.y, 0.001)],
			"up": "-Z",
			"pixel_to_world": "x = rect[0] + px / pixels_per_metre, z = rect[1] + py / pixels_per_metre",
			"view": view,
			"tiles": tiles.size(),
			"tile_metres": snappedf(tile_metres, 0.001),
			"max_scenes_per_tile": max_loaded_seen,
			"preview": [preview_size.x, preview_size.y],
		},
		"totals": {"scenes": reports.size(), "no_collision": no_collision, "no_geometry": no_geometry, "failed": failed},
		"problems": problems,
		"scenes": reports,
		"renderer": str(ProjectSettings.get_setting("rendering/renderer/rendering_method", "")),
		"warnings": warnings,
	}
	_write(answer)
	phase = Phase.DONE
	quit()


func _fail(problem: String) -> void:
	_write({"error": problem, "warnings": warnings})
	phase = Phase.DONE
	quit(1)


func _write(answer: Dictionary) -> void:
	var file: FileAccess = FileAccess.open(str(request.get("answer", "")), FileAccess.WRITE)
	if file == null:
		printerr("godot_world_map: the answer file could not be written")
		return
	file.store_string(JSON.stringify(answer))
	file.close()


# --- helpers ------------------------------------------------------------------------------------


func _make_viewport() -> void:
	viewport = SubViewport.new()
	viewport.name = "GodotMcpWorldMap"
	viewport.size = Vector2i(16, 16)
	viewport.own_world_3d = true
	viewport.transparent_bg = false
	viewport.render_target_update_mode = SubViewport.UPDATE_DISABLED
	viewport.msaa_3d = root.msaa_3d
	viewport.screen_space_aa = root.screen_space_aa
	root.add_child(viewport)
	stage = Node3D.new()
	stage.name = "Stage"
	viewport.add_child(stage)


func _with_descendants(node: Node) -> Array[Node]:
	var nodes: Array[Node] = [node]
	nodes.append_array(node.find_children("*", "", true, false))
	return nodes


## The ground-plane footprint of a box: x across, z down.
func _flat(box: AABB) -> Rect2:
	return Rect2(box.position.x, box.position.z, box.size.x, box.size.z)


func _vector_json(value: Vector3) -> Array:
	return [snappedf(value.x, 0.001), snappedf(value.y, 0.001), snappedf(value.z, 0.001)]


func _aabb_json(box: AABB) -> Dictionary:
	return {"min": _vector_json(box.position), "max": _vector_json(box.end), "size": _vector_json(box.size)}
