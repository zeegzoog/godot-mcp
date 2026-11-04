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
## Process frames a capture waits for the window to draw a new frame before it draws one itself.
## A window the system stops drawing (covered on macOS, minimized) keeps its last frame in the
## viewport texture, and the engine skips drawing while no window can draw, so the capture then
## draws one frame of the rendering server into the viewports (without presenting it) and reads
## that. A visible window draws every process frame, so a short wait is enough.
const DRAW_WAIT := 4

static var _previous = null
static var _baselines: Dictionary = {}
## The physics tick count when the rendering server last drew, -1 before the first frame.
static var _drawn_physics := -1


static func methods() -> Dictionary:
    if not RenderingServer.frame_post_draw.is_connected(_note_drawn):
        RenderingServer.frame_post_draw.connect(_note_drawn)
    return {"game.screenshot": {"fn": screenshot}}


static func _note_drawn() -> void:
    _drawn_physics = Engine.get_physics_frames()


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
    var picked := _aim(rt, params) if params.has("aim") else _pick(rt, params)
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
        "aim_viewport": picked.get("aim_viewport"),
        "aim": picked.get("aim", {}),
        "label": picked.get("label", "viewport"),
        "baseline": request,
        "drawn_mark": Engine.get_frames_drawn(),
        "stale": 0,
        "forced": 0,
        "waited": 0,
    }
    if Hold.is_held() or state.get("aim_viewport") != null:
        return _capture_now(state)
    return {"_pending": true, "_tick": func() -> bool: return _tick_shot(rt, state)}


## A held game draws nothing new, so its capture is the last frame it drew, taken at once.
## An aimed capture has never been drawn, and the held state is not drawn when the window was
## not drawing (covered or minimized) or a step ran several ticks in one frame, so either draws
## one frame of the rendering server here: the scene does not advance. A held game waits at the
## start of a tick, which the engine has already counted, so the tick before it is the held state.
## Compatibility can omit instance-shaded meshes on a fresh aimed viewport's first draw, so an
## aimed capture primes that renderer once more before readback.
static func _capture_now(state: Dictionary) -> Dictionary:
    var undrawn := _drawn_physics < Engine.get_physics_frames() - 1
    if state.get("aim_viewport") != null or undrawn:
        RenderingServer.force_draw(false, 0.0)
        if state.get("aim_viewport") != null and RenderingServer.get_current_rendering_method() == "gl_compatibility":
            RenderingServer.force_draw(false, 0.0)
        if undrawn and state.get("aim_viewport") == null:
            state["forced"] = 1
    var grabbed = _grab(state)
    if grabbed == null or grabbed is Dictionary:
        _restore_cameras(state)
        return grabbed if grabbed is Dictionary else R.err(R.GODOT_ERROR, "Nothing has been drawn yet. Take one tick with godot_game_physics first.")
    state["got"] = [grabbed]
    state["held"] = Hold.is_held()
    var composed := _compose(state)
    _restore_cameras(state)
    return composed


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
    if not _drew_since_mark(state):
        state["draw_wait"] = int(state.get("draw_wait", 0)) + 1
        if int(state["draw_wait"]) <= DRAW_WAIT:
            return true
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
    if not _drew_since_mark(state):
        # The engine drew nothing new: draw the current state now instead of returning the frame
        # the window drew last.
        RenderingServer.force_draw(false, rt.get_process_delta_time())
        var redrawn = _grab(state)
        if redrawn is Image:
            grabbed = redrawn
            state["forced"] = int(state["forced"]) + 1
        else:
            state["stale"] = int(state["stale"]) + 1
        state["waited"] = DRAW_WAIT
    state["draw_wait"] = 0
    state["drawn_mark"] = Engine.get_frames_drawn()
    (state["got"] as Array).append(grabbed)
    if (state["got"] as Array).size() < int(state["want"]):
        state["pause"] = int(state["every"])
        state["phase"] = "pause"
        return true
    # The picture is composed before the cameras are restored: an aimed capture frees its
    # viewport there, and the answer still measures it.
    var composed := _compose(state)
    _restore_cameras(state)
    rt.finish_job(composed)
    return false


