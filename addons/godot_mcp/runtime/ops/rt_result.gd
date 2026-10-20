extends RefCounted
## Result helpers for the in-game runtime. Every method returns {"result"} or {"error"}.

const BAD_PARAMS := "BAD_PARAMS"
const NOT_FOUND := "NOT_FOUND"
const UNSUPPORTED := "UNSUPPORTED"
const GODOT_ERROR := "GODOT_ERROR"
const DENIED := "DENIED"


static func err(code: String, message: String, data: Variant = null) -> Dictionary:
    var body := {"code": code, "message": message}
    if data != null:
        body["data"] = data
    return {"error": body}


static func ok(result: Dictionary) -> Dictionary:
    return {"result": result}


static func is_err(value: Variant) -> bool:
    return value is Dictionary and (value as Dictionary).has("error")
