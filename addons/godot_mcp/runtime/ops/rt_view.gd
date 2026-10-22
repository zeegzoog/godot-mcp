extends RefCounted
## Screenshots of the running game. The image is returned as base64 PNG inside the
## debugger reply. No file is written. Several frames are stitched into one sheet, and
## each capture is compared with the previous one from this process.

const R := preload("rt_result.gd")
const Hold := preload("rt_hold.gd")
const DEFAULT_MAX := 768
const MAX_MAX := 4096
const MAX_FRAMES := 8
const MAX_BASELINES := 8
const MAX_BASELINE_PIXELS := 512 * 512
## Frames a capture waits for a window that has not drawn anything yet before it gives up.
const EMPTY_RETRIES := 60

static var _previous = null
static var _baselines: Dictionary = {}


static func methods() -> Dictionary:
    return {"game.screenshot": {"fn": screenshot}}


static func screenshot(rt: Node, params: Dictionary) -> Dictionary:
    var frames := int(params.get("frames", 1))
    if frames < 1 or frames > MAX_FRAMES:
        return R.err(R.BAD_PARAMS, "frames must be from 1 to %d." % MAX_FRAMES)
    var every := int(params.get("every", 1))
    if every < 1 or every > 60:
        return R.err(R.BAD_PARAMS, "every must be from 1 to 60.")
    var request := _baseline_request(params)
    if R.is_err(request):
        return request
    if DisplayServer.get_name() == "headless":
        return R.err(R.UNSUPPORTED, "This game is headless, so there is no image to capture. Start it without headless mode.")
    var picked := _pick(rt, params)
    if R.is_err(picked):
        return picked
    var max_side := clampi(int(params.get("max_dimension", DEFAULT_MAX)), 16, MAX_MAX)
    # A baseline is compared pixel for pixel, so a comparison without max_dimension is taken at
    # the size the baseline was saved at.
    var compare_name := str(request.get("compare", ""))
    if not params.has("max_dimension") and not compare_name.is_empty() and frames == 1:
        var stored: Image = _baselines[compare_name]
        max_side = clampi(maxi(stored.get_width(), stored.get_height()), 16, MAX_MAX)
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
        "baseline": request,
    }
    if Hold.is_held():
        return _capture_now(state)
    return {"_pending": true, "_tick": func() -> bool: return _tick_shot(rt, state)}


## A held game draws nothing new, so its capture is the last frame it drew, taken at once.
static func _capture_now(state: Dictionary) -> Dictionary:
    var grabbed = _grab(state)
    if grabbed == null:
        return R.err(R.GODOT_ERROR, "Nothing has been drawn yet. Take one tick with godot_game_physics first.")
    if grabbed is Dictionary:
        return grabbed
    state["got"] = [grabbed]
    return _compose(state)


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
    rt.finish_job(_compose(state))
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


static func _compose(state: Dictionary) -> Dictionary:
    var frames: Array = state["got"]
    var label := str(state.get("label", "viewport"))
    var last: Image = frames[frames.size() - 1]
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
    var request: Dictionary = state.get("baseline", {})
    var applied := _apply_baseline(sheet, request)
    if R.is_err(applied):
        return applied
    var diff := _diff_against(last, _previous as Image)
    _previous = last.duplicate()
    var body := {
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
    }
    var report: Dictionary = applied.get("report", {})
    if not report.is_empty():
        body["baseline"] = report
    return R.ok(body)


static func _diff_against(image: Image, previous: Image) -> Dictionary:
    if previous == null:
        return {"compared": false, "reason": "no previous capture"}
    if previous.get_width() != image.get_width() or previous.get_height() != image.get_height():
        # A picture scaled to another size differs from the original everywhere, so a count of
        # changed pixels between the two would report changes that are not in the game.
        return {"compared": false, "reason": "the previous capture had another size"}
    var measured := _measure(image, previous, 0, Rect2i(0, 0, image.get_width(), image.get_height()))
    measured["rect"] = measured.get("bounds", null)
    return measured


static func _measure(current: Image, previous: Image, tolerance: int, roi: Rect2i) -> Dictionary:
    var image_width := current.get_width()
    var image_height := current.get_height()
    var width := roi.size.x
    var height := roi.size.y
    var stride := 1
    while width * height > stride * stride * 700000:
        stride += 1
    var current_bytes := current.get_data()
    var previous_bytes := previous.get_data()
    var changed := 0
    var sampled := 0
    var min_x := image_width
    var min_y := image_height
    var max_x := -1
    var max_y := -1
    var y := 0
    while y < height:
        var x := 0
        while x < width:
            var pixel_x := roi.position.x + x
            var pixel_y := roi.position.y + y
            var offset := (pixel_y * image_width + pixel_x) * 4
            sampled += 1
            if absi(int(current_bytes[offset]) - int(previous_bytes[offset])) > tolerance \
                    or absi(int(current_bytes[offset + 1]) - int(previous_bytes[offset + 1])) > tolerance \
                    or absi(int(current_bytes[offset + 2]) - int(previous_bytes[offset + 2])) > tolerance \
                    or absi(int(current_bytes[offset + 3]) - int(previous_bytes[offset + 3])) > tolerance:
                changed += 1
                min_x = mini(min_x, pixel_x)
                min_y = mini(min_y, pixel_y)
                max_x = maxi(max_x, pixel_x)
                max_y = maxi(max_y, pixel_y)
            x += stride
        y += stride
    var bounds = null
    if changed > 0:
        bounds = {
            "x": min_x,
            "y": min_y,
            "w": mini(max_x - min_x + stride, image_width - min_x),
            "h": mini(max_y - min_y + stride, image_height - min_y),
        }
    var ratio := 0.0 if sampled == 0 else float(changed) / float(sampled)
    return {
        "compared": true,
        "changed": changed,
        "sampled": sampled,
        "ratio": snappedf(ratio, 0.0001),
        "bounds": bounds,
        "exact": stride == 1,
    }


