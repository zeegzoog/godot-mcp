extends RefCounted
## Raycasts in the running game's 3D physics world. Nothing moves: the rays are queries against
## the physics server as it stands, so a node teleported for a measurement is no longer needed.
## A ray goes between two points, or straight down at (x, z); a grid is many down rays over an
## area, answered as a table of ground heights.

const R := preload("rt_result.gd")
const MAX_RAYS := 1024
const MAX_GRID := 4096
const DEFAULT_TOP := 1000.0
const DEFAULT_BOTTOM := -1000.0
const ALL_LAYERS := 0xFFFFFFFF


static func methods() -> Dictionary:
    return {"game.probe": {"fn": probe}}


static func probe(rt: Node, params: Dictionary) -> Dictionary:
    var has_rays := params.has("rays")
    var has_grid := params.has("grid")
    if has_rays == has_grid:
        return R.err(R.BAD_PARAMS, "Pass rays (a list of rays) or grid (an area of down rays), not both and not neither.")
    var options := _options(rt, params)
    if R.is_err(options):
        return options
    var space: PhysicsDirectSpaceState3D = options["space"]
    if has_rays:
        var rays := _parse_rays(params.get("rays"), options)
        if R.is_err(rays):
            return rays
        var results: Array = []
        var hits := 0
        for index in (rays["rays"] as Array).size():
            var ray: Array = rays["rays"][index]
            var answer := _cast(rt, space, ray[0], ray[1], options)
            answer["index"] = index
            if bool(answer["hit"]):
                hits += 1
            results.append(answer)
        return R.ok({"rays": results, "hits": hits, "missed": results.size() - hits, "mask": int(options["mask"])})
    return _grid(rt, space, params.get("grid"), options, bool(params.get("detail", false)))


## The settings every ray of one call shares, and the physics space they are cast in.
static func _options(rt: Node, params: Dictionary) -> Dictionary:
    var mask := ALL_LAYERS
    if params.has("mask"):
        var raw: Variant = _whole_number(params.get("mask"))
        if raw == null or int(raw) < 0 or int(raw) > ALL_LAYERS:
            return R.err(R.BAD_PARAMS, "mask must be an integer from 0 to 4294967295 (bit 0 is collision layer 1).")
        mask = int(raw)
    var top := DEFAULT_TOP
    var bottom := DEFAULT_BOTTOM
    for key: String in ["top", "bottom"]:
        if params.has(key):
            var value: Variant = params.get(key)
            if not (value is int or value is float):
                return R.err(R.BAD_PARAMS, "%s must be a number." % key)
            if key == "top":
                top = float(value)
            else:
                bottom = float(value)
    if bottom >= top:
        return R.err(R.BAD_PARAMS, "bottom (%s) must be below top (%s)." % [bottom, top])
    for key: String in ["areas", "bodies", "detail"]:
        if params.has(key) and not params.get(key) is bool:
            return R.err(R.BAD_PARAMS, "%s must be true or false." % key)
    var exclude: Array[RID] = []
    var excluded: Array = []
    var raw_exclude: Variant = params.get("exclude", [])
    if not raw_exclude is Array:
        return R.err(R.BAD_PARAMS, "exclude must be a list of node paths.")
    for entry: Variant in raw_exclude:
        var node: Node = rt.find_node(str(entry))
        if node == null:
            return R.err(R.NOT_FOUND, "exclude: no node at '%s'." % str(entry))
        if not node is CollisionObject3D:
            return R.err(R.BAD_PARAMS, "exclude: '%s' is a %s, not a CollisionObject3D (a body or an area)." % [str(entry), node.get_class()])
        exclude.append((node as CollisionObject3D).get_rid())
        excluded.append(rt.path_of(node))
    var world_path := str(params.get("world", "")).strip_edges()
    var source: Node = rt
    if not world_path.is_empty():
        source = rt.find_node(world_path)
        if source == null:
            return R.err(R.NOT_FOUND, "world: no node at '%s'." % world_path)
    var viewport: Viewport = (source as Viewport) if source is Viewport else source.get_viewport()
    var world: World3D = viewport.find_world_3d() if viewport != null else null
    if world == null or world.direct_space_state == null:
        return R.err(R.UNSUPPORTED, "There is no 3D physics world to probe here. Name a node in a 3D scene with world.")
    return {
        "space": world.direct_space_state,
        "mask": mask,
        "top": top,
        "bottom": bottom,
        "areas": bool(params.get("areas", false)),
        "bodies": bool(params.get("bodies", true)),
        "exclude": exclude,
        "excluded": excluded,
    }


