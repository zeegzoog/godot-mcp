extends RefCounted
## Screenshots of the running game. The image is returned as base64 PNG inside the
## debugger reply. No file is written unless the caller passes save_to, and save_to is
## rejected: this runtime keeps captures in memory.

const R := preload("rt_result.gd")
const DEFAULT_MAX := 768
const MAX_MAX := 4096


static func methods() -> Dictionary:
    return {"game.screenshot": {"fn": screenshot}}


static func screenshot(rt: Node, params: Dictionary) -> Dictionary:
    if DisplayServer.get_name() == "headless":
        return R.err(R.UNSUPPORTED, "This game is headless, so there is no image to capture. Start it without headless mode.")
    var picked := _pick(rt, params)
    if R.is_err(picked):
        return picked
    var viewport: Viewport = picked["viewport"]
    var restore_camera: Camera3D = picked.get("restore_camera")
    var made_current: Camera3D = picked.get("made_current")
    var made_current_2d: Camera2D = picked.get("made_current_2d")
    var previous_2d: bool = bool(picked.get("previous_2d", false))
    var state := {
        "waited": false,
        "viewport": viewport,
        "restore_camera": restore_camera,
        "made_current": made_current,
        "made_current_2d": made_current_2d,
        "previous_2d": previous_2d,
        "params": params,
        "label": picked.get("label", "viewport"),
    }
    return {"_pending": true, "_tick": func() -> bool: return _finish_shot(rt, state)}


static func _finish_shot(rt: Node, state: Dictionary) -> bool:
    if not bool(state["waited"]):
        state["waited"] = true
        return true
    var viewport: Viewport = state["viewport"]
    var made_current: Camera3D = state.get("made_current")
    var restore_camera: Camera3D = state.get("restore_camera")
    var made_current_2d: Camera2D = state.get("made_current_2d")
    var previous_2d: bool = bool(state.get("previous_2d", false))
    if not is_instance_valid(viewport):
        rt.finish_job(R.err(R.NOT_FOUND, "The viewport was freed before the capture."))
        return false
    var texture := viewport.get_texture()
    var image: Image = texture.get_image() if texture != null else null
    if made_current != null and is_instance_valid(made_current):
        made_current.current = false
    if restore_camera != null and is_instance_valid(restore_camera):
        restore_camera.current = true
    if made_current_2d != null and is_instance_valid(made_current_2d):
        made_current_2d.enabled = previous_2d
    if image == null or image.is_empty():
        rt.finish_job(R.err(R.GODOT_ERROR, "The viewport returned no image."))
        return false
    image.convert(Image.FORMAT_RGBA8)
    var params: Dictionary = state["params"]
    var max_side := clampi(int(params.get("max_dimension", DEFAULT_MAX)), 16, MAX_MAX)
    var size := image.get_size()
    var longest := maxi(size.x, size.y)
    if longest > max_side and longest > 0:
        var scale := float(max_side) / float(longest)
        image.resize(maxi(int(size.x * scale), 1), maxi(int(size.y * scale), 1), Image.INTERPOLATE_BILINEAR)
    var bytes := image.save_png_to_buffer()
    if bytes.is_empty() or bytes.size() < 8 or bytes[0] != 0x89:
        rt.finish_job(R.err(R.GODOT_ERROR, "PNG encoding produced no image."))
        return false
    rt.finish_job(R.ok({
        "mime_type": "image/png",
        "data": Marshalls.raw_to_base64(bytes),
        "width": image.get_width(),
        "height": image.get_height(),
        "source": state["label"],
    }))
    return false


static func _pick(rt: Node, params: Dictionary) -> Dictionary:
    var camera_path := str(params.get("camera", "")).strip_edges()
    var node_path := str(params.get("node", "")).strip_edges()
    var path := camera_path if not camera_path.is_empty() else node_path
    if path.is_empty():
        return {"viewport": rt.get_tree().root, "label": "main window"}
    var node: Node = rt.find_node(path) as Node
    if node == null:
        return R.err(R.NOT_FOUND, "No node at '%s' to capture." % path)
    if node is Camera3D:
        var camera := node as Camera3D
        var viewport := camera.get_viewport()
        var previous := viewport.get_camera_3d()
        if previous != camera:
            camera.current = true
        return {"viewport": viewport, "label": rt.path_of(camera), "made_current": camera if previous != camera else null, "restore_camera": previous if previous != camera else null}
    if node is Camera2D:
        var camera_2d := node as Camera2D
        var was_enabled := camera_2d.enabled
        if not was_enabled:
            camera_2d.enabled = true
            camera_2d.make_current()
        return {"viewport": camera_2d.get_viewport(), "label": rt.path_of(camera_2d), "made_current_2d": camera_2d if not was_enabled else null, "previous_2d": was_enabled}
    if node is Viewport:
        return {"viewport": node, "label": rt.path_of(node)}
    if node is SubViewportContainer:
        for child: Node in node.get_children():
            if child is SubViewport:
                return {"viewport": child, "label": rt.path_of(child)}
        return R.err(R.NOT_FOUND, "SubViewportContainer '%s' has no SubViewport." % path)
    return {"viewport": node.get_viewport(), "label": rt.path_of(node)}
