//! Conformance against the **shared** cross-language test vectors.
//!
//! The architecture spec mandates that every `corelib-<lang>` consume
//! `assets/test_vectors.json` — copied verbatim from the documentation
//! repository — as the single source of truth, rather than a divergent
//! hand-maintained copy. This suite embeds that file at build time and, for
//! every vector, checks the spec's scenarios:
//!
//! 1. **encode** — replay `fields[]`, assert output matches `serialized.hex`.
//! 2. **chunked-encode** — re-encode through tiny 1/3/7-byte flush buffers
//!    (exercising [`OStream`]'s buffer-full → flush → resume path) and assert
//!    the streamed-out bytes still match `serialized.hex`.
//! 3. **decode** — feed the official hex, assert the recovered fields match.
//! 4. **chunked-decode** — feed the same bytes one byte at a time, assert identical.
//! 5. **skip** — for vectors carrying `skip_ids`, a receiver that ignores those
//!    ids (skipping a `sequence_begin` skips its whole sub-tree) must still
//!    recover every other field, whole and chunked.
//!
//! Roundtrip (encode → decode) falls out of running (1) and (3) on every vector.
//!
//! ## What "skip" means in *this* port
//!
//! There is no receiver-declines-a-field call here. Unlike the C decoder, this
//! one has no per-field "bind a destination" step and no skip bookkeeping: it
//! walks every field on the wire and a [`Visitor`] simply does not record the
//! ids it is not interested in (`istream.rs`, [`Visitor`] docs). So the skip
//! scenario is expressed receiver-side — the ids in `skip_ids` are left unread
//! **at every nesting level**, and a skipped `sequence_begin` drops the whole
//! sub-sequence at any depth — while the length arithmetic each construct needs
//! (a varint's continuation bit, a `fixlen_word`'s length, `count` elements,
//! `count × element_length`, a sequence's end marker) is the decoder's ordinary
//! path rather than a separate skip path. The assertion is therefore the same
//! one the shared vectors are designed around: every *remaining* field decodes
//! to its exact value, in order, and the message is fully consumed
//! (`COMPLETE`) — an anchor read from the wrong offset still fails.
//!
//! **What that costs, and what buys it back.** Because the decoder walks the
//! declined bytes on its ordinary path, the *receiver-side* half of this
//! scenario cannot break independently of [`all_shared_vectors_conform`]: a
//! decoder mutation that corrupts a skipped construct corrupts the read field
//! next to it too, and both tests go red together. That is stated plainly so
//! the skip tally is not read as 58 vectors' worth of a code path nothing else
//! covers. What *is* its own is the chunking: this scenario sweeps **every
//! two-way split** of each message ([`decode_with_skip_split`]), the only feed
//! shape that makes the decoder resume a half-read construct and then keep
//! consuming in the same call. Neither a whole feed nor a byte-at-a-time feed
//! reaches it, so a mutation of that path — capping the bulk `fixlen` copy
//! against `fixlen_total` instead of `fixlen_remaining`, say — turns this test
//! red while [`all_shared_vectors_conform`] stays green. (Hand-written
//! `istream_tests` cover that path too; the point is only that within *this*
//! suite the skip scenario is no longer a strict subset of the plain one.)
//!
//! ## The skip matrix
//!
//! `assets/test_vectors.json` carries 58 vectors with `skip_ids`: the 36-vector
//! `skip/matrix` group (the full cross product of the ten skippable constructs,
//! as `[read P] [skipped S] [unsigned anchor]` chains), the 16-vector `skip`
//! group (empty payloads, two-byte lengths/counts, `fp64` element length,
//! three-byte header varints, and the skip at each message/sequence edge), and
//! six older `sequence`/`composite` vectors. Every one of them runs here whole,
//! one byte at a time, *and* once per two-way split of the message, so a resync
//! bug that a single-buffer feed hides has to show up at a chunk boundary.
//!
//! ## No fixed size in the loader
//!
//! The vector file is parsed with `serde_json` into growable values, so nothing
//! caps a `skip_ids` list, a field id, an element count or a payload length —
//! the C harness's `MAXSKIP`-style silent truncation has no counterpart. The one
//! fixed quantity left, the encode scratch buffer, is sized *from the vector's
//! own expected byte length* rather than a constant, and every loader step that
//! could narrow a value (id → [`Id`], element → its width) panics rather than
//! saturating. [`loader_reads_every_vector_whole`] pins that end of it.
//!
//! ## Not run here
//!
//! The file's top-level `sequence_growth` block (CORELIB_PLAN §7.2 item 8) is
//! carried by the verbatim copy but does not apply to this port: those cases
//! assert how a container grows as sequence-array elements arrive, and this
//! profile never grows one — no allocator, every destination fixed-capacity
//! caller storage (see the README's "Testing" section). The loader ignores a
//! top-level block it does not run rather than failing or warning on it.
//! ## `requires`-aware feature gating
//!
//! Each vector may carry a top-level `requires` array naming the optional
//! capabilities it needs (`fixlen` / `array` / `sequence` / `fp64` / `int64`).
//! This suite honours it: built without a feature, it **skips** the vectors that
//! need it, so the same vector file runs against every build configuration
//! (`cargo test --test vectors_tests --no-default-features --features …`). The
//! `int64` tag maps to this crate's `value64` feature.