## Whether the engine drew a frame since the last mark. Low processor mode draws only when
## something changed, so a frame not drawn there is still the current picture.
static func _drew_since_mark(state: Dictionary) -> bool:
    if OS.low_processor_usage_mode:
        return true
    return Engine.get_frames_drawn() > int(state["drawn_mark"])


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
    var space := _input_space(state)
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
        "input_width": space["input_width"],
        "input_height": space["input_height"],
        "captured_width": space["captured_width"],
        "captured_height": space["captured_height"],
        "diff": diff,
        "render": {
            "frames_drawn": Engine.get_frames_drawn(),
            "process_frame": Engine.get_process_frames(),
            "stale_frames": 0 if bool(state.get("held", false)) else int(state.get("stale", 0)),
            "forced_frames": int(state.get("forced", 0)),
            "path": _render_path(state, count),
            "waited_frames": int(state.get("waited", 0)),
            "window_can_draw": DisplayServer.window_can_draw(),
            "held": bool(state.get("held", false)),
        },
    }
    var report: Dictionary = applied.get("report", {})
    if not report.is_empty():
        body["baseline"] = report
    var aim: Dictionary = state.get("aim", {})
    if not aim.is_empty():
        body["aim"] = aim
    return R.ok(body)


## Which path produced the frames: the window's own drawing, a frame the capture drew itself
## because the window was not drawing, both (a sheet), or the last frame of a held game.
static func _render_path(state: Dictionary, count: int) -> String:
    var forced := int(state.get("forced", 0))
    if bool(state.get("held", false)):
        return "forced_draw" if forced > 0 else "held"
    if forced == 0:
        return "window"
    return "forced_draw" if forced >= count else "mixed"


## The coordinate space godot_game_input uses, and the viewport this image was taken from.
## Input positions are the root viewport's visible rectangle (after content scale). The image
## is a downscale of the captured viewport, so image pixels map back by input_size / frame_size
## when that viewport is the root.
static func _input_space(state: Dictionary) -> Dictionary:
    var input_w := 0
    var input_h := 0
    var captured_w := 0
    var captured_h := 0
    var tree := Engine.get_main_loop() as SceneTree
    if tree != null and tree.root != null:
        var input_size := tree.root.get_visible_rect().size
        input_w = int(round(input_size.x))
        input_h = int(round(input_size.y))
    var viewport: Viewport = state.get("viewport")
    if viewport != null and is_instance_valid(viewport):
        var captured_size := viewport.get_visible_rect().size
        captured_w = int(round(captured_size.x))
        captured_h = int(round(captured_size.y))
    if input_w <= 0 or input_h <= 0:
        input_w = captured_w
        input_h = captured_h
    return {
        "input_width": input_w,
        "input_height": input_h,
        "captured_width": captured_w,
        "captured_height": captured_h,
    }


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
    var aim_viewport: SubViewport = state.get("aim_viewport")
    if aim_viewport != null and is_instance_valid(aim_viewport):
        aim_viewport.get_parent().remove_child(aim_viewport)
        aim_viewport.free()
    state["aim_viewport"] = null


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


