//! Godot's binary `encode_variant` / `decode_variant`, as used by the remote debugger.
//!
//! Layout matches Godot 4.x `core/io/marshalls.cpp` and was checked against the
//! shin2344234/godot-mcp encoder (`server/src/game/variant.ts`), which was verified on
//! Godot 4.7.2. Numbers are little-endian. Every value starts with a 32-bit header:
//! the low 8 bits are `Variant::Type`, bit 16 selects 64-bit numbers (and "object as id").
//! Payloads are padded to 4 bytes.
//!
//! Encoding covers what this server sends: null, bool, integers, floats, strings, arrays
//! and string-keyed objects. Decoding also accepts the math and packed types a game may
//! send back (stack variables, error payloads).

// The decoder reads bytes from another process, so a panic here would take the server down:
// no unwrap or expect outside the tests.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Map, Number, Value};

const NIL: u32 = 0;
const BOOL: u32 = 1;
const INT: u32 = 2;
const FLOAT: u32 = 3;
const STRING: u32 = 4;
const VECTOR2: u32 = 5;
const VECTOR2I: u32 = 6;
const RECT2: u32 = 7;
const RECT2I: u32 = 8;
const VECTOR3: u32 = 9;
const VECTOR3I: u32 = 10;
const TRANSFORM2D: u32 = 11;
const VECTOR4: u32 = 12;
const VECTOR4I: u32 = 13;
const PLANE: u32 = 14;
const QUATERNION: u32 = 15;
const AABB: u32 = 16;
const BASIS: u32 = 17;
const TRANSFORM3D: u32 = 18;
const PROJECTION: u32 = 19;
const COLOR: u32 = 20;
const STRING_NAME: u32 = 21;
const NODE_PATH: u32 = 22;
const RID: u32 = 23;
const OBJECT: u32 = 24;
const CALLABLE: u32 = 25;
const SIGNAL: u32 = 26;
const DICTIONARY: u32 = 27;
const ARRAY: u32 = 28;
const PACKED_BYTE_ARRAY: u32 = 29;
const PACKED_INT32_ARRAY: u32 = 30;
const PACKED_INT64_ARRAY: u32 = 31;
const PACKED_FLOAT32_ARRAY: u32 = 32;
const PACKED_FLOAT64_ARRAY: u32 = 33;
const PACKED_STRING_ARRAY: u32 = 34;
const PACKED_VECTOR2_ARRAY: u32 = 35;
const PACKED_VECTOR3_ARRAY: u32 = 36;
const PACKED_COLOR_ARRAY: u32 = 37;
const PACKED_VECTOR4_ARRAY: u32 = 38;

const FLAG_64: u32 = 1 << 16;
const TYPE_MASK: u32 = 0xff;
const MAX_DEPTH: usize = 64;
const MAX_PACKET: usize = 8 << 20;

const KIND_NONE: u32 = 0;
const KIND_BUILTIN: u32 = 1;
const KIND_CLASS: u32 = 2;
const KIND_SCRIPT: u32 = 3;

#[derive(Debug)]
pub struct VariantError(pub String);

impl std::fmt::Display for VariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn need(&self, n: usize) -> Result<(), VariantError> {
        if self.pos + n > self.buf.len() {
            Err(VariantError(format!(
                "variant data ends early (need {n} bytes at {} of {})",
                self.pos,
                self.buf.len()
            )))
        } else {
            Ok(())
        }
    }

    /// Bytes left after the current position.
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    /// How many elements to reserve room for. The count comes from the peer, so it is capped by
    /// what the data can hold (every element takes at least four bytes); a count that promises
    /// more fails later on the missing data instead of reserving memory for it.
    fn capacity_for(&self, count: usize) -> usize {
        count.min(self.remaining() / 4)
    }

    fn take<const N: usize>(&mut self) -> Result<[u8; N], VariantError> {
        self.need(N)?;
        let mut out = [0_u8; N];
        out.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        Ok(out)
    }

    fn u32(&mut self) -> Result<u32, VariantError> {
        Ok(u32::from_le_bytes(self.take()?))
    }

    fn i32(&mut self) -> Result<i32, VariantError> {
        Ok(self.u32()? as i32)
    }

    fn i64(&mut self) -> Result<i64, VariantError> {
        Ok(i64::from_le_bytes(self.take()?))
    }

    fn u64(&mut self) -> Result<u64, VariantError> {
        Ok(u64::from_le_bytes(self.take()?))
    }

    fn f32(&mut self) -> Result<f32, VariantError> {
        Ok(f32::from_le_bytes(self.u32()?.to_le_bytes()))
    }

    fn f64(&mut self) -> Result<f64, VariantError> {
        Ok(f64::from_le_bytes(self.u64()?.to_le_bytes()))
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], VariantError> {
        self.need(n)?;
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn str(&mut self) -> Result<String, VariantError> {
        let len = self.u32()? as usize;
        if len > self.buf.len() {
            return Err(VariantError(format!(
                "string length {len} exceeds the packet"
            )));
        }
        let bytes = self.bytes(len)?;
        let text = String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .to_string();
        self.pos += (4 - (len % 4)) % 4;
        Ok(text)
    }
}

