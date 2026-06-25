//! Conformance against the **shared** cross-language test vectors.
//!
//! The architecture spec mandates that every `corelib-<lang>` consume
//! `assets/test_vectors.json` — copied verbatim from the documentation
//! repository — as the single source of truth, rather than a divergent
//! hand-maintained copy. This suite embeds that file at build time and, for
//! every vector, checks the spec's required categories:
//!
//! 1. **Vector encode** — replay `fields[]`, assert output matches `serialized.hex`.
//! 2. **Vector decode** — feed the official hex, assert the recovered fields match.
//! 3. **Chunked streaming** — feed the same bytes one at a time, assert identical.
//!
//! Roundtrip (encode → decode) falls out of running (1) and (2) on every vector.

mod common;

use common::{Event, Recorder};
use serde_json::Value;
use sofab::{ArrayKind, IStream, OStream};

/// The shared vectors, embedded from the verbatim asset copy.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

// --- helpers ----------------------------------------------------------------

/// A finite float as a JSON number, or `+/-infinity` as the strings `inf`/`-inf`.
fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.as_f64().expect("float number"),
        Value::String(s) => match s.as_str() {
            "inf" => f64::INFINITY,
            "-inf" => f64::NEG_INFINITY,
            other => panic!("unexpected float string {other:?}"),
        },
        other => panic!("unexpected float JSON {other:?}"),
    }
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd hex length");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
        .collect()
}

/// Map a vector's `element_type` string to the array kind the decoder reports.
fn array_kind(element_type: &str) -> ArrayKind {
    match element_type {
        "u8" | "u16" | "u32" | "u64" => ArrayKind::Unsigned,
        "i8" | "i16" | "i32" | "i64" => ArrayKind::Signed,
        "fp32" | "fp64" => ArrayKind::Fixlen,
        other => panic!("unknown element_type {other:?}"),
    }
}

/// Encode one vector's `fields[]` exactly as the spec's encode operations.
fn encode_fields(fields: &[Value], offset: usize) -> Vec<u8> {
    // Generous fixed buffer; vectors are small.
    let mut buf = vec![0u8; 4096];
    let used = {
        let mut os = OStream::with_offset(&mut buf, offset);
        for f in fields {
            let op = f["op"].as_str().expect("op");
            let id = f.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
            match op {
                "unsigned" => os.write_unsigned(id, f["value"].as_u64().unwrap()).unwrap(),
                "signed" => os.write_signed(id, f["value"].as_i64().unwrap()).unwrap(),
                "boolean" => os.write_boolean(id, f["value"].as_bool().unwrap()).unwrap(),
                "fp32" => os.write_fp32(id, as_f64(&f["value"]) as f32).unwrap(),
                "fp64" => os.write_fp64(id, as_f64(&f["value"])).unwrap(),
                "string" => os.write_str(id, f["value"].as_str().unwrap()).unwrap(),
                "blob" => os
                    .write_blob(id, &hex_to_bytes(f["value_hex"].as_str().unwrap()))
                    .unwrap(),
                "array" => encode_array(&mut os, id, f),
                "sequence_begin" => os.write_sequence_begin(id).unwrap(),
                "sequence_end" => os.write_sequence_end().unwrap(),
                other => panic!("unknown op {other:?}"),
            }
        }
        os.bytes_used()
    };
    // The message is the bytes after the reserved framing offset.
    buf[offset..used].to_vec()
}

fn encode_array(os: &mut OStream, id: u32, f: &Value) {
    let et = f["element_type"].as_str().unwrap();
    let vals = f["values"].as_array().unwrap();
    match et {
        "u8" => os.write_array_unsigned(id, &u_vec::<u8>(vals)).unwrap(),
        "u16" => os.write_array_unsigned(id, &u_vec::<u16>(vals)).unwrap(),
        "u32" => os.write_array_unsigned(id, &u_vec::<u32>(vals)).unwrap(),
        "u64" => os.write_array_unsigned(id, &u_vec::<u64>(vals)).unwrap(),
        "i8" => os.write_array_signed(id, &i_vec::<i8>(vals)).unwrap(),
        "i16" => os.write_array_signed(id, &i_vec::<i16>(vals)).unwrap(),
        "i32" => os.write_array_signed(id, &i_vec::<i32>(vals)).unwrap(),
        "i64" => os.write_array_signed(id, &i_vec::<i64>(vals)).unwrap(),
        "fp32" => {
            let a: Vec<f32> = vals.iter().map(|v| as_f64(v) as f32).collect();
            os.write_array_fp32(id, &a).unwrap();
        }
        "fp64" => {
            let a: Vec<f64> = vals.iter().map(as_f64).collect();
            os.write_array_fp64(id, &a).unwrap();
        }
        other => panic!("unknown element_type {other:?}"),
    }
}