mod common;

use common::{decode, decode_one_byte_at_a_time, hex_to_bytes, Event};
use serde_json::Value;
#[cfg(feature = "array")]
use sofab::ArrayKind;
use sofab::{Error, Flush, IStream, Id, OStream, Signed, Status, Unsigned, Visitor};
use std::collections::{BTreeMap, BTreeSet};

/// The shared vectors, embedded from the verbatim asset copy.
///
/// **Which column this suite asserts.** Every vector carries two encodings:
/// `serialized` — the dense, primitive-layer ground truth, in which every
/// sequence is framed — and `serialized_sparse`, the canonical *message-layer*
/// form where a sequence-typed field equal to its declared default is omitted
/// (MESSAGE_SPEC §2). This corelib has **no message layer**: it knows nothing
/// about schemas, declared defaults, or which closer a schema position calls
/// for, so it cannot produce the sparse form. This suite therefore asserts
/// `serialized` only, closing every sequence with `end_keep` (see
/// `write_fields`). `serialized_sparse` is exercised by the **generator's**
/// conformance drivers (`sofabgen`'s `tests/conformance/<lang>/`), which own the
/// layer that decides per field between `write_sequence_end` and
/// `write_sequence_end_keep`. The column is present here only because the asset
/// file is copied verbatim from the documentation repo; its absence from the
/// assertions below is deliberate, not a coverage gap.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

// --- requires / capability gating -------------------------------------------