/// Decode one Variant. Returns the JSON value and the number of bytes consumed.
pub fn decode_variant(buf: &[u8]) -> Result<(Value, usize), VariantError> {
    let mut reader = Reader { buf, pos: 0 };
    let value = read_value(&mut reader, 0)?;
    Ok((value, reader.pos))
}

fn read_container_type(reader: &mut Reader<'_>, kind: u32) -> Result<(), VariantError> {
    match kind {
        KIND_NONE => Ok(()),
        KIND_BUILTIN => {
            reader.u32()?;
            Ok(())
        }
        KIND_CLASS | KIND_SCRIPT => {
            reader.str()?;
            Ok(())
        }
        other => Err(VariantError(format!("invalid container type kind {other}"))),
    }
}

fn num(n: f64) -> String {
    if n.is_finite() {
        let rounded = (n * 1_000_000.0).round() / 1_000_000.0;
        if rounded.fract() == 0.0 {
            format!("{}", rounded as i64)
        } else {
            format!("{rounded}")
        }
    } else if n.is_nan() {
        "nan".into()
    } else if n.is_sign_positive() {
        "inf".into()
    } else {
        "-inf".into()
    }
}

fn math_spec(type_id: u32) -> Option<(&'static str, usize, bool)> {
    Some(match type_id {
        VECTOR2 => ("Vector2", 2, false),
        VECTOR2I => ("Vector2i", 2, true),
        RECT2 => ("Rect2", 4, false),
        RECT2I => ("Rect2i", 4, true),
        VECTOR3 => ("Vector3", 3, false),
        VECTOR3I => ("Vector3i", 3, true),
        TRANSFORM2D => ("Transform2D", 6, false),
        VECTOR4 => ("Vector4", 4, false),
        VECTOR4I => ("Vector4i", 4, true),
        PLANE => ("Plane", 4, false),
        QUATERNION => ("Quaternion", 4, false),
        AABB => ("AABB", 6, false),
        BASIS => ("Basis", 9, false),
        TRANSFORM3D => ("Transform3D", 12, false),
        PROJECTION => ("Projection", 16, false),
        _ => return None,
    })
}

