extends RefCounted
## Screenshots of the running game. The image is returned as base64 PNG inside the
## debugger reply. No file is written. Several frames are stitched into one sheet, and
## each capture is compared with the previous one from this process.

const R := preload("rt_result.gd")
const DEFAULT_MAX := 768
const MAX_MAX := 4096
const MAX_FRAMES := 8
## Frames a capture waits for a window that has not drawn anything yet before it gives up.
const EMPTY_RETRIES := 60

static var _previous = null


static func methods() -> Dictionary:
    return {"game.screenshot": {"fn": screenshot}}


static func screenshot(rt: Node, params: Dictionary) -> Dictionary:
    var frames := int(params.get("frames", 1))
    if frames < 1 or frames > MAX_FRAMES:
        return R.err(R.BAD_PARAMS, "frames must be from 1 to %d." % MAX_FRAMES)
    var every := int(params.get("every", 1))
    if every < 1 or every > 60:
        return R.err(R.BAD_PARAMS, "every must be from 1 to 60.")
    if DisplayServer.get_name() == "headless":
        return R.err(R.UNSUPPORTED, "This game is headless, so there is no image to capture. Start it without headless mode.")
    var picked := _pick(rt, params)
    if R.is_err(picked):
        return picked
    var max_side := clampi(int(params.get("max_dimension", DEFAULT_MAX)), 16, MAX_MAX)
    var cols := 1
    while cols * cols < frames:
        cols += 1
    var rows := int(ceil(float(frames) / float(cols)))
    var frame_max := maxi(int(float(max_side) / float(maxi(cols, rows))), 1)
    var state := {
        "phase": "render",
        "pause": 0,
        "every": every,
        "want": frames,
        "got": [],
        "frame_max": frame_max,
        "viewport": picked["viewport"],
        "restore_camera": picked.get("restore_camera"),
        "made_current": picked.get("made_current"),
        "made_current_2d": picked.get("made_current_2d"),
        "previous_2d": bool(picked.get("previous_2d", false)),
        "label": picked.get("label", "viewport"),
    }
    return {"_pending": true, "_tick": func() -> bool: return _tick_shot(rt, state)}


static func _tick_shot(rt: Node, state: Dictionary) -> bool:
    var phase := str(state["phase"])
    if phase == "render":
        state["phase"] = "capture"
        return true
    if phase == "pause":
        state["pause"] = int(state["pause"]) - 1
        if int(state["pause"]) > 0:
            return true
        state["phase"] = "capture"
    var grabbed = _grab(state)
    if grabbed == null:
        # A window that has not drawn its first frame has no image yet, which a slow start
        # makes likely: wait for it instead of failing the first capture.
        state["empty"] = int(state.get("empty", 0)) + 1
        if int(state["empty"]) <= EMPTY_RETRIES:
            return true
        _restore_cameras(state)
        rt.finish_job(R.err(R.GODOT_ERROR, "The viewport returned no image."))
        return false
    if grabbed is Dictionary:
        _restore_cameras(state)
        rt.finish_job(grabbed)
        return false
    (state["got"] as Array).append(grabbed)
    if (state["got"] as Array).size() < int(state["want"]):
        state["pause"] = int(state["every"])
        state["phase"] = "pause"
        return true
    _restore_cameras(state)
    rt.finish_job(_compose(state["got"], str(state["label"])))
    return false


static func _grab(state: Dictionary) -> Variant:
    var viewport: Viewport = state["viewport"]
    if not is_instance_valid(viewport):
        return R.err(R.NOT_FOUND, "The viewport was freed before the capture.")
    var texture := viewport.get_texture()
    var image: Image = texture.get_image() if texture != null else null
    if image == null or image.is_empty():
        return null
    image.convert(Image.FORMAT_RGBA8)
    var frame_max := int(state["frame_max"])
    var size := image.get_size()
    var longest := maxi(size.x, size.y)
    if longest > frame_max and longest > 0:
        var scale := float(frame_max) / float(longest)
        image.resize(maxi(int(size.x * scale), 1), maxi(int(size.y * scale), 1), Image.INTERPOLATE_BILINEAR)
    return image


