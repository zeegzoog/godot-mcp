extends RefCounted
## Saves a series of PNG frames of the game window to a directory the caller created.
## The debugger reply lists paths and timings. It does not carry image bytes.
## Each frame only grabs an Image. PNG encoding waits until the last frame is captured,
## so the game's frame time is the capture interval and `msec` is that instant.

const R := preload("rt_result.gd")
const MAX_FRAMES := 240
const MAX_EVERY := 60
## frames × every is the process-frame span of the recording, kept inside the server's wait.
const MAX_SPAN := 480
const DEFAULT_FRAMES := 60
const DEFAULT_MAX := 768
const MAX_MAX := 4096
const MIN_MAX := 16
## Frames a capture waits for a window that has not drawn anything yet.
const EMPTY_RETRIES := 60
## Raw RGBA bytes held until the PNGs are written. frames × width × height × 4, after max_dimension.
const MAX_PIXEL_BYTES := 512 * 1024 * 1024


static func methods() -> Dictionary:
    return {"game.record": {"fn": record}}


static func record(rt: Node, params: Dictionary) -> Dictionary:
    var frames: Variant = _whole(params.get("frames", DEFAULT_FRAMES))
    if frames == null or int(frames) < 1 or int(frames) > MAX_FRAMES:
        return R.err(R.BAD_PARAMS, "frames must be a whole number from 1 to %d." % MAX_FRAMES)
    var every: Variant = _whole(params.get("every", 1))
    if every == null or int(every) < 1 or int(every) > MAX_EVERY:
        return R.err(R.BAD_PARAMS, "every must be a whole number from 1 to %d." % MAX_EVERY)
    if int(frames) * int(every) > MAX_SPAN:
        return R.err(R.BAD_PARAMS, "frames × every must be at most %d process frames." % MAX_SPAN)
    var max_side: Variant = _whole(params.get("max_dimension", DEFAULT_MAX))
    if max_side == null or int(max_side) < MIN_MAX or int(max_side) > MAX_MAX:
        return R.err(R.BAD_PARAMS, "max_dimension must be a whole number from %d to %d." % [MIN_MAX, MAX_MAX])
    var directory := str(params.get("directory", "")).strip_edges()
    if directory.is_empty() or directory.begins_with("res://") or directory.begins_with("user://") or not directory.is_absolute_path():
        return R.err(R.BAD_PARAMS, "directory must be an absolute host path the caller created.")
    if not DirAccess.dir_exists_absolute(directory):
        return R.err(R.NOT_FOUND, "Recording directory does not exist.")
    if DisplayServer.get_name() == "headless":
        return R.err(R.UNSUPPORTED, "This game is headless, so there is no image to record. Start it without headless mode.")
    var state := {
        "phase": "render",
        "pause": 0,
        "every": int(every),
        "want": int(frames),
        "images": [],
        "meta": [],
        "max_side": int(max_side),
        "viewport": rt.get_tree().root,
        "directory": directory,
        "empty": 0,
        "clock": false,
        "started_msec": 0,
    }
    return {"_pending": true, "_tick": func() -> bool: return _tick_record(rt, state)}


static func _tick_record(rt: Node, state: Dictionary) -> bool:
    var phase := str(state["phase"])
    if phase == "render":
        state["phase"] = "capture"
        return true
    if phase == "pause":
        state["pause"] = int(state["pause"]) - 1
        if int(state["pause"]) > 0:
            return true
        state["phase"] = "capture"
    var grabbed: Variant = _grab(state["viewport"], int(state["max_side"]))
    if grabbed == null:
        # Only the first frame waits. A later empty image means the window went away.
        if (state["images"] as Array).is_empty():
            state["empty"] = int(state["empty"]) + 1
            if int(state["empty"]) <= EMPTY_RETRIES:
                return true
        rt.finish_job(R.err(R.GODOT_ERROR, "The viewport returned no image."))
        return false
    if grabbed is Dictionary:
        rt.finish_job(grabbed)
        return false
    # Stamp the capture before the copy. The PNG write happens after the last frame.
    var captured_at := Time.get_ticks_msec()
    var kept: Variant = _keep_frame(state, grabbed as Image, captured_at)
    if kept is Dictionary:
        rt.finish_job(kept)
        return false
    if (state["images"] as Array).size() < int(state["want"]):
        state["pause"] = int(state["every"])
        state["phase"] = "pause"
        return true
    var written: Variant = _write_frames(state)
    if written is Dictionary:
        rt.finish_job(written)
        return false
    rt.finish_job(_finish(state))
    return false


static func _grab(viewport: Variant, max_side: int) -> Variant:
    if not viewport is Viewport or not is_instance_valid(viewport):
        return R.err(R.NOT_FOUND, "The viewport was freed before the recording.")
    var texture := (viewport as Viewport).get_texture()
    var image: Image = texture.get_image() if texture != null else null
    if image == null or image.is_empty():
        return null
    image.convert(Image.FORMAT_RGBA8)
    var size := image.get_size()
    var longest := maxi(size.x, size.y)
    if longest > max_side and longest > 0:
        var scale := float(max_side) / float(longest)
        image.resize(maxi(int(size.x * scale), 1), maxi(int(size.y * scale), 1), Image.INTERPOLATE_BILINEAR)
    return image


static func _keep_frame(state: Dictionary, image: Image, captured_at: int) -> Variant:
    if not bool(state["clock"]):
        state["clock"] = true
        state["started_msec"] = captured_at
    var index := (state["images"] as Array).size() + 1
    if index == 1:
        var pixels := int(state["want"]) * image.get_width() * image.get_height() * 4
        if pixels > MAX_PIXEL_BYTES:
            var megabytes := pixels / (1024 * 1024)
            return R.err(R.BAD_PARAMS, "This recording would keep %d MB of pixels in memory (frames × width × height × 4), above the 512 MB limit. Lower frames or max_dimension." % megabytes)
    var copy := image.duplicate() as Image
    if copy == null:
        return R.err(R.GODOT_ERROR, "Could not keep frame %d in memory." % index)
    (state["images"] as Array).append(copy)
    (state["meta"] as Array).append({
        "index": index,
        "file": "frame-%04d.png" % index,
        "width": image.get_width(),
        "height": image.get_height(),
        "process_frame": Engine.get_process_frames(),
        "msec": captured_at - int(state["started_msec"]),
    })
    return null


static func _write_frames(state: Dictionary) -> Variant:
    var images: Array = state["images"]
    var meta: Array = state["meta"]
    var directory := str(state["directory"])
    for index in images.size():
        var image := images[index] as Image
        var info: Dictionary = meta[index]
        var file_name := str(info["file"])
        var path := directory.path_join(file_name)
        var err := image.save_png(path)
        if err != OK:
            return R.err(R.GODOT_ERROR, "Could not save %s (%s)." % [file_name, error_string(err)])
        info["path"] = path
    # The reply lists paths, not pixels. Drop the images before the runtime answers.
    state["images"] = []
    return null


static func _finish(state: Dictionary) -> Dictionary:
    var meta: Array = state["meta"]
    var duration := 0
    if not meta.is_empty():
        duration = int((meta[meta.size() - 1] as Dictionary).get("msec", 0))
    return R.ok({
        "directory": str(state["directory"]),
        "count": meta.size(),
        "every": int(state["every"]),
        "duration_ms": duration,
        "frames": meta,
    })


static func _whole(raw: Variant) -> Variant:
    if raw is int:
        return int(raw)
    if raw is float and float(int(raw)) == float(raw):
        return int(raw)
    return null