static func _parse_rays(raw: Variant, options: Dictionary) -> Dictionary:
    if not raw is Array or (raw as Array).is_empty():
        return R.err(R.BAD_PARAMS, "rays must be a non-empty list.")
    if (raw as Array).size() > MAX_RAYS:
        return R.err(R.BAD_PARAMS, "At most %d rays in one call; use grid for an area of ground heights." % MAX_RAYS)
    var rays: Array = []
    for index in (raw as Array).size():
        var entry: Variant = raw[index]
        if not entry is Dictionary:
            return R.err(R.BAD_PARAMS, "rays[%d] must be an object: {from, to} or {x, z}." % index)
        var ray: Dictionary = entry
        if ray.has("from") or ray.has("to"):
            var from: Variant = _vector3(ray.get("from"))
            var to: Variant = _vector3(ray.get("to"))
            if from == null or to == null:
                return R.err(R.BAD_PARAMS, "rays[%d]: from and to must both be [x, y, z]." % index)
            if (from as Vector3).is_equal_approx(to):
                return R.err(R.BAD_PARAMS, "rays[%d]: from and to are the same point." % index)
            rays.append([from, to])
        elif ray.has("x") and ray.has("z"):
            if not _is_number(ray["x"]) or not _is_number(ray["z"]):
                return R.err(R.BAD_PARAMS, "rays[%d]: x and z must be numbers." % index)
            var x := float(ray["x"])
            var z := float(ray["z"])
            rays.append([Vector3(x, float(options["top"]), z), Vector3(x, float(options["bottom"]), z)])
        else:
            return R.err(R.BAD_PARAMS, "rays[%d] must have from and to, or x and z for a ray straight down." % index)
    return {"rays": rays}


static func _grid(rt: Node, space: PhysicsDirectSpaceState3D, raw: Variant, options: Dictionary, detail: bool) -> Dictionary:
    if not raw is Dictionary:
        return R.err(R.BAD_PARAMS, "grid must be an object: {min: [x, z], max: [x, z], step} or {min, max, count: [columns, rows]}.")
    var grid: Dictionary = raw
    var low: Variant = _vector2(grid.get("min"))
    var high: Variant = _vector2(grid.get("max"))
    if low == null or high == null:
        return R.err(R.BAD_PARAMS, "grid.min and grid.max must be [x, z].")
    var lo: Vector2 = low
    var hi: Vector2 = high
    if hi.x < lo.x or hi.y < lo.y:
        return R.err(R.BAD_PARAMS, "grid.max must not be below grid.min on x or z.")
    var columns := 0
    var rows := 0
    if grid.has("step") == grid.has("count"):
        return R.err(R.BAD_PARAMS, "grid needs step (the spacing in metres) or count ([columns, rows]), not both.")
    if grid.has("step"):
        if not _is_number(grid["step"]) or float(grid["step"]) <= 0.0:
            return R.err(R.BAD_PARAMS, "grid.step must be a number above 0.")
        var step := float(grid["step"])
        # A point on max is included; the small slack keeps it when the division rounds down.
        columns = int(floor((hi.x - lo.x) / step + 1e-6)) + 1
        rows = int(floor((hi.y - lo.y) / step + 1e-6)) + 1
    else:
        var count: Variant = grid.get("count")
        if not count is Array or (count as Array).size() != 2 or _whole_number(count[0]) == null or _whole_number(count[1]) == null:
            return R.err(R.BAD_PARAMS, "grid.count must be [columns, rows], two whole numbers.")
        columns = int(_whole_number(count[0]))
        rows = int(_whole_number(count[1]))
        if columns < 1 or rows < 1:
            return R.err(R.BAD_PARAMS, "grid.count values must be at least 1.")
    if columns * rows > MAX_GRID:
        return R.err(R.BAD_PARAMS, "This grid has %d x %d = %d points; at most %d. Use a larger step or a smaller area." % [columns, rows, columns * rows, MAX_GRID])
    var step_x := 0.0 if columns == 1 else (hi.x - lo.x) / float(columns - 1)
    var step_z := 0.0 if rows == 1 else (hi.y - lo.y) / float(rows - 1)
    if grid.has("step"):
        step_x = float(grid["step"])
        step_z = float(grid["step"])
    var heights: Array = []
    var hits_detail: Array = []
    var colliders := {}
    var hit_count := 0
    var lowest: Variant = null
    var highest: Variant = null
    for row in rows:
        var line: Array = []
        var z := lo.y + step_z * row
        for column in columns:
            var x := lo.x + step_x * column
            var answer := _cast(rt, space, Vector3(x, float(options["top"]), z), Vector3(x, float(options["bottom"]), z), options)
            if not bool(answer["hit"]):
                line.append(null)
                if detail:
                    hits_detail.append({"x": _round(x), "z": _round(z), "hit": false})
                continue
            hit_count += 1
            var y: float = answer["position"][1]
            line.append(y)
            lowest = y if lowest == null else minf(float(lowest), y)
            highest = y if highest == null else maxf(float(highest), y)
            var key := str(answer["collider"])
            if not colliders.has(key):
                colliders[key] = {"path": answer["collider"], "class": answer["class"], "count": 0}
            colliders[key]["count"] = int(colliders[key]["count"]) + 1
            if detail:
                answer["x"] = _round(x)
                answer["z"] = _round(z)
                hits_detail.append(answer)
        heights.append(line)
    var body := {
        "grid": {
            "min": [_round(lo.x), _round(lo.y)],
            "max": [_round(lo.x + step_x * (columns - 1)), _round(lo.y + step_z * (rows - 1))],
            "step": [_round(step_x), _round(step_z)],
            "columns": columns,
            "rows": rows,
            "layout": "heights[row][column]: row r is z = min.z + r * step.z, column c is x = min.x + c * step.x; null is no hit",
        },
        "heights": heights,
        "hits": hit_count,
        "missed": columns * rows - hit_count,
        "lowest": lowest,
        "highest": highest,
        "colliders": colliders.values(),
        "top": float(options["top"]),
        "bottom": float(options["bottom"]),
        "mask": int(options["mask"]),
    }
    if detail:
        body["points"] = hits_detail
    return R.ok(body)