## A capture from a temporary camera in a temporary SubViewport that shares the game's 3D world.
## The game's own cameras are not touched: its current camera stays current and keeps its
## transform. The new camera copies the game camera's projection and environment and is either
## moved so `at` is in the centre (same direction, same distance as the game camera) or placed at
## `position` looking at `look_at`. Only the 3D world is drawn: the game's HUD and other 2D canvas
## items are not in the picture. Both nodes are freed after the capture.
static func _aim(rt: Node, params: Dictionary) -> Dictionary:
    var raw: Variant = params.get("aim")
    if not raw is Dictionary:
        return R.err(R.BAD_PARAMS, "aim must be an object: {at: [x, y, z]} or {position: [x, y, z], look_at: [x, y, z]}.")
    var aim: Dictionary = raw
    for key: String in aim.keys():
        if not key in ["at", "distance", "position", "look_at", "up"]:
            return R.err(R.BAD_PARAMS, "aim has no '%s'. It takes at and distance, or position, look_at and up." % key)
    if int(params.get("frames", 1)) != 1:
        return R.err(R.BAD_PARAMS, "aim takes one picture: leave frames at 1.")
    if not str(params.get("node", "")).strip_edges().is_empty():
        return R.err(R.BAD_PARAMS, "aim and node do not go together: aim draws its own viewport.")
    var at: Variant = _vector3(aim.get("at"))
    var position: Variant = _vector3(aim.get("position"))
    var look_at: Variant = _vector3(aim.get("look_at"))
    var by_point := aim.has("at")
    if by_point == (aim.has("position") or aim.has("look_at")):
        return R.err(R.BAD_PARAMS, "aim takes at (a point to centre), or position and look_at, not both.")
    if by_point and at == null:
        return R.err(R.BAD_PARAMS, "aim.at must be [x, y, z].")
    if not by_point and (position == null or look_at == null):
        return R.err(R.BAD_PARAMS, "aim.position and aim.look_at must both be [x, y, z].")
    if not by_point and (aim.has("distance")):
        return R.err(R.BAD_PARAMS, "aim.distance goes with aim.at; with position and look_at the camera is where position says.")
    if by_point and aim.has("up"):
        return R.err(R.BAD_PARAMS, "aim.up goes with position and look_at; with at the camera keeps the game camera's angle.")
    var distance: Variant = null
    if aim.has("distance"):
        var value: Variant = aim.get("distance")
        if not (value is int or value is float) or float(value) <= 0.0 or not is_finite(float(value)):
            return R.err(R.BAD_PARAMS, "aim.distance must be a number above 0.")
        distance = float(value)
    var up := Vector3.UP
    if aim.has("up"):
        var given_up: Variant = _vector3(aim.get("up"))
        if given_up == null or (given_up as Vector3).is_zero_approx():
            return R.err(R.BAD_PARAMS, "aim.up must be a non-zero [x, y, z].")
        up = (given_up as Vector3).normalized()
    var source: Camera3D = null
    var camera_path := str(params.get("camera", "")).strip_edges()
    if not camera_path.is_empty():
        var named: Node = rt.find_node(camera_path)
        if not named is Camera3D:
            return R.err(R.NOT_FOUND, "aim copies a Camera3D, and '%s' is %s." % [camera_path, "missing" if named == null else "a " + named.get_class()])
        source = named as Camera3D
    else:
        source = rt.get_tree().root.get_camera_3d()
    if source == null and by_point:
        return R.err(R.NOT_FOUND, "The game has no current Camera3D to copy, so aim.at has no direction to keep. Pass aim.position and aim.look_at, or name a Camera3D with camera.")
    var source_viewport: Viewport = source.get_viewport() if source != null else rt.get_tree().root
    var world: World3D = source_viewport.find_world_3d()
    if world == null:
        return R.err(R.UNSUPPORTED, "There is no 3D world to aim at.")
    var report := {"copied_from": rt.path_of(source) if source != null else ""}
    var transform := Transform3D.IDENTITY
    if by_point:
        var forward := -source.global_basis.z.normalized()
        var point: Vector3 = at
        if distance == null:
            var measured := _camera_distance(source, world, point)
            distance = measured["distance"]
            report["distance_from"] = measured["from"]
        else:
            report["distance_from"] = "given"
        transform = Transform3D(source.global_basis.orthonormalized(), point - forward * float(distance))
        report["mode"] = "at"
        report["at"] = _vec(point)
        report["distance"] = snappedf(float(distance), 0.0001)
    else:
        var eye: Vector3 = position
        var target: Vector3 = look_at
        if eye.is_equal_approx(target):
            return R.err(R.BAD_PARAMS, "aim.position and aim.look_at are the same point.")
        var direction := (target - eye).normalized()
        if absf(direction.dot(up)) > 0.999:
            # Looking straight along up leaves the roll undefined: keep the picture's top to -Z.
            up = Vector3.FORWARD if absf(direction.dot(Vector3.FORWARD)) < 0.999 else Vector3.RIGHT
            report["up_replaced"] = _vec(up)
        transform = Transform3D(Basis.looking_at(direction, up), eye)
        report["mode"] = "look_at"
        report["look_at"] = _vec(target)
        report["distance"] = snappedf(eye.distance_to(target), 0.0001)
    var size := Vector2i(source_viewport.get_visible_rect().size)
    var texture := source_viewport.get_texture()
    if texture != null and texture.get_width() > 0 and texture.get_height() > 0:
        size = Vector2i(texture.get_width(), texture.get_height())
    if size.x <= 0 or size.y <= 0:
        return R.err(R.GODOT_ERROR, "The game's viewport has no size yet.")
    var viewport := SubViewport.new()
    viewport.name = "GodotMcpAimViewport"
    viewport.size = size
    viewport.world_3d = world
    viewport.transparent_bg = false
    viewport.render_target_update_mode = SubViewport.UPDATE_ALWAYS
    for property: String in ["msaa_3d", "screen_space_aa", "use_taa", "use_debanding", "use_occlusion_culling", "mesh_lod_threshold", "scaling_3d_mode", "scaling_3d_scale", "fsr_sharpness", "texture_mipmap_bias", "positional_shadow_atlas_size", "positional_shadow_atlas_16_bits", "positional_shadow_atlas_quad_0", "positional_shadow_atlas_quad_1", "positional_shadow_atlas_quad_2", "positional_shadow_atlas_quad_3", "use_hdr_2d"]:
        if property in source_viewport and property in viewport:
            viewport.set(property, source_viewport.get(property))
    var camera := Camera3D.new()
    camera.name = "GodotMcpAimCamera"
    if source != null:
        for property: String in ["projection", "fov", "size", "near", "far", "keep_aspect", "frustum_offset", "cull_mask", "environment", "attributes", "compositor"]:
            if property in source:
                camera.set(property, source.get(property))
        if not is_zero_approx(source.h_offset) or not is_zero_approx(source.v_offset):
            report["offsets_dropped"] = [source.h_offset, source.v_offset]
    viewport.add_child(camera)
    rt.add_child(viewport)
    camera.global_transform = transform
    # A held game flushes no transform notifications, and the forced draw would otherwise use
    # the camera's transform from before it was placed.
    camera.force_update_transform()
    camera.current = true
    report["position"] = _vec(transform.origin)
    report["forward"] = _vec(-transform.basis.z)
    report["projection"] = ["perspective", "orthogonal", "frustum"][camera.projection]
    if camera.projection == Camera3D.PROJECTION_ORTHOGONAL:
        report["size"] = snappedf(camera.size, 0.0001)
    else:
        report["fov"] = snappedf(camera.fov, 0.0001)
    report["image_centre"] = "the aimed point is the centre pixel of the returned image"
    return {"viewport": viewport, "label": "aim camera", "aim_viewport": viewport, "aim": report}