/// The `requires` tags for a vector (empty if the key is absent).
fn parse_requires(v: &Value) -> Vec<&str> {
    v.get("requires")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Whether this build supports every capability a vector requires. `int64` maps
/// to the `value64` feature; unknown tags are assumed supported.
fn vector_supported(requires: &[&str]) -> bool {
    requires.iter().all(|r| match *r {
        "fixlen" => cfg!(feature = "fixlen"),
        "array" => cfg!(feature = "array"),
        "sequence" => cfg!(feature = "sequence"),
        "fp64" => cfg!(feature = "fp64"),
        "int64" => cfg!(feature = "value64"),
        _ => true,
    })
}

// --- helpers ----------------------------------------------------------------

/// A finite float as a JSON number, or `+/-infinity` as the strings `inf`/`-inf`.
#[cfg(feature = "fixlen")]
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

// The scalar value type is `u64`/`i64` (default) or `u32`/`i32` (`value64` off);
// the cast is only a no-op in the former, so silence the lint for the latter.
#[allow(clippy::unnecessary_cast)]
fn to_unsigned(v: u64) -> Unsigned {
    v as Unsigned
}
#[allow(clippy::unnecessary_cast)]
fn to_signed(v: i64) -> Signed {
    v as Signed
}

/// Print one scenario's tally: how many vectors ran, how many were gated out by
/// `requires`, and how many checks that came to.
///
/// Rust's test harness captures stdout for a *passing* test, so CI runs this
/// suite with `-- --nocapture` (see the `Vectors` job in
/// `.github/workflows/ci.yml`) to keep the counts in the run log; locally,
/// `cargo test --test vectors_tests -- --nocapture` does the same.
fn report(scenario: &str, vectors: usize, gated: usize, checks: usize) {
    println!(
        "[vectors] {scenario}: {vectors} vectors ran, {gated} gated out by \
         `requires`, {checks} checks",
    );
}

/// Read a field id as the wire type, refusing rather than truncating.
///
/// Every id in the file must survive the trip into [`Id`]; the skip group
/// carries ids up to `100001` (a three-byte header varint) and the id vectors
/// reach `ID_MAX`, so a lossy cast here would quietly test a different message.
fn field_id(f: &Value) -> Id {
    match f.get("id") {
        None => 0,
        Some(v) => {
            let raw = v.as_u64().expect("field id is a JSON integer");
            Id::try_from(raw).unwrap_or_else(|_| panic!("field id {raw} does not fit `Id`"))
        }
    }
}

fn bytes_to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Map a vector's `element_type` string to the array kind the decoder reports.
#[cfg(feature = "array")]
fn array_kind(element_type: &str) -> ArrayKind {
    match element_type {
        "u8" | "u16" | "u32" | "u64" => ArrayKind::Unsigned,
        "i8" | "i16" | "i32" | "i64" => ArrayKind::Signed,
        // A fixlen array names its element subtype, not a collapsed "fixlen"
        // category (§4.8): the decoder reports what the `fixlen_word` carried.
        #[cfg(feature = "fixlen")]
        "fp32" => ArrayKind::Fp32,
        #[cfg(feature = "fp64")]
        "fp64" => ArrayKind::Fp64,
        other => panic!("unknown element_type {other:?}"),
    }
}

// --- encode -----------------------------------------------------------------

/// Write a vector's `fields[]` into any stream (buffered or flushing). Feature-
/// gated per op; vectors needing a disabled op are filtered out by `requires`.
fn write_fields<F: Flush>(os: &mut OStream<F>, fields: &[Value]) {
    // A vector's `serialized` form is the primitive-layer ground truth and always
    // carries the frame, so every sequence closes with `end_keep`: identical bytes
    // once the sequence has content, and the empty-sequence vectors keep their
    // `begin`+`end` pair instead of vanishing.
    for f in fields {
        let op = f["op"].as_str().expect("op");
        let id = field_id(f);
        match op {
            "unsigned" => os
                .write_unsigned(id, to_unsigned(f["value"].as_u64().unwrap()))
                .unwrap(),
            "signed" => os
                .write_signed(id, to_signed(f["value"].as_i64().unwrap()))
                .unwrap(),
            "boolean" => os.write_boolean(id, f["value"].as_bool().unwrap()).unwrap(),
            #[cfg(feature = "fixlen")]
            "fp32" => os.write_fp32(id, as_f64(&f["value"]) as f32).unwrap(),
            #[cfg(feature = "fp64")]
            "fp64" => os.write_fp64(id, as_f64(&f["value"])).unwrap(),
            #[cfg(feature = "fixlen")]
            "string" => os.write_str(id, f["value"].as_str().unwrap()).unwrap(),
            #[cfg(feature = "fixlen")]
            "blob" => os
                .write_blob(id, &hex_to_bytes(f["value_hex"].as_str().unwrap()))
                .unwrap(),
            #[cfg(feature = "array")]
            "array" => encode_array(os, id, f),
            #[cfg(feature = "sequence")]
            "sequence_begin" => os.write_sequence_begin_lazy(id).unwrap(),
            #[cfg(feature = "sequence")]
            "sequence_end" => os.write_sequence_end_keep().unwrap(),
            other => panic!("unsupported op {other:?} (vector should be `requires`-skipped)"),
        }
    }
}

#[cfg(feature = "array")]
fn encode_array<F: Flush>(os: &mut OStream<F>, id: Id, f: &Value) {
    let et = f["element_type"].as_str().unwrap();
    let vals = f["values"].as_array().unwrap();
    match et {
        "u8" => os.write_array_unsigned(id, &u_vec::<u8>(vals)).unwrap(),
        "u16" => os.write_array_unsigned(id, &u_vec::<u16>(vals)).unwrap(),
        "u32" => os.write_array_unsigned(id, &u_vec::<u32>(vals)).unwrap(),
        #[cfg(feature = "value64")]
        "u64" => os.write_array_unsigned(id, &u_vec::<u64>(vals)).unwrap(),
        "i8" => os.write_array_signed(id, &i_vec::<i8>(vals)).unwrap(),
        "i16" => os.write_array_signed(id, &i_vec::<i16>(vals)).unwrap(),
        "i32" => os.write_array_signed(id, &i_vec::<i32>(vals)).unwrap(),
        #[cfg(feature = "value64")]
        "i64" => os.write_array_signed(id, &i_vec::<i64>(vals)).unwrap(),
        #[cfg(feature = "fixlen")]
        "fp32" => {
            let a: Vec<f32> = vals.iter().map(|v| as_f64(v) as f32).collect();
            os.write_array_fp32(id, &a).unwrap();
        }
        #[cfg(feature = "fp64")]
        "fp64" => {
            let a: Vec<f64> = vals.iter().map(as_f64).collect();
            os.write_array_fp64(id, &a).unwrap();
        }
        other => panic!("unsupported element_type {other:?}"),
    }
}

#[cfg(feature = "array")]
fn u_vec<T: TryFrom<u64>>(vals: &[Value]) -> Vec<T> {
    vals.iter()
        .map(|v| {
            T::try_from(v.as_u64().unwrap())
                .ok()
                .expect("u element fits")
        })
        .collect()
}

#[cfg(feature = "array")]
fn i_vec<T: TryFrom<i64>>(vals: &[Value]) -> Vec<T> {
    vals.iter()
        .map(|v| {
            T::try_from(v.as_i64().unwrap())
                .ok()
                .expect("i element fits")
        })
        .collect()
}

/// Encode `fields[]` into a single buffer, returning the message bytes (without
/// the reserved framing `offset`).
///
/// `expected_len` is the vector's own serialized length, and it — not a
/// constant — sizes the scratch buffer. A fixed `4096` used to stand here; it
/// happened to be large enough, but it is exactly the shape of cap the C
/// harness's `MAXSKIP` bug had: a constant that silently decides how much of a
/// vector can be tested. Sizing from the data means the 130-element arrays and
/// 130-byte payloads the skip group added need no bound to be revisited, and an
/// encoder that writes *more* than the ground truth hits `BufferFull` and
/// panics loudly instead of being compared away.
fn encode_fields(fields: &[Value], offset: usize, expected_len: usize) -> Vec<u8> {
    let mut buf = vec![0u8; offset + expected_len];
    let used = {
        let mut os = OStream::with_offset(&mut buf, offset).unwrap();
        write_fields(&mut os, fields);
        os.bytes_used()
    };
    buf[offset..used].to_vec()
}

/// Encode `fields[]` through a tiny `buf_size`-byte buffer with a flush sink, so
/// the encoder repeatedly fills, flushes, and resumes. Returns the streamed-out
/// bytes (the message is independent of any reserved offset, so we use 0).
fn chunked_encode(fields: &[Value], buf_size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut scratch = vec![0u8; buf_size];
    {
        let mut os =
            OStream::with_flush(&mut scratch, 0, |c: &[u8]| out.extend_from_slice(c)).unwrap();
        write_fields(&mut os, fields);
        os.flush();
    }
    out
}

// --- expected decode events -------------------------------------------------

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
    let id = field_id(f);
    match op {
        "unsigned" => ev.push(Event::Unsigned(
            id,
            to_unsigned(f["value"].as_u64().unwrap()),
        )),
        // booleans decode as plain unsigned 0/1.
        "boolean" => ev.push(Event::Unsigned(
            id,
            to_unsigned(f["value"].as_bool().unwrap() as u64),
        )),
        "signed" => ev.push(Event::Signed(id, to_signed(f["value"].as_i64().unwrap()))),
        #[cfg(feature = "fixlen")]
        "fp32" => ev.push(Event::Fp32(id, (as_f64(&f["value"]) as f32).to_bits())),
        #[cfg(feature = "fp64")]
        "fp64" => ev.push(Event::Fp64(id, as_f64(&f["value"]).to_bits())),
        #[cfg(feature = "fixlen")]
        "string" => ev.push(Event::Str(
            id,
            f["value"].as_str().unwrap().as_bytes().to_vec(),
        )),
        #[cfg(feature = "fixlen")]
        "blob" => ev.push(Event::Blob(
            id,
            hex_to_bytes(f["value_hex"].as_str().unwrap()),
        )),
        #[cfg(feature = "array")]
        "array" => expected_array_events(ev, id, f),
        #[cfg(feature = "sequence")]
        "sequence_begin" => ev.push(Event::SequenceBegin(id)),
        #[cfg(feature = "sequence")]
        "sequence_end" => ev.push(Event::SequenceEnd),
        other => panic!("unsupported op {other:?}"),
    }
}