fn u_vec<T: TryFrom<u64>>(vals: &[Value]) -> Vec<T> {
    vals.iter()
        .map(|v| {
            T::try_from(v.as_u64().unwrap())
                .ok()
                .expect("u element fits")
        })
        .collect()
}

fn i_vec<T: TryFrom<i64>>(vals: &[Value]) -> Vec<T> {
    vals.iter()
        .map(|v| {
            T::try_from(v.as_i64().unwrap())
                .ok()
                .expect("i element fits")
        })
        .collect()
}

/// The events a correct decoder must emit for one vector's `fields[]`.
fn expected_events(fields: &[Value]) -> Vec<Event> {
    let mut ev = Vec::new();
    for f in fields {
        let op = f["op"].as_str().unwrap();
        let id = f.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
        match op {
            "unsigned" => ev.push(Event::Unsigned(id, f["value"].as_u64().unwrap())),
            // booleans decode as plain unsigned 0/1.
            "boolean" => ev.push(Event::Unsigned(id, f["value"].as_bool().unwrap() as u64)),
            "signed" => ev.push(Event::Signed(id, f["value"].as_i64().unwrap())),
            "fp32" => ev.push(Event::Fp32(id, (as_f64(&f["value"]) as f32).to_bits())),
            "fp64" => ev.push(Event::Fp64(id, as_f64(&f["value"]).to_bits())),
            "string" => ev.push(Event::Str(
                id,
                f["value"].as_str().unwrap().as_bytes().to_vec(),
            )),
            "blob" => ev.push(Event::Blob(
                id,
                hex_to_bytes(f["value_hex"].as_str().unwrap()),
            )),
            "array" => expected_array_events(&mut ev, id, f),
            "sequence_begin" => ev.push(Event::SequenceBegin(id)),
            "sequence_end" => ev.push(Event::SequenceEnd),
            other => panic!("unknown op {other:?}"),
        }
    }
    ev
}

fn expected_array_events(ev: &mut Vec<Event>, id: u32, f: &Value) {
    let et = f["element_type"].as_str().unwrap();
    let vals = f["values"].as_array().unwrap();
    ev.push(Event::ArrayBegin(id, array_kind(et), vals.len()));
    for v in vals {
        match et {
            "u8" | "u16" | "u32" | "u64" => ev.push(Event::Unsigned(id, v.as_u64().unwrap())),
            "i8" | "i16" | "i32" | "i64" => ev.push(Event::Signed(id, v.as_i64().unwrap())),
            "fp32" => ev.push(Event::Fp32(id, (as_f64(v) as f32).to_bits())),
            "fp64" => ev.push(Event::Fp64(id, as_f64(v).to_bits())),
            other => panic!("unknown element_type {other:?}"),
        }
    }
}

fn decode(bytes: &[u8]) -> Vec<Event> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    is.feed(bytes, &mut rec).expect("decode");
    rec.events
}

fn decode_one_byte_at_a_time(bytes: &[u8]) -> Vec<Event> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for &b in bytes {
        is.feed(&[b], &mut rec).expect("chunked decode");
    }
    rec.events
}

// --- the suite ---------------------------------------------------------------

#[test]
fn shared_vectors_present_and_parsed() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    assert_eq!(doc["format"], "sofabuffers-test-vectors");
    assert_eq!(doc["version"], 1);
    let n = doc["vectors"].as_array().expect("vectors array").len();
    assert!(n > 0, "expected at least one shared vector");
}

#[test]
fn all_shared_vectors_conform() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    for vec in vectors {
        let name = vec["name"].as_str().unwrap();
        let offset = vec["offset"].as_u64().unwrap_or(0) as usize;
        let fields = vec["fields"].as_array().unwrap();
        let expected_hex = vec["serialized"]["hex"].as_str().unwrap();
        let expected_bytes = hex_to_bytes(expected_hex);

        // 1. Vector encode: replay fields, bytes must match the ground truth.
        let encoded = encode_fields(fields, offset);
        assert_eq!(
            encoded,
            expected_bytes,
            "[{name}] encode mismatch:\n  got {}\n  exp {expected_hex}",
            bytes_to_hex(&encoded),
        );

        // 2. Vector decode: feed the official bytes, recovered fields must match.
        let want = expected_events(fields);
        assert_eq!(decode(&expected_bytes), want, "[{name}] decode mismatch");

        // 3. Chunked streaming: one byte at a time yields identical events.
        assert_eq!(
            decode_one_byte_at_a_time(&expected_bytes),
            want,
            "[{name}] chunked decode mismatch",
        );
    }
}

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
