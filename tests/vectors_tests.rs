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
//! 4. **Skip** — for vectors carrying `skip_ids`, a receiver that ignores those
//!    ids (skipping a `sequence_begin` skips its whole sub-tree) must still
//!    recover every other field, whole and chunked.
//!
//! Roundtrip (encode → decode) falls out of running (1) and (2) on every vector.

mod common;

use common::{Event, Recorder};
use serde_json::Value;
use sofab::{ArrayKind, IStream, Id, OStream, Signed, Unsigned, Visitor};

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
        push_field_events(&mut ev, f);
    }
    ev
}

/// Append the decoder events for a single `fields[]` entry.
fn push_field_events(ev: &mut Vec<Event>, f: &Value) {
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
        "array" => expected_array_events(ev, id, f),
        "sequence_begin" => ev.push(Event::SequenceBegin(id)),
        "sequence_end" => ev.push(Event::SequenceEnd),
        other => panic!("unknown op {other:?}"),
    }
}

/// The events a receiver must observe for `fields[]` when it ignores `skip_ids`.
///
/// Scalars/arrays whose id is in `skip_ids` are dropped; a `sequence_begin`
/// whose id is in `skip_ids` drops the *entire* nested sequence (its begin,
/// everything inside, and the matching end), and decoding resumes after it.
/// This mirrors the C decoder's auto-skip; in this push/visitor port the same
/// behaviour is realised by a [`SkipRecorder`] that tracks nesting depth.
fn expected_events_with_skip(fields: &[Value], skip: &[u32]) -> Vec<Event> {
    let mut ev = Vec::new();
    let mut depth: u32 = 0;
    // `Some(d)` while inside a skipped sub-tree that was opened at depth `d`.
    let mut skip_until: Option<u32> = None;
    for f in fields {
        let op = f["op"].as_str().unwrap();
        let id = f.get("id").and_then(Value::as_u64).unwrap_or(0) as u32;
        match op {
            "sequence_begin" => {
                if skip_until.is_none() && skip.contains(&id) {
                    skip_until = Some(depth);
                } else if skip_until.is_none() {
                    ev.push(Event::SequenceBegin(id));
                }
                depth += 1;
            }
            "sequence_end" => {
                depth -= 1;
                match skip_until {
                    Some(d) if d == depth => skip_until = None,
                    Some(_) => {}
                    None => ev.push(Event::SequenceEnd),
                }
            }
            _ => {
                if skip_until.is_none() && !skip.contains(&id) {
                    push_field_events(&mut ev, f);
                }
            }
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

/// A [`Visitor`] modelling a receiver that ignores a set of field `skip_ids`.
///
/// Scalars/arrays with a skipped id are dropped; encountering a skipped
/// `sequence_begin` drops the whole nested sequence by tracking depth until the
/// matching `sequence_end`. This is how an application performs the spec's
/// "auto-skip" in the push/visitor model — the decoder still streams every
/// field, and the visitor chooses what to keep.
struct SkipRecorder<'a> {
    skip: &'a [Id],
    events: Vec<Event>,
    depth: u32,
    skip_until: Option<u32>,
    // chunked string/blob accumulator for kept fields: (id, is_blob, buffer)
    pending: Option<(Id, bool, Vec<u8>)>,
}

impl<'a> SkipRecorder<'a> {
    fn new(skip: &'a [Id]) -> Self {
        SkipRecorder {
            skip,
            events: Vec::new(),
            depth: 0,
            skip_until: None,
            pending: None,
        }
    }

    fn skipping(&self) -> bool {
        self.skip_until.is_some()
    }

    /// Drop this field's payload? True while inside a skipped sub-tree, or when
    /// the field's own id is in `skip_ids`.
    fn drop_id(&self, id: Id) -> bool {
        self.skipping() || self.skip.contains(&id)
    }

    fn accumulate(&mut self, id: Id, is_blob: bool, total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.pending = Some((id, is_blob, Vec::with_capacity(total)));
        }
        let (_, _, buf) = self.pending.as_mut().expect("chunk without begin");
        buf.extend_from_slice(chunk);
        if buf.len() == total {
            let (pid, pblob, buf) = self.pending.take().unwrap();
            self.events.push(if pblob {
                Event::Blob(pid, buf)
            } else {
                Event::Str(pid, buf)
            });
        }
    }
}

impl Visitor for SkipRecorder<'_> {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        if !self.drop_id(id) {
            self.events.push(Event::Unsigned(id, value));
        }
    }
    fn signed(&mut self, id: Id, value: Signed) {
        if !self.drop_id(id) {
            self.events.push(Event::Signed(id, value));
        }
    }
    fn fp32(&mut self, id: Id, value: f32) {
        if !self.drop_id(id) {
            self.events.push(Event::Fp32(id, value.to_bits()));
        }
    }
    fn fp64(&mut self, id: Id, value: f64) {
        if !self.drop_id(id) {
            self.events.push(Event::Fp64(id, value.to_bits()));
        }
    }
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if !self.drop_id(id) {
            self.accumulate(id, false, total, offset, chunk);
        }
    }
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if !self.drop_id(id) {
            self.accumulate(id, true, total, offset, chunk);
        }
    }
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        // Array elements arrive via the scalar/float callbacks with this same
        // id, so a skipped id drops them too — only the header is handled here.
        if !self.drop_id(id) {
            self.events.push(Event::ArrayBegin(id, kind, count));
        }
    }
    fn sequence_begin(&mut self, id: Id) {
        if !self.skipping() {
            if self.skip.contains(&id) {
                self.skip_until = Some(self.depth);
            } else {
                self.events.push(Event::SequenceBegin(id));
            }
        }
        self.depth += 1;
    }
    fn sequence_end(&mut self) {
        self.depth -= 1;
        match self.skip_until {
            Some(d) if d == self.depth => self.skip_until = None,
            Some(_) => {}
            None => self.events.push(Event::SequenceEnd),
        }
    }
}