#[cfg(feature = "array")]
fn expected_array_events(ev: &mut Vec<Event>, id: Id, f: &Value) {
    let et = f["element_type"].as_str().unwrap();
    let vals = f["values"].as_array().unwrap();
    ev.push(Event::ArrayBegin(id, array_kind(et), vals.len()));
    for v in vals {
        match et {
            "u8" | "u16" | "u32" => ev.push(Event::Unsigned(id, to_unsigned(v.as_u64().unwrap()))),
            #[cfg(feature = "value64")]
            "u64" => ev.push(Event::Unsigned(id, to_unsigned(v.as_u64().unwrap()))),
            "i8" | "i16" | "i32" => ev.push(Event::Signed(id, to_signed(v.as_i64().unwrap()))),
            #[cfg(feature = "value64")]
            "i64" => ev.push(Event::Signed(id, to_signed(v.as_i64().unwrap()))),
            #[cfg(feature = "fixlen")]
            "fp32" => ev.push(Event::Fp32(id, (as_f64(v) as f32).to_bits())),
            #[cfg(feature = "fp64")]
            "fp64" => ev.push(Event::Fp64(id, as_f64(v).to_bits())),
            other => panic!("unsupported element_type {other:?}"),
        }
    }
}

/// The events a receiver must observe for `fields[]` when it ignores `skip_ids`.
///
/// Scalars/arrays whose id is in `skip_ids` are dropped; a `sequence_begin`
/// whose id is in `skip_ids` drops the *entire* nested sequence (its begin,
/// everything inside, and the matching end), and decoding resumes after it.
fn expected_events_with_skip(fields: &[Value], skip: &[Id]) -> Vec<Event> {
    let mut ev = Vec::new();
    #[cfg(feature = "sequence")]
    let mut depth: u32 = 0;
    // `Some(d)` while inside a skipped sub-tree opened at depth `d`.
    #[allow(unused_mut)]
    let mut skip_until: Option<u32> = None;
    for f in fields {
        let op = f["op"].as_str().unwrap();
        let id = field_id(f);
        match op {
            #[cfg(feature = "sequence")]
            "sequence_begin" => {
                if skip_until.is_none() && skip.contains(&id) {
                    skip_until = Some(depth);
                } else if skip_until.is_none() {
                    ev.push(Event::SequenceBegin(id));
                }
                depth += 1;
            }
            #[cfg(feature = "sequence")]
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

// --- decode -----------------------------------------------------------------

/// A [`Visitor`] modelling a receiver that ignores a set of field `skip_ids`.
/// Scalars/arrays with a skipped id are dropped; a skipped `sequence_begin`
/// drops the whole nested sequence by tracking depth until the matching end.
struct SkipRecorder<'a> {
    skip: &'a [Id],
    events: Vec<Event>,
    #[cfg(feature = "fixlen")]
    pending: Option<(Id, bool, Vec<u8>)>,
    #[cfg(feature = "sequence")]
    depth: u32,
    #[cfg(feature = "sequence")]
    skip_until: Option<u32>,
}

impl<'a> SkipRecorder<'a> {
    fn new(skip: &'a [Id]) -> Self {
        SkipRecorder {
            skip,
            events: Vec::new(),
            #[cfg(feature = "fixlen")]
            pending: None,
            #[cfg(feature = "sequence")]
            depth: 0,
            #[cfg(feature = "sequence")]
            skip_until: None,
        }
    }

    fn skipping(&self) -> bool {
        #[cfg(feature = "sequence")]
        {
            self.skip_until.is_some()
        }
        #[cfg(not(feature = "sequence"))]
        {
            false
        }
    }

    fn drop_id(&self, id: Id) -> bool {
        self.skipping() || self.skip.contains(&id)
    }

    #[cfg(feature = "fixlen")]
    fn accumulate(&mut self, id: Id, is_blob: bool, total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.pending = Some((id, is_blob, Vec::with_capacity(total)));
        }
        let done = {
            let p = self.pending.as_mut().expect("chunk without begin");
            p.2.extend_from_slice(chunk);
            p.2.len() == total
        };
        if done {
            let (i, b, buf) = self.pending.take().unwrap();
            self.events.push(if b {
                Event::Blob(i, buf)
            } else {
                Event::Str(i, buf)
            });
        }
    }
}

impl Visitor for SkipRecorder<'_> {
    fn unsigned(&mut self, id: Id, v: Unsigned) {
        if !self.drop_id(id) {
            self.events.push(Event::Unsigned(id, v));
        }
    }
    fn signed(&mut self, id: Id, v: Signed) {
        if !self.drop_id(id) {
            self.events.push(Event::Signed(id, v));
        }
    }
    #[cfg(feature = "fixlen")]
    fn fp32(&mut self, id: Id, v: f32) {
        if !self.drop_id(id) {
            self.events.push(Event::Fp32(id, v.to_bits()));
        }
    }
    #[cfg(feature = "fp64")]
    fn fp64(&mut self, id: Id, v: f64) {
        if !self.drop_id(id) {
            self.events.push(Event::Fp64(id, v.to_bits()));
        }
    }
    #[cfg(feature = "fixlen")]
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if !self.drop_id(id) {
            self.accumulate(id, false, total, offset, chunk);
        }
    }
    #[cfg(feature = "fixlen")]
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        if !self.drop_id(id) {
            self.accumulate(id, true, total, offset, chunk);
        }
    }
    #[cfg(feature = "array")]
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        // Array elements arrive via the scalar/float callbacks with this id,
        // so a skipped id drops them too — only the header is handled here.
        if !self.drop_id(id) {
            self.events.push(Event::ArrayBegin(id, kind, count));
        }
    }
    #[cfg(feature = "sequence")]
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
    #[cfg(feature = "sequence")]
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
    assert_eq!(
        is.feed(bytes, &mut rec),
        Ok(Status::Complete),
        "skip decode"
    );
    rec.events
}