fn read_value(reader: &mut Reader<'_>, depth: usize) -> Result<Value, VariantError> {
    if depth > MAX_DEPTH {
        return Err(VariantError("variant is nested too deeply".into()));
    }
    let header = reader.u32()?;
    let type_id = header & TYPE_MASK;
    let wide = header & FLAG_64 != 0;
    let real = |reader: &mut Reader<'_>, wide: bool| -> Result<f64, VariantError> {
        if wide {
            reader.f64()
        } else {
            Ok(f64::from(reader.f32()?))
        }
    };

    if let Some((name, count, ints)) = math_spec(type_id) {
        let mut parts = Vec::with_capacity(count);
        for _ in 0..count {
            if ints {
                parts.push(num(f64::from(reader.i32()?)));
            } else {
                parts.push(num(real(reader, wide)?));
            }
        }
        return Ok(Value::String(format!("{name}({})", parts.join(", "))));
    }

    match type_id {
        NIL => Ok(Value::Null),
        BOOL => Ok(Value::Bool(reader.u32()? != 0)),
        INT => {
            if wide {
                Ok(Value::Number(reader.i64()?.into()))
            } else {
                Ok(Value::Number(reader.i32()?.into()))
            }
        }
        FLOAT => {
            let n = if wide {
                reader.f64()?
            } else {
                f64::from(reader.f32()?)
            };
            Ok(Number::from_f64(n)
                .map(Value::Number)
                .unwrap_or(Value::Null))
        }
        STRING | STRING_NAME => Ok(Value::String(reader.str()?)),
        COLOR => {
            let parts = [
                num(f64::from(reader.f32()?)),
                num(f64::from(reader.f32()?)),
                num(f64::from(reader.f32()?)),
                num(f64::from(reader.f32()?)),
            ];
            Ok(Value::String(format!("Color({})", parts.join(", "))))
        }
        NODE_PATH => {
            let first = reader.u32()?;
            if first & 0x8000_0000 == 0 {
                reader.pos -= 4;
                return Ok(Value::String(reader.str()?));
            }
            let names = (first & 0x7fff_ffff) as usize;
            let mut subnames = reader.u32()? as usize;
            let flags = reader.u32()?;
            if flags & 2 != 0 {
                subnames += 1;
            }
            let mut parts = Vec::with_capacity(reader.capacity_for(names + subnames));
            for _ in 0..names + subnames {
                parts.push(reader.str()?);
            }
            let path = parts[..names].join("/");
            let sub = if subnames == 0 {
                String::new()
            } else {
                format!(":{}", parts[names..].join(":"))
            };
            let absolute = if flags & 1 != 0 { "/" } else { "" };
            Ok(Value::String(format!("{absolute}{path}{sub}")))
        }
        RID => Ok(Value::String(format!("RID({})", reader.u64()?))),
        OBJECT => {
            if wide {
                let id = reader.u64()?;
                if id == 0 {
                    Ok(Value::Null)
                } else {
                    Ok(serde_json::json!({ "object_id": id.to_string() }))
                }
            } else {
                let class = reader.str()?;
                if class.is_empty() {
                    return Ok(Value::Null);
                }
                let count = reader.u32()? as usize;
                let mut properties = Map::new();
                for _ in 0..count {
                    let name = reader.str()?;
                    properties.insert(name, read_value(reader, depth + 1)?);
                }
                Ok(serde_json::json!({ "object": class, "properties": properties }))
            }
        }
        CALLABLE => Ok(Value::String("Callable()".into())),
        SIGNAL => {
            let name = reader.str()?;
            let id = reader.u64()?;
            Ok(serde_json::json!({ "signal": name, "object_id": id.to_string() }))
        }
        DICTIONARY => {
            read_container_type(reader, (header >> 16) & 3)?;
            read_container_type(reader, (header >> 18) & 3)?;
            let count = (reader.u32()? & 0x7fff_ffff) as usize;
            let mut map = Map::new();
            for _ in 0..count {
                let key = read_value(reader, depth + 1)?;
                let value = read_value(reader, depth + 1)?;
                let key = match key {
                    Value::String(text) => text,
                    other => other.to_string(),
                };
                map.insert(key, value);
            }
            Ok(Value::Object(map))
        }
        ARRAY => {
            read_container_type(reader, (header >> 16) & 3)?;
            let count = (reader.u32()? & 0x7fff_ffff) as usize;
            let mut items = Vec::with_capacity(reader.capacity_for(count));
            for _ in 0..count {
                items.push(read_value(reader, depth + 1)?);
            }
            Ok(Value::Array(items))
        }
        PACKED_BYTE_ARRAY => {
            let n = reader.u32()? as usize;
            let bytes = reader.bytes(n)?;
            let values: Vec<Value> = bytes
                .iter()
                .map(|byte| Value::Number((*byte).into()))
                .collect();
            reader.pos += (4 - (n % 4)) % 4;
            Ok(Value::Array(values))
        }
        PACKED_INT32_ARRAY => read_packed(reader, |r| Ok(Value::Number(r.i32()?.into()))),
        PACKED_INT64_ARRAY => read_packed(reader, |r| Ok(Value::Number(r.i64()?.into()))),
        PACKED_FLOAT32_ARRAY => read_packed(reader, |r| {
            Ok(Number::from_f64(f64::from(r.f32()?))
                .map(Value::Number)
                .unwrap_or(Value::Null))
        }),
        PACKED_FLOAT64_ARRAY => read_packed(reader, |r| {
            Ok(Number::from_f64(r.f64()?)
                .map(Value::Number)
                .unwrap_or(Value::Null))
        }),
        PACKED_STRING_ARRAY => read_packed(reader, |r| Ok(Value::String(r.str()?))),
        PACKED_VECTOR2_ARRAY => read_packed(reader, |r| {
            Ok(Value::String(format!(
                "Vector2({}, {})",
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?))
            )))
        }),
        PACKED_VECTOR3_ARRAY => read_packed(reader, |r| {
            Ok(Value::String(format!(
                "Vector3({}, {}, {})",
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?))
            )))
        }),
        PACKED_VECTOR4_ARRAY => read_packed(reader, |r| {
            Ok(Value::String(format!(
                "Vector4({}, {}, {}, {})",
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?))
            )))
        }),
        PACKED_COLOR_ARRAY => read_packed(reader, |r| {
            Ok(Value::String(format!(
                "Color({}, {}, {}, {})",
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?)),
                num(f64::from(r.f32()?))
            )))
        }),
        other => Err(VariantError(format!("unknown Variant type {other}"))),
    }
}