## How far the game camera is from what it looks at: the first hit straight ahead of it, else the
## distance from it to the point along its view direction.
static func _camera_distance(source: Camera3D, world: World3D, point: Vector3) -> Dictionary:
    var origin := source.global_position
    var forward := -source.global_basis.z.normalized()
    var space := world.direct_space_state
    if space != null:
        var query := PhysicsRayQueryParameters3D.create(origin, origin + forward * source.far)
        var hit := space.intersect_ray(query)
        if not hit.is_empty():
            var reach := origin.distance_to(hit["position"])
            if reach > source.near:
                return {"distance": reach, "from": "game_camera_centre_hit"}
    var along := (point - origin).dot(forward)
    if along > source.near:
        return {"distance": along, "from": "game_camera_to_point"}
    return {"distance": maxf(origin.distance_to(point), source.near * 2.0 + 0.1), "from": "game_camera_to_point"}


static func _vector3(raw: Variant) -> Variant:
    if not raw is Array or (raw as Array).size() != 3:
        return null
    for item: Variant in raw:
        if not (item is int or item is float) or not is_finite(float(item)):
            return null
    return Vector3(float(raw[0]), float(raw[1]), float(raw[2]))


static func _vec(value: Vector3) -> Array:
    return [snappedf(value.x, 0.0001), snappedf(value.y, 0.0001), snappedf(value.z, 0.0001)]