/// The same decode fed one byte at a time, so every skipped construct is
/// crossed by a chunk boundary. The final feed must report `COMPLETE`, not
/// `INCOMPLETE`: "the message is fully consumed" is half of what the skip
/// scenario asserts, and a resync that lands one byte off typically leaves the
/// decoder mid-field rather than mis-valuing an anchor.
fn decode_with_skip_chunked(bytes: &[u8], skip: &[Id]) -> Vec<Event> {
    let mut rec = SkipRecorder::new(skip);
    let mut is = IStream::new();
    let mut last: Result<Status, Error> = Ok(Status::Complete);
    for &b in bytes {
        last = is.feed(&[b], &mut rec);
        match last {
            Ok(Status::Complete) | Ok(Status::Incomplete) => {}
            Err(e) => panic!("skip chunked decode: {e:?}"),
        }
    }
    assert_eq!(
        last,
        Ok(Status::Complete),
        "skip chunked decode ended mid-message, not COMPLETE",
    );
    rec.events
}

/// The same decode fed as exactly **two** chunks, split at byte `at`.
///
/// Neither of the other two feeds reaches the decoder's resume-then-continue
/// path. A whole feed never resumes; a one-byte feed resumes on every byte but
/// always with a one-byte remainder, so anything that recomputes a *length*
/// against the new chunk gets the same answer either way. A two-way split is
/// the only shape where the decoder resumes a construct it was in the middle of
/// and then has more bytes to consume in the same call — which is precisely
/// what `IStream::feed`'s bulk `fixlen` path does (`take =
/// rest.len().min(fixlen_remaining)`, with a non-zero `offset`).
///
/// Sweeping *every* split point puts that boundary inside each declined
/// construct in turn: its header varint, its length word, its element count,
/// its payload, and the byte after it. This is the assertion that gives the
/// skip scenario bite of its own in this port — see the "What `skip` means in
/// *this* port" note at the top of the file for why the receiver-side skip
/// alone does not.
fn decode_with_skip_split(bytes: &[u8], at: usize, skip: &[Id]) -> Vec<Event> {
    let mut rec = SkipRecorder::new(skip);
    let mut is = IStream::new();
    match is.feed(&bytes[..at], &mut rec) {
        Ok(Status::Complete) | Ok(Status::Incomplete) => {}
        Err(e) => panic!("skip split decode (head of {at}): {e:?}"),
    }
    assert_eq!(
        is.feed(&bytes[at..], &mut rec),
        Ok(Status::Complete),
        "skip split decode (split at {at}) ended mid-message, not COMPLETE",
    );
    rec.events
}

// --- the suite --------------------------------------------------------------