static func _baseline_request(params: Dictionary) -> Dictionary:
    var save := ""
    if params.has("save_baseline"):
        save = str(params.get("save_baseline", "")).strip_edges()
        if not _valid_baseline_name(save):
            return R.err(R.BAD_PARAMS, "save_baseline name must match ^[A-Za-z_][A-Za-z0-9_-]{0,63}$.")
    var compare := ""
    if params.has("baseline"):
        compare = str(params.get("baseline", "")).strip_edges()
        if not _valid_baseline_name(compare):
            return R.err(R.BAD_PARAMS, "baseline name must match ^[A-Za-z_][A-Za-z0-9_-]{0,63}$.")
        if not _baselines.has(compare):
            return R.err(R.NOT_FOUND, "No baseline named '%s' is saved in this run." % compare)
    var tolerance := 0
    if params.has("tolerance"):
        var whole: Variant = _whole_number(params.get("tolerance"))
        if whole == null or int(whole) < 0 or int(whole) > 255:
            return R.err(R.BAD_PARAMS, "tolerance must be an integer from 0 to 255.")
        tolerance = int(whole)
    var roi: Variant = null
    if params.has("roi"):
        var parsed := _parse_roi(params.get("roi"))
        if R.is_err(parsed):
            return parsed
        roi = parsed["roi"]
    if save.is_empty() and compare.is_empty() and (roi != null or params.has("tolerance")):
        return R.err(R.BAD_PARAMS, "roi and tolerance are only used when comparing a baseline.")
    return {"save": save, "compare": compare, "tolerance": tolerance, "roi": roi}


static func _apply_baseline(image: Image, request: Dictionary) -> Dictionary:
    var compare := str(request.get("compare", ""))
    var save := str(request.get("save", ""))
    if compare.is_empty() and save.is_empty():
        return {"report": {}}
    var report := {}
    if not compare.is_empty():
        var stored: Image = _baselines[compare]
        if stored.get_width() != image.get_width() or stored.get_height() != image.get_height():
            return R.err(
                R.BAD_PARAMS,
                "Baseline '%s' is %dx%d and this image is %dx%d, so an roi would not refer to the same pixels." % [compare, stored.get_width(), stored.get_height(), image.get_width(), image.get_height()],
            )
        var roi := Rect2i(0, 0, image.get_width(), image.get_height())
        if request.get("roi") != null:
            var fitted := _fit_roi(request["roi"], image)
            if R.is_err(fitted):
                return fitted
            roi = fitted["rect"]
        report = _measure(image, stored, int(request.get("tolerance", 0)), roi)
        report["name"] = compare
        report["tolerance"] = int(request.get("tolerance", 0))
        report["roi"] = {"x": roi.position.x, "y": roi.position.y, "w": roi.size.x, "h": roi.size.y}
        report["roi_space"] = "returned_image"
    if not save.is_empty():
        var pixels := image.get_width() * image.get_height()
        if pixels > MAX_BASELINE_PIXELS:
            return R.err(R.BAD_PARAMS, "This image has %d pixels. A baseline can store at most %d pixels." % [pixels, MAX_BASELINE_PIXELS])
        if not _baselines.has(save) and _baselines.size() >= MAX_BASELINES:
            return R.err(R.BAD_PARAMS, "At most %d baselines can be stored in this run." % MAX_BASELINES)
        _baselines[save] = image.duplicate()
        report["saved"] = save
        report["roi_space"] = "returned_image"
    return {"report": report}


static func _parse_roi(raw: Variant) -> Dictionary:
    if not raw is Dictionary:
        return R.err(R.BAD_PARAMS, "roi must be an object with integer x, y, w and h.")
    var roi: Dictionary = raw
    for key: String in ["x", "y", "w", "h"]:
        if _whole_number(roi.get(key, null)) == null:
            return R.err(R.BAD_PARAMS, "roi.%s must be an integer." % key)
    var w := int(_whole_number(roi["w"]))
    var h := int(_whole_number(roi["h"]))
    if w < 1 or h < 1:
        return R.err(R.BAD_PARAMS, "roi w and h must be at least 1.")
    return {"roi": {
        "x": int(_whole_number(roi["x"])),
        "y": int(_whole_number(roi["y"])),
        "w": w,
        "h": h,
    }}


static func _fit_roi(raw: Variant, image: Image) -> Dictionary:
    var roi: Dictionary = raw
    var x := int(roi["x"])
    var y := int(roi["y"])
    var w := int(roi["w"])
    var h := int(roi["h"])
    if x < 0 or y < 0 or x + w > image.get_width() or y + h > image.get_height():
        return R.err(R.BAD_PARAMS, "roi %d,%d %dx%d is outside the returned image (%dx%d)." % [x, y, w, h, image.get_width(), image.get_height()])
    return {"rect": Rect2i(x, y, w, h)}


static func _valid_baseline_name(name: String) -> bool:
    if name.is_empty() or name.length() > 64:
        return false
    var pattern := RegEx.new()
    if pattern.compile("^[A-Za-z_][A-Za-z0-9_\\-]{0,63}$") != OK:
        return false
    return pattern.search(name) != null


static func _whole_number(raw: Variant) -> Variant:
    if raw is int:
        return int(raw)
    if raw is float and float(int(raw)) == float(raw):
        return int(raw)
    return null


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