static func _cast(rt: Node, space: PhysicsDirectSpaceState3D, from: Vector3, to: Vector3, options: Dictionary) -> Dictionary:
    var query := PhysicsRayQueryParameters3D.create(from, to, int(options["mask"]), options["exclude"])
    query.collide_with_areas = bool(options["areas"])
    query.collide_with_bodies = bool(options["bodies"])
    var hit := space.intersect_ray(query)
    var answer := {"from": _vec(from), "to": _vec(to), "hit": not hit.is_empty()}
    if hit.is_empty():
        return answer
    var position: Vector3 = hit.get("position", Vector3.ZERO)
    var collider: Object = hit.get("collider")
    var shape := int(hit.get("shape", 0))
    answer["position"] = _vec(position)
    answer["normal"] = _vec(hit.get("normal", Vector3.ZERO))
    answer["distance"] = _round(from.distance_to(position))
    answer["shape"] = shape
    answer["collider_id"] = int(hit.get("collider_id", 0))
    if collider is Node:
        answer["collider"] = rt.path_of(collider)
        answer["class"] = collider.get_class()
    else:
        answer["collider"] = ""
        answer["class"] = collider.get_class() if collider != null else ""
    # The node that owns the shape that was hit, such as one CollisionShape3D of a body.
    if collider is CollisionObject3D:
        var body := collider as CollisionObject3D
        var owner_id := body.shape_find_owner(shape)
        if owner_id >= 0:
            var shape_owner: Object = body.shape_owner_get_owner(owner_id)
            if shape_owner is Node:
                answer["shape_node"] = rt.path_of(shape_owner)
    return answer


static func _vector3(raw: Variant) -> Variant:
    if not raw is Array or (raw as Array).size() != 3:
        return null
    for item: Variant in raw:
        if not _is_number(item):
            return null
    return Vector3(float(raw[0]), float(raw[1]), float(raw[2]))


static func _vector2(raw: Variant) -> Variant:
    if not raw is Array or (raw as Array).size() != 2:
        return null
    for item: Variant in raw:
        if not _is_number(item):
            return null
    return Vector2(float(raw[0]), float(raw[1]))


static func _is_number(value: Variant) -> bool:
    return (value is int or value is float) and is_finite(float(value))


static func _whole_number(raw: Variant) -> Variant:
    if raw is int:
        return int(raw)
    if raw is float and is_finite(raw) and float(int(raw)) == float(raw):
        return int(raw)
    return null


static func _round(value: float) -> float:
    return snappedf(value, 0.0001)


static func _vec(value: Vector3) -> Array:
    return [_round(value.x), _round(value.y), _round(value.z)]