#[test]
fn shared_vectors_present_and_parsed() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    assert_eq!(doc["format"], "sofabuffers-test-vectors");
    assert_eq!(doc["version"], 1);
    let vectors = doc["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty(), "expected at least one shared vector");
    assert!(
        vectors.iter().any(|v| v.get("requires").is_some()),
        "expected `requires` capability tags in the vector file",
    );

    // Top-level blocks beside `vectors` are read by the suite that owns them
    // (`invalid_utf8` by the UTF-8 tests) or by none — `sequence_growth`,
    // CORELIB_PLAN §7.2 item 8, does not apply to a profile that never grows a
    // container. The file is copied verbatim from `corelib-c-cpp`, so the loader
    // must tolerate a block it does not run rather than failing or warning on
    // it; asserting the block is present, and that the suite is green
    // regardless, is what "tolerated" means operationally.
    assert!(doc.get("invalid_utf8").is_some(), "invalid_utf8 block");
    assert!(
        doc.get("sequence_growth").is_some(),
        "sequence_growth block (carried, not run here)",
    );
}

#[test]
fn all_shared_vectors_conform() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut ran = 0;
    let mut checks = 0;
    let mut gated = 0;
    for vec in vectors {
        if !vector_supported(&parse_requires(vec)) {
            gated += 1;
            continue; // capability disabled in this build — skip per `requires`
        }
        ran += 1;

        let name = vec["name"].as_str().unwrap();
        let offset = vec["offset"].as_u64().unwrap_or(0) as usize;
        let fields = vec["fields"].as_array().unwrap();
        let expected_hex = vec["serialized"]["hex"].as_str().unwrap();
        let expected_bytes = hex_to_bytes(expected_hex);

        // 1. Vector encode: replay fields, bytes must match the ground truth.
        let encoded = encode_fields(fields, offset, expected_bytes.len());
        assert_eq!(
            encoded,
            expected_bytes,
            "[{name}] encode mismatch:\n  got {}\n  exp {expected_hex}",
            bytes_to_hex(&encoded),
        );
        checks += 1;

        // 2. Chunked encode: stream out through tiny flush buffers.
        for &bs in &[1usize, 3, 7] {
            assert_eq!(
                chunked_encode(fields, bs),
                expected_bytes,
                "[{name}] chunked-encode (buffer={bs}) mismatch",
            );
            checks += 1;
        }

        // 3. Vector decode: feed the official bytes, recovered fields must match.
        let want = expected_events(fields);
        assert_eq!(decode(&expected_bytes), want, "[{name}] decode mismatch");
        checks += 1;

        // 4. Chunked decode: one byte at a time yields identical events.
        assert_eq!(
            decode_one_byte_at_a_time(&expected_bytes),
            want,
            "[{name}] chunked decode mismatch",
        );
        checks += 1;
    }

    assert!(ran > 0, "no vectors ran for this feature configuration");
    report("encode/decode", ran, gated, checks);
}

#[test]
fn unsupported_vectors_are_rejected_not_ignored() {
    // The inversion of `all_shared_vectors_conform`, and the coverage shape a
    // feature powerset otherwise misses: every leg asks "does what I enabled
    // work?" and nothing asks "what happens to what I disabled?". A matrix in
    // which each leg drops the tests for what it turned off is not coverage.
    //
    // This corelib implements a *subset* of the wire format when features are
    // turned off — that is the point of turning them off, and carrying a parser
    // for a wire type that can never be delivered would defeat it. So a message
    // that needs a capability this build does not have is `INVALID`, not
    // partially decoded: the `requires` mask names exactly those vectors, and
    // each one must be rejected rather than quietly mis-decoded or waved
    // through.
    //
    // With every feature on there is nothing unsupported and this test is
    // vacuous by construction — which is asserted, so it cannot rot into
    // silently running nothing in the configuration CI runs most.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut ran = 0;
    let mut rejected_after_delivering = 0;
    for vec in vectors {
        let requires = parse_requires(vec);
        if vector_supported(&requires) {
            continue;
        }
        ran += 1;

        let name = vec["name"].as_str().unwrap();
        let bytes = hex_to_bytes(vec["serialized"]["hex"].as_str().unwrap());

        // Whole, and one byte at a time: `INVALID` is terminal and latched
        // (§5.2), so the verdict must not depend on where the chunks fall.
        let (outcome, delivered) = common::feed(&bytes);
        assert_eq!(
            outcome,
            Err(Error::InvalidMsg),
            "[{name}] needs {requires:?}, which this build lacks — the message \
             must be rejected, not decoded",
        );
        // The rejection happens *at* the unsupported construct, so whatever
        // stood before it in the message has already reached the visitor. The
        // outcome is the only truth: a caller must discard those fields rather
        // than keep a half-message. Counted so the hazard is demonstrably real
        // and not merely documented.
        if !delivered.is_empty() {
            rejected_after_delivering += 1;
        }

        let mut rec = common::Recorder::new();
        let mut is = IStream::new();
        let mut chunked = Ok(Status::Complete);
        for b in &bytes {
            chunked = is.feed(&[*b], &mut rec);
            if chunked == Err(Error::InvalidMsg) {
                break;
            }
        }
        assert_eq!(
            chunked,
            Err(Error::InvalidMsg),
            "[{name}] chunked feeding changed the verdict",
        );
    }

    let all_on = cfg!(all(
        feature = "fixlen",
        feature = "array",
        feature = "sequence",
        feature = "fp64",
        feature = "value64"
    ));
    if all_on {
        assert_eq!(
            ran, 0,
            "with every feature on, no vector can be unsupported"
        );
    } else {
        assert!(
            ran > 0,
            "this build disables a capability but no vector exercised it",
        );
        assert!(
            rejected_after_delivering > 0,
            "expected at least one vector to deliver fields before the \
             unsupported construct rejected the message — the partial-delivery \
             hazard the feature-flag docs warn about",
        );
    }
}