fn decode_with_skip(bytes: &[u8], skip: &[Id]) -> Vec<Event> {
    let mut rec = SkipRecorder::new(skip);
    let mut is = IStream::new();
    is.feed(bytes, &mut rec).expect("skip decode");
    rec.events
}

fn decode_with_skip_chunked(bytes: &[u8], skip: &[Id]) -> Vec<Event> {
    let mut rec = SkipRecorder::new(skip);
    let mut is = IStream::new();
    for &b in bytes {
        is.feed(&[b], &mut rec).expect("skip chunked decode");
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

#[test]
fn skip_ids_vectors_conform() {
    // The spec's `skip_ids` dimension: a receiver that ignores those ids (a
    // skipped `sequence_begin` skips the whole sub-tree) must still recover every
    // other field, in order, without losing decoder sync — including over chunk
    // boundaries.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut seen = 0;
    for vec in vectors {
        let skip_ids: Vec<Id> = match vec.get("skip_ids").and_then(Value::as_array) {
            Some(a) => a.iter().map(|x| x.as_u64().unwrap() as Id).collect(),
            None => continue,
        };
        seen += 1;

        let name = vec["name"].as_str().unwrap();
        let fields = vec["fields"].as_array().unwrap();
        let bytes = hex_to_bytes(vec["serialized"]["hex"].as_str().unwrap());

        let want = expected_events_with_skip(fields, &skip_ids);
        // Sanity: the skip set must actually drop something, otherwise the vector
        // would not be exercising the feature.
        assert!(
            want.len() < expected_events(fields).len(),
            "[{name}] skip_ids dropped nothing",
        );

        assert_eq!(
            decode_with_skip(&bytes, &skip_ids),
            want,
            "[{name}] skip decode mismatch",
        );
        assert_eq!(
            decode_with_skip_chunked(&bytes, &skip_ids),
            want,
            "[{name}] skip chunked decode mismatch",
        );
    }

    assert!(seen >= 8, "expected the shared skip vectors (saw {seen})");
}

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