static func _compose(frames: Array, label: String) -> Dictionary:
    var last: Image = frames[frames.size() - 1]
    var diff := _diff_against(last)
    var count := frames.size()
    var cols := 1
    while cols * cols < count:
        cols += 1
    var rows := int(ceil(float(count) / float(cols)))
    var frame_w := last.get_width()
    var frame_h := last.get_height()
    var sheet := last
    if count > 1:
        sheet = Image.create(cols * frame_w, rows * frame_h, false, Image.FORMAT_RGBA8)
        sheet.fill(Color(0, 0, 0, 1))
        for index in count:
            var frame: Image = frames[index]
            if frame.get_width() != frame_w or frame.get_height() != frame_h:
                frame = frame.duplicate()
                frame.resize(frame_w, frame_h, Image.INTERPOLATE_BILINEAR)
            var cell := Vector2i((index % cols) * frame_w, int(float(index) / float(cols)) * frame_h)
            sheet.blit_rect(frame, Rect2i(0, 0, frame.get_width(), frame.get_height()), cell)
    var bytes := sheet.save_png_to_buffer()
    if bytes.is_empty() or bytes.size() < 8 or bytes[0] != 0x89:
        return R.err(R.GODOT_ERROR, "PNG encoding produced no image.")
    return R.ok({
        "mime_type": "image/png",
        "data": Marshalls.raw_to_base64(bytes),
        "width": sheet.get_width(),
        "height": sheet.get_height(),
        "source": label,
        "frames": count,
        "columns": cols,
        "frame_width": frame_w,
        "frame_height": frame_h,
        "diff": diff,
    })


static func _diff_against(image: Image) -> Dictionary:
    var previous: Image = _previous as Image
    _previous = image.duplicate()
    if previous == null:
        return {"compared": false, "reason": "no previous capture"}
    if previous.get_width() != image.get_width() or previous.get_height() != image.get_height():
        # A picture scaled to another size differs from the original everywhere, so a count of
        # changed pixels between the two would report changes that are not in the game.
        return {"compared": false, "reason": "the previous capture had another size"}
    var width := image.get_width()
    var height := image.get_height()
    var stride := 1
    while width * height > stride * stride * 700000:
        stride += 1
    var current_bytes := image.get_data()
    var previous_bytes := previous.get_data()
    var changed := 0
    var sampled := 0
    var min_x := width
    var min_y := height
    var max_x := -1
    var max_y := -1
    var y := 0
    while y < height:
        var x := 0
        while x < width:
            var offset := (y * width + x) * 4
            sampled += 1
            if current_bytes[offset] != previous_bytes[offset] \
                    or current_bytes[offset + 1] != previous_bytes[offset + 1] \
                    or current_bytes[offset + 2] != previous_bytes[offset + 2] \
                    or current_bytes[offset + 3] != previous_bytes[offset + 3]:
                changed += 1
                min_x = mini(min_x, x)
                min_y = mini(min_y, y)
                max_x = maxi(max_x, x)
                max_y = maxi(max_y, y)
            x += stride
        y += stride
    var ratio := 0.0 if sampled == 0 else float(changed) / float(sampled)
    var rect = null
    if changed > 0:
        rect = {
            "x": min_x,
            "y": min_y,
            "w": max_x - min_x + stride,
            "h": max_y - min_y + stride,
        }
    return {
        "compared": true,
        "changed": changed,
        "sampled": sampled,
        "ratio": snappedf(ratio, 0.0001),
        "rect": rect,
    }


static func _restore_cameras(state: Dictionary) -> void:
    var made_current: Camera3D = state.get("made_current")
    var restore_camera: Camera3D = state.get("restore_camera")
    var made_current_2d: Camera2D = state.get("made_current_2d")
    if made_current != null and is_instance_valid(made_current):
        made_current.current = false
    if restore_camera != null and is_instance_valid(restore_camera):
        restore_camera.current = true
    if made_current_2d != null and is_instance_valid(made_current_2d):
        made_current_2d.enabled = bool(state.get("previous_2d", false))


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