/// Every id in `skip_ids`, read without a cap.
///
/// The C harness carried a fixed `MAXSKIP` and *truncated* a longer list: the
/// surplus ids were then read instead of skipped, so the vector still passed
/// while testing less than it claimed. Nothing here bounds the list — it is a
/// `Vec` sized by the JSON — and the length is asserted against the JSON array
/// so a future cap cannot reintroduce the silent version. The new vectors reach
/// nine entries.
fn parse_skip_ids(v: &Value) -> Option<Vec<Id>> {
    let arr = v.get("skip_ids")?.as_array().expect("skip_ids is an array");
    let ids: Vec<Id> = arr.iter().map(field_id_of_value).collect();
    assert_eq!(
        ids.len(),
        arr.len(),
        "skip_ids truncated while loading ({} of {})",
        ids.len(),
        arr.len(),
    );
    Some(ids)
}

fn field_id_of_value(v: &Value) -> Id {
    let raw = v.as_u64().expect("skip id is a JSON integer");
    Id::try_from(raw).unwrap_or_else(|_| panic!("skip id {raw} does not fit `Id`"))
}

/// The construct a skipped field is, at the granularity the skip matrix varies:
/// the eight wire types, with `fixlen` and `fixlen` arrays split by subtype
/// because a decoder branches on the subtype to learn the element length.
///
/// Used only to prove the matrix is actually being exercised — see
/// [`skip_ids_vectors_conform`].
fn skipped_construct(f: &Value) -> String {
    let op = f["op"].as_str().unwrap();
    match op {
        "array" => {
            let et = f["element_type"].as_str().unwrap();
            match et.chars().next().unwrap() {
                'u' => "array<unsigned>".into(),
                'i' => "array<signed>".into(),
                _ => format!("array<fixlen:{et}>"),
            }
        }
        "fp32" | "fp64" | "string" | "blob" => format!("fixlen:{op}"),
        "sequence_begin" => "sequence".into(),
        other => other.into(),
    }
}

/// The constructs the full build must see declined across the skip vectors:
/// the ten the matrix crosses, plus `boolean` and the second `fixlen` array
/// subtype that the `skip` group adds.
#[cfg(all(
    feature = "fixlen",
    feature = "array",
    feature = "sequence",
    feature = "fp64",
    feature = "value64"
))]
const SKIPPED_CONSTRUCTS: &[&str] = &[
    "unsigned",
    "signed",
    "boolean",
    "fixlen:fp32",
    "fixlen:fp64",
    "fixlen:string",
    "fixlen:blob",
    "array<unsigned>",
    "array<signed>",
    "array<fixlen:fp32>",
    "array<fixlen:fp64>",
    "sequence",
];