fn read_packed(
    reader: &mut Reader<'_>,
    mut item: impl FnMut(&mut Reader<'_>) -> Result<Value, VariantError>,
) -> Result<Value, VariantError> {
    let count = reader.u32()? as usize;
    let mut items = Vec::with_capacity(reader.capacity_for(count));
    for _ in 0..count {
        items.push(item(reader)?);
    }
    Ok(Value::Array(items))
}

/// Encode a JSON value as a Godot Variant.
pub fn encode_variant(value: &Value) -> Result<Vec<u8>, VariantError> {
    let mut out = Vec::new();
    write_value(value, &mut out, 0)?;
    Ok(out)
}

fn write_u32(out: &mut Vec<u8>, n: u32) {
    out.extend_from_slice(&n.to_le_bytes());
}

fn write_string(out: &mut Vec<u8>, text: &str) {
    let bytes = text.as_bytes();
    write_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
    out.resize(out.len() + (4 - (bytes.len() % 4)) % 4, 0);
}

fn write_value(value: &Value, out: &mut Vec<u8>, depth: usize) -> Result<(), VariantError> {
    if depth > MAX_DEPTH {
        return Err(VariantError("value is nested too deeply to encode".into()));
    }
    match value {
        Value::Null => write_u32(out, NIL),
        Value::Bool(flag) => {
            write_u32(out, BOOL);
            write_u32(out, u32::from(*flag));
        }
        Value::Number(number) => write_number(number, out),
        Value::String(text) => {
            write_u32(out, STRING);
            write_string(out, text);
        }
        Value::Array(items) => {
            write_u32(out, ARRAY);
            write_u32(out, items.len() as u32);
            for item in items {
                write_value(item, out, depth + 1)?;
            }
        }
        Value::Object(map) => {
            write_u32(out, DICTIONARY);
            write_u32(out, map.len() as u32);
            for (key, item) in map {
                write_value(&Value::String(key.clone()), out, depth + 1)?;
                write_value(item, out, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn write_number(number: &Number, out: &mut Vec<u8>) {
    if let Some(n) = number.as_i64() {
        if i64::from(i32::MIN) <= n && n <= i64::from(i32::MAX) {
            write_u32(out, INT);
            write_u32(out, n as u32);
            return;
        }
        write_u32(out, INT | FLAG_64);
        out.extend_from_slice(&n.to_le_bytes());
        return;
    }
    if let Some(n) = number.as_u64() {
        if i32::try_from(n).is_ok() {
            write_u32(out, INT);
            write_u32(out, n as u32);
            return;
        }
        write_u32(out, INT | FLAG_64);
        out.extend_from_slice(&(n as i64).to_le_bytes());
        return;
    }
    let n = number.as_f64().unwrap_or(0.0);
    write_u32(out, FLOAT | FLAG_64);
    out.extend_from_slice(&n.to_le_bytes());
}

/// Debugger packet: little-endian byte length, then one Variant.
pub fn frame_packet(value: &Value) -> Result<Vec<u8>, VariantError> {
    let body = encode_variant(value)?;
    if body.len() > MAX_PACKET {
        return Err(VariantError(format!(
            "debugger packet is too large ({} bytes)",
            body.len()
        )));
    }
    let mut framed = Vec::with_capacity(4 + body.len());
    write_u32(&mut framed, body.len() as u32);
    framed.extend_from_slice(&body);
    Ok(framed)
}

/// Splits a TCP byte stream into decoded debugger packets.
#[derive(Debug, Default)]
pub struct PacketReader {
    pending: Vec<u8>,
}

impl PacketReader {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Value>, VariantError> {
        self.pending.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(prefix) = self.pending.first_chunk::<4>() {
            let size = u32::from_le_bytes(*prefix) as usize;
            if size > MAX_PACKET {
                return Err(VariantError(format!(
                    "debugger packet too large ({size} bytes)"
                )));
            }
            if self.pending.len() < 4 + size {
                break;
            }
            let body = self.pending[4..4 + size].to_vec();
            self.pending.drain(..4 + size);
            let (value, used) = decode_variant(&body)?;
            if used != body.len() {
                return Err(VariantError(format!(
                    "variant used {used} bytes of a {size}-byte packet"
                )));
            }
            out.push(value);
        }
        Ok(out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roundtrip(value: Value) {
        let encoded = encode_variant(&value).expect("encode");
        let (decoded, used) = decode_variant(&encoded).expect("decode");
        assert_eq!(used, encoded.len());
        assert_eq!(decoded, value);
    }

    #[test]
    fn scalars_and_containers_roundtrip() {
        roundtrip(Value::Null);
        roundtrip(json!(true));
        roundtrip(json!(false));
        roundtrip(json!(0));
        roundtrip(json!(-1));
        roundtrip(json!(42));
        roundtrip(json!(i64::from(i32::MAX)));
        roundtrip(json!(i64::from(i32::MIN)));
        roundtrip(json!(""));
        roundtrip(json!("hi"));
        roundtrip(json!("héllo"));
        roundtrip(json!(["a", 1, true, null]));
        roundtrip(json!({"cmd": "mcp:request", "n": 7}));
    }

    #[test]
    fn wide_integers_roundtrip() {
        roundtrip(json!(i64::from(i32::MAX) + 1));
        roundtrip(json!(i64::from(i32::MIN) - 1));
    }

    #[test]
    fn framed_packets_split_across_reads() {
        let first = frame_packet(&json!(["set_pid", 1, [7]])).unwrap();
        let second = frame_packet(&json!("ok")).unwrap();
        let mut reader = PacketReader::default();
        assert!(reader.push(&first[..3]).unwrap().is_empty());
        let mut rest = first[3..].to_vec();
        rest.extend_from_slice(&second[..1]);
        let one = reader.push(&rest).unwrap();
        assert_eq!(one, vec![json!(["set_pid", 1, [7]])]);
        let two = reader.push(&second[1..]).unwrap();
        assert_eq!(two, vec![json!("ok")]);
    }

    #[test]
    fn oversized_packet_is_rejected() {
        let mut reader = PacketReader::default();
        let huge = ((MAX_PACKET as u32) + 1).to_le_bytes();
        let err = reader.push(&huge).expect_err("too large");
        assert!(err.0.contains("too large"), "{err}");
    }

    /// A peer chooses the element counts. A twelve-byte packet must not make the decoder
    /// reserve memory for billions of elements; it has to fail on the missing data instead.
    #[test]
    fn counts_larger_than_the_data_fail_without_reserving_memory() {
        for (type_id, count) in [
            (ARRAY, 0x7fff_ffff_u32),
            (PACKED_INT64_ARRAY, 0xffff_ffff),
            (PACKED_STRING_ARRAY, 0xffff_ffff),
            (PACKED_COLOR_ARRAY, 0xffff_ffff),
        ] {
            let mut buf = Vec::new();
            write_u32(&mut buf, type_id);
            write_u32(&mut buf, count);
            let err = decode_variant(&buf).expect_err("the data ends right after the count");
            assert!(err.0.contains("ends early"), "type {type_id}: {err}");
        }
    }

    #[test]
    fn node_path_name_counts_larger_than_the_data_fail_without_reserving_memory() {
        let mut buf = Vec::new();
        write_u32(&mut buf, NODE_PATH);
        write_u32(&mut buf, 0x8000_0000 | 0x7fff_ffff);
        write_u32(&mut buf, 0xffff_ffff);
        write_u32(&mut buf, 0);
        let err = decode_variant(&buf).expect_err("no names follow");
        assert!(err.0.contains("ends early"), "{err}");
    }

    #[test]
    fn color_decodes_as_text() {
        let mut buf = Vec::new();
        write_u32(&mut buf, COLOR);
        for component in [1.0f32, 0.0, 0.0, 1.0] {
            buf.extend_from_slice(&component.to_le_bytes());
        }
        let (value, _) = decode_variant(&buf).unwrap();
        assert_eq!(value, json!("Color(1, 0, 0, 1)"));
    }
}