#[test]
fn skip_ids_vectors_conform() {
    // The spec's `skip_ids` scenario: a receiver that ignores those ids (a
    // skipped `sequence_begin` skips the whole sub-tree, at any depth) must
    // still recover every other field, in order and with its exact value,
    // without losing decoder sync and with the message fully consumed —
    // including over chunk boundaries, where a resync bug that a single-buffer
    // feed hides tends to show up.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();

    let mut seen = 0;
    let mut gated = 0;
    let mut checks = 0;
    let mut splits = 0;
    let mut by_group: BTreeMap<&str, usize> = BTreeMap::new();
    let mut constructs: BTreeSet<String> = BTreeSet::new();

    for vec in vectors {
        let skip_ids = match parse_skip_ids(vec) {
            Some(ids) => ids,
            None => continue, // fields are skipped only where `skip_ids` says so
        };
        if !vector_supported(&parse_requires(vec)) {
            gated += 1;
            continue;
        }
        seen += 1;
        *by_group
            .entry(vec["group"].as_str().unwrap_or("(none)"))
            .or_default() += 1;

        let name = vec["name"].as_str().unwrap();
        let fields = vec["fields"].as_array().unwrap();
        let bytes = hex_to_bytes(vec["serialized"]["hex"].as_str().unwrap());

        // Every id named must exist in the vector, at whatever nesting level —
        // a skip set that names nothing would make the whole vector vacuous
        // without any assertion below noticing.
        for id in &skip_ids {
            let target = fields.iter().find(|f| field_id(f) == *id);
            let target = target
                .unwrap_or_else(|| panic!("[{name}] skip id {id} names no field in this vector"));
            constructs.insert(skipped_construct(target));
        }

        let want = expected_events_with_skip(fields, &skip_ids);
        // Sanity: the skip set must actually drop something.
        assert!(
            want.len() < expected_events(fields).len(),
            "[{name}] skip_ids dropped nothing",
        );

        assert_eq!(
            decode_with_skip(&bytes, &skip_ids),
            want,
            "[{name}] skip decode mismatch",
        );
        checks += 1;
        assert_eq!(
            decode_with_skip_chunked(&bytes, &skip_ids),
            want,
            "[{name}] skip chunked decode mismatch",
        );
        checks += 1;

        // Every two-way split of the message, so each declined construct is cut
        // at every byte inside it and the decoder has to resume *and then keep
        // going* in the same feed. See `decode_with_skip_split`.
        for at in 1..bytes.len() {
            assert_eq!(
                decode_with_skip_split(&bytes, at, &skip_ids),
                want,
                "[{name}] skip split decode mismatch (split at {at} of {})",
                bytes.len(),
            );
            checks += 1;
            splits += 1;
        }
    }

    report("skip", seen, gated, checks);
    println!("[vectors] skip: {splits} two-way splits swept");
    println!("[vectors] skip: groups {by_group:?}");
    println!(
        "[vectors] skip: declined constructs ({}) {:?}",
        constructs.len(),
        constructs,
    );

    // Under the full build every shared skip vector is supported, so the tally
    // is exact — and it is the guard against re-copying a stale asset: the
    // 131-vector file carries 58 vectors with `skip_ids`, 36 of them the
    // `skip/matrix` cross product and 16 the `skip` axes beside it. The old
    // 81-vector file had 8 in total.
    #[cfg(all(
        feature = "fixlen",
        feature = "array",
        feature = "sequence",
        feature = "fp64",
        feature = "value64"
    ))]
    {
        assert_eq!(gated, 0, "nothing can be gated out with every feature on");
        assert_eq!(seen, 58, "expected all 58 shared skip vectors");
        // The split sweep is the skip scenario's own assertion (see
        // `decode_with_skip_split`); pin its size so it cannot shrink to a
        // token one-split-per-vector without failing here.
        assert!(
            splits > 2_000,
            "expected a full two-way split sweep, got {splits} splits",
        );
        assert_eq!(by_group.get("skip/matrix"), Some(&36), "skip/matrix group");
        assert_eq!(by_group.get("skip"), Some(&16), "skip group");
        // Not vacuous: every skippable construct is actually declined
        // somewhere, including the two — `array<signed>` and `array<fixlen>` —
        // that were never a declined field before this vector file.
        let want: BTreeSet<String> = SKIPPED_CONSTRUCTS.iter().map(|s| (*s).into()).collect();
        assert_eq!(constructs, want, "declined-construct coverage");
    }
    // A reduced build runs the part of the matrix it can represent; `requires`
    // decides which part, and at least some of it must survive.
    assert!(
        seen > 0,
        "no skip vector ran for this feature configuration"
    );
}

#[test]
fn loader_reads_every_vector_whole() {
    // Acceptance criterion 4 of the adoption issue, stated as a test rather than
    // as the absence of a constant: no fixed size in the loader may silently
    // truncate a `skip_ids` list, a payload, an element count or an id. The
    // sizes below are the ones the new vectors introduced — the suite would
    // still pass if a cap quietly clipped them, which is exactly how the C
    // harness's `MAXSKIP` bug stayed invisible, so they are pinned as *observed*
    // maxima: if a future change caps one, this test fails instead of testing
    // less.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let vectors = doc["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 131, "the 131-vector shared file");

    let mut with_skip_ids = 0;
    let mut max_skip_ids = 0;
    let mut max_id: u64 = 0;
    let mut max_elements = 0;
    let mut max_payload = 0;
    let mut fp64_arrays = 0;

    for vec in vectors {
        if let Some(ids) = parse_skip_ids(vec) {
            with_skip_ids += 1;
            max_skip_ids = max_skip_ids.max(ids.len());
        }
        for f in vec["fields"].as_array().unwrap() {
            // `field_id` refuses a lossy conversion; compare it back against the
            // JSON so a *silent* narrowing cannot pass either.
            let raw = f.get("id").and_then(Value::as_u64).unwrap_or(0);
            assert_eq!(u64::from(field_id(f)), raw, "field id narrowed");
            max_id = max_id.max(raw);
            if let Some(vals) = f.get("values").and_then(Value::as_array) {
                max_elements = max_elements.max(vals.len());
                if f["element_type"] == "fp64" {
                    fp64_arrays += 1;
                }
            }
            if let Some(t) = f.get("value").and_then(Value::as_str) {
                max_payload = max_payload.max(t.len());
            }
            if let Some(h) = f.get("value_hex").and_then(Value::as_str) {
                max_payload = max_payload.max(h.len() / 2);
            }
        }
    }

    assert_eq!(with_skip_ids, 58, "vectors carrying skip_ids");
    assert!(
        max_skip_ids >= 9,
        "skip_ids lists of 9 (saw {max_skip_ids})"
    );
    assert!(max_id >= 100_001, "field ids past 100001 (saw {max_id})");
    assert!(
        max_elements >= 130,
        "130-element arrays (saw {max_elements})"
    );
    assert!(max_payload >= 130, "130-byte payloads (saw {max_payload})");
    assert!(fp64_arrays > 0, "fp64 arrays");
    println!(
        "[vectors] loader: {} vectors, {with_skip_ids} with skip_ids (longest {max_skip_ids}), \
         max id {max_id}, max array {max_elements} elements, max payload {max_payload} bytes, \
         {fp64_arrays} fp64 arrays",
        vectors.len(),
    );
}
