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
//!
//! ## Header-ceiling cases — `header_limits`
//!
//! The file's fourth block: bytes that **declare** a length or count and then
//! **end**, with no payload behind them (`02 a2 06` — a 100-byte string
//! declared at id 0, and the message stops there). The ceiling is decided at
//! that word, before the payload is asked for, so the answer is the ceiling's
//! and it is **terminal** — never `INCOMPLETE`, which §5.2.1 defines as the
//! outcome more bytes *can* change.
//!
//! Which ceiling speaks is the subject, and the two give opposite answers on
//! the same word: a schema `maxlen` breach is `invalid` (MESSAGE_SPEC §7.1), a
//! §6.2.1 receiver-cap breach is `limit_exceeded`. `header_string_over_cap` and
//! `header_string_schema_bounded` carry the *identical* bytes and differ only in
//! which ceiling the case configures.
//!
//! **This profile runs the schema-bounded pair and skips the other eight.**
//! `receiver_caps` is a profile capability, declared by a port whose generated
//! code carries §6.2.1 caps *distinct from* schema bounds; this one refuses
//! schema-unbounded fields at generate time, so it has no such cap and does not
//! declare the tag. In this block an unsatisfied `requires` tag means **skip**,
//! for every tag — the cases assert a rejection with a specific *category*, so a
//! build that cannot represent the construct would reject it for an unrelated
//! reason and appear to pass while testing nothing. The skips are counted,
//! printed and asserted ([`header_limits_cases_conform`]), never silent.
//!
//! The ceiling itself is generated code's, as everywhere in this family: the
//! corelib is schema-agnostic and enforces no limit of its own (`error.rs`,
//! [`Error::LimitExceeded`]). What it owes — and what these cases exercise — is
//! that the length word is *reported* at the word, through
//! [`Visitor::fixlen_begin`], before any payload byte, so the consumer holding
//! the bound can answer there. `header_limits_cases_conform` models that
//! consumer.
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
    // `header_limits` *is* run here, in part: see `header_limits_cases_conform`.
    assert!(doc.get("header_limits").is_some(), "header_limits block");
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

// --- the `header_limits` block ----------------------------------------------
//
// See the module-level "Header-ceiling cases" note for what this port runs and
// what it skips.

/// The outcome vocabulary of a header-ceiling case.
///
/// Four values, because the block's whole subject is keeping two of them apart:
/// a schema bound answers `invalid`, a §6.2.1 receiver cap answers
/// `limit_exceeded`, and neither may answer `incomplete` — §5.2.1 defines that
/// as the outcome more bytes *can* change, and after a ceiling has fired
/// nothing can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderOutcome {
    Incomplete,
    Complete,
    Invalid,
    LimitExceeded,
}

impl HeaderOutcome {
    fn parse(s: &str) -> Self {
        match s {
            "incomplete" => HeaderOutcome::Incomplete,
            "complete" => HeaderOutcome::Complete,
            "invalid" => HeaderOutcome::Invalid,
            "limit_exceeded" => HeaderOutcome::LimitExceeded,
            other => panic!("unknown header_limits outcome `{other}`"),
        }
    }

    fn is_rejection(self) -> bool {
        matches!(self, HeaderOutcome::Invalid | HeaderOutcome::LimitExceeded)
    }
}

/// Which ceiling a case configures. A case carries `schema` **or** `limits`,
/// never both: §6.2.1 forbids applying a receiver cap to a field the schema
/// already bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ceiling<'a> {
    /// `"schema": { "maxlen": N }` — a breach is `invalid` (MESSAGE_SPEC §7.1).
    SchemaMaxlen(usize),
    /// `"limits": { "max_dyn_…": N }` — a breach is `limit_exceeded` (§6.2.1).
    /// This profile has no such cap; the cases carrying one are gated out.
    ReceiverCap(&'a str, usize),
}

impl<'a> Ceiling<'a> {
    /// The key a case's ceiling is grouped under, for the paired-control check.
    fn key(&self) -> &'a str {
        match *self {
            Ceiling::SchemaMaxlen(_) => "schema.maxlen",
            Ceiling::ReceiverCap(k, _) => k,
        }
    }
}

/// The ceiling a header-ceiling case configures for its run.
fn header_ceiling(case: &Value) -> Ceiling<'_> {
    let schema = case.get("schema").and_then(|s| s.get("maxlen"));
    let limits = case.get("limits").and_then(Value::as_object);
    match (schema, limits) {
        (Some(m), None) => {
            Ceiling::SchemaMaxlen(m.as_u64().expect("maxlen is an integer") as usize)
        }
        (None, Some(l)) => {
            assert_eq!(l.len(), 1, "a case configures exactly one receiver cap");
            let (k, v) = l.iter().next().unwrap();
            Ceiling::ReceiverCap(
                k.as_str(),
                v.as_u64().expect("a cap is an integer") as usize,
            )
        }
        (Some(_), Some(_)) => panic!("a case carries `schema` and `limits` — §6.2.1 forbids both"),
        (None, None) => panic!("a header-ceiling case carries no ceiling at all"),
    }
}

/// Whether one `requires` tag of a header-ceiling case holds for this build and
/// this profile.
///
/// **An unsatisfied tag means SKIP here, for every tag** — unlike a *vector*,
/// where an unsatisfied wire-construct tag turns the vector into a negative
/// case. These cases already assert a rejection *with a specific category*, so a
/// build that cannot represent the construct would reject it for an unrelated
/// reason and appear to pass while testing nothing.
///
/// `receiver_caps` is a **profile** capability: a port declares it when its
/// generated code carries §6.2.1 receiver caps *distinct from* schema bounds.
/// This profile refuses schema-unbounded fields at generate time (see the
/// module note), so it has no such cap and does not declare the tag —
/// inventing one to make the cases run would test a ceiling nothing here
/// implements. An unknown tag is treated the same way: a capability that did not
/// exist when this reader was written is one this port has not shown it has.
fn header_tag_satisfied(tag: &str) -> bool {
    match tag {
        // Wire constructs: whether this *build* can represent them.
        "fixlen" => cfg!(feature = "fixlen"),
        "array" => cfg!(feature = "array"),
        "sequence" => cfg!(feature = "sequence"),
        "fp64" => cfg!(feature = "fp64"),
        "int64" => cfg!(feature = "value64"),
        // Everything else: whether this *profile* declares the capability.
        other => header_profile_capability(other),
    }
}

/// Whether this profile declares the named profile capability. It declares
/// none — see [`header_tag_satisfied`] for why `receiver_caps` in particular is
/// not one of them, and why an unrecognised tag is answered the same way.
fn header_profile_capability(_tag: &str) -> bool {
    false
}

fn header_case_supported(requires: &[&str]) -> bool {
    requires.iter().all(|r| header_tag_satisfied(r))
}

/// The consumer half of a schema-bounded field, as generated code would carry
/// it: the `maxlen` lives here, the corelib knows nothing about it, and the
/// bound is latched at [`Visitor::fixlen_begin`] — the length word, before any
/// payload byte (`istream.rs`, and `fixlen_header_tests`).
#[cfg(feature = "fixlen")]
#[derive(Debug)]
struct SchemaBoundedField {
    field_id: Id,
    maxlen: usize,
    /// The `total` the corelib announced at the length word, if it announced one.
    announced: Option<usize>,
    /// The bound was breached by the announced length.
    breached: bool,
    /// Every callback the decoder made, so "a further feed consumed nothing"
    /// can be asserted rather than assumed.
    deliveries: usize,
}

#[cfg(feature = "fixlen")]
impl Visitor for SchemaBoundedField {
    fn fixlen_begin(&mut self, id: Id, subtype: sofab::FixlenType, total: usize) {
        self.deliveries += 1;
        // The subtype is the one *on the wire*: a field whose schema declares
        // something else is a §7.3 skip, not a `maxlen` measurement (§4.8).
        if id == self.field_id && subtype == sofab::FixlenType::Str && total > self.maxlen {
            self.breached = true;
        }
        if id == self.field_id {
            self.announced = Some(total);
        }
    }
    fn string(&mut self, _id: Id, _total: usize, _offset: usize, _chunk: &[u8]) {
        self.deliveries += 1;
    }
    fn blob(&mut self, _id: Id, _total: usize, _offset: usize, _chunk: &[u8]) {
        self.deliveries += 1;
    }
    fn unsigned(&mut self, _id: Id, _value: Unsigned) {
        self.deliveries += 1;
    }
}

/// A receiver for one case: the corelib's decoder plus the generated code that
/// holds the ceiling. `feed` is the whole contract the block asserts — the
/// outcome per chunk, and terminality across chunks.
#[cfg(feature = "fixlen")]
struct HeaderReceiver {
    stream: IStream,
    field: SchemaBoundedField,
    /// A latched terminal verdict. Once a ceiling has answered, §6.3 makes the
    /// rejection terminal: a further feed **re-raises** it and consumes nothing.
    verdict: Option<HeaderOutcome>,
}

#[cfg(feature = "fixlen")]
impl HeaderReceiver {
    fn new(field_id: Id, maxlen: usize) -> Self {
        Self {
            stream: IStream::new(),
            field: SchemaBoundedField {
                field_id,
                maxlen,
                announced: None,
                breached: false,
                deliveries: 0,
            },
            verdict: None,
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> HeaderOutcome {
        if let Some(v) = self.verdict {
            return v; // terminal: re-raised, and the bytes are not consumed
        }
        let status = self.stream.feed(chunk, &mut self.field);
        // The schema bound is decided at the length word, so it is read before
        // the decoder's own outcome: for a message that ends *at* that word the
        // decoder says INCOMPLETE and the bound says INVALID, and §5.2 makes
        // INVALID dominate.
        if self.field.breached {
            self.verdict = Some(HeaderOutcome::Invalid);
            return HeaderOutcome::Invalid;
        }
        match status {
            Ok(Status::Complete) => HeaderOutcome::Complete,
            Ok(Status::Incomplete) => HeaderOutcome::Incomplete,
            Err(Error::InvalidMsg) => {
                self.verdict = Some(HeaderOutcome::Invalid);
                HeaderOutcome::Invalid
            }
            Err(Error::LimitExceeded) => {
                self.verdict = Some(HeaderOutcome::LimitExceeded);
                HeaderOutcome::LimitExceeded
            }
            Err(e) => panic!("unexpected decoder error {e:?}"),
        }
    }
}

/// Feed one case's bytes under a `maxlen` and return the receiver and the
/// outcome of the last feed.
///
/// `chunks`, where present, is how the bytes are delivered — the verdict is a
/// property of the bytes and not of the split (§7.2 item 4), so the chunks must
/// reassemble to `serialized`, and a decoder that compares before the varint is
/// complete sees a truncated number.
#[cfg(feature = "fixlen")]
fn feed_header_case(case: &Value, field_id: Id, maxlen: usize) -> (HeaderReceiver, HeaderOutcome) {
    let name = case["name"].as_str().expect("case name");
    let bytes = hex_to_bytes(case["serialized"].as_str().expect("serialized"));
    let chunks: Vec<Vec<u8>> = match case.get("chunks") {
        Some(cs) => cs
            .as_array()
            .expect("chunks is an array")
            .iter()
            .map(|c| hex_to_bytes(c.as_str().expect("a chunk is a hex string")))
            .collect(),
        None => vec![bytes.clone()],
    };
    assert_eq!(
        chunks.concat(),
        bytes,
        "[{name}] `chunks` do not reassemble `serialized`",
    );

    let mut rx = HeaderReceiver::new(field_id, maxlen);
    let mut outcome = HeaderOutcome::Incomplete;
    for chunk in &chunks {
        outcome = rx.feed(chunk);
    }
    (rx, outcome)
}

/// Run one header-ceiling case; returns the number of checks it made.
#[cfg(feature = "fixlen")]
fn run_header_case(case: &Value) -> usize {
    let name = case["name"].as_str().expect("case name");
    let field_id = field_id_of_value(&case["field_id"]);
    let declared = case["declared"].as_u64().expect("declared") as usize;
    let expected = HeaderOutcome::parse(case["expect"]["outcome"].as_str().expect("outcome"));
    let terminal = case["expect"]["terminal"].as_bool().unwrap_or(false);

    let maxlen = match header_ceiling(case) {
        Ceiling::SchemaMaxlen(n) => n,
        Ceiling::ReceiverCap(k, _) => panic!(
            "[{name}] configures the receiver cap `{k}`, which this profile does not have — \
             the case should have been gated out by `receiver_caps`",
        ),
    };

    let (mut rx, outcome) = feed_header_case(case, field_id, maxlen);
    assert_eq!(outcome, expected, "[{name}] outcome");
    let mut checks = 1;

    // The ceiling can only answer at the word if the word was reported there:
    // the announced length is the number generated code judges, and the case's
    // `declared` is what it must be.
    assert_eq!(
        rx.field.announced,
        Some(declared),
        "[{name}] the length word declaring {declared} was not announced at the header",
    );
    checks += 1;

    if terminal {
        assert!(
            expected.is_rejection(),
            "[{name}] `terminal` is set on a non-rejection",
        );
        // A further feed re-raises rather than consuming: the payload the header
        // declared, arriving late, cannot lift a verdict already reached (§6.3,
        // §5.2.1).
        let before = rx.field.deliveries;
        let late_payload = vec![b'x'; declared];
        assert_eq!(
            rx.feed(&late_payload),
            expected,
            "[{name}] a further feed changed the verdict — the rejection is not terminal",
        );
        assert_eq!(
            rx.field.deliveries, before,
            "[{name}] a further feed was consumed after the rejection",
        );
        checks += 2;
    } else {
        // The in-ceiling control, and the half that keeps the block honest: not
        // only must these bytes answer INCOMPLETE, the state must really be the
        // one more bytes lift — its declared payload completes the message.
        assert_eq!(
            expected,
            HeaderOutcome::Incomplete,
            "[{name}] a non-terminal case that is not INCOMPLETE",
        );
        let payload = vec![b'x'; declared];
        assert_eq!(
            rx.feed(&payload),
            HeaderOutcome::Complete,
            "[{name}] the in-ceiling control did not complete once its payload arrived",
        );
        checks += 1;
    }
    checks
}

/// Every case in the block needs `fixlen` or `array`, and every `array` case
/// also needs `receiver_caps`, which this profile never declares — so a build
/// without `fixlen` runs none of them and this is never reached.
#[cfg(not(feature = "fixlen"))]
fn run_header_case(case: &Value) -> usize {
    unreachable!(
        "header-ceiling case `{}` ran in a build without `fixlen`",
        case["name"],
    )
}

#[test]
fn header_limits_block_is_well_formed() {
    // The block's own invariants, asserted before anything is run and
    // independently of what this build can execute: a case this port skips is
    // still a case whose shape it can check, and the README makes a missing
    // in-ceiling control a bug in the block rather than an omission to tolerate.
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let cases = doc["header_limits"]
        .as_array()
        .expect("the header_limits block");
    assert_eq!(cases.len(), 10, "the ten header-ceiling cases");

    // ceiling key -> (rejections, in-ceiling controls)
    let mut pairs: BTreeMap<&str, (usize, usize)> = BTreeMap::new();

    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let requires = parse_requires(case);
        let ceiling = header_ceiling(case);
        let outcome = HeaderOutcome::parse(case["expect"]["outcome"].as_str().expect("outcome"));
        let terminal = case["expect"].get("terminal").and_then(Value::as_bool);

        assert!(
            case.get("field_id").is_some() && case.get("declared").is_some(),
            "[{name}] a header-ceiling case names its field and its declared length",
        );
        assert!(
            !case["serialized"].as_str().expect("serialized").is_empty(),
            "[{name}] carries the header bytes",
        );

        // Which ceiling speaks decides the category — and which tag gates it.
        match ceiling {
            Ceiling::ReceiverCap(k, _) => {
                assert!(
                    requires.contains(&"receiver_caps"),
                    "[{name}] configures the cap `{k}` but is not gated by `receiver_caps`",
                );
                if outcome.is_rejection() {
                    assert_eq!(
                        outcome,
                        HeaderOutcome::LimitExceeded,
                        "[{name}] a cap breach is `limit_exceeded` (§6.2.1)",
                    );
                }
            }
            Ceiling::SchemaMaxlen(_) => {
                assert!(
                    !requires.contains(&"receiver_caps"),
                    "[{name}] a schema-bounded case needs no receiver cap",
                );
                if outcome.is_rejection() {
                    assert_eq!(
                        outcome,
                        HeaderOutcome::Invalid,
                        "[{name}] a schema-bound breach is `invalid` (MESSAGE_SPEC §7.1)",
                    );
                }
            }
        }

        // `terminal` marks the rejections, and only them: INCOMPLETE is
        // precisely the state more bytes can lift.
        assert_eq!(
            terminal,
            if outcome.is_rejection() {
                Some(true)
            } else {
                None
            },
            "[{name}] `expect.terminal` must be set on a rejection and absent otherwise",
        );

        let slot = pairs.entry(ceiling.key()).or_insert((0, 0));
        if outcome.is_rejection() {
            slot.0 += 1;
        } else {
            slot.1 += 1;
        }
    }

    // Every rejection is paired with the same shape at a length its ceiling
    // admits. Without the control the block proves nothing: a port that rejects
    // every short read passes all six rejection cases and is badly broken.
    for (key, (rejections, controls)) in &pairs {
        if *rejections > 0 {
            assert!(
                *controls > 0,
                "ceiling `{key}` has {rejections} rejection case(s) and no in-ceiling control",
            );
        }
    }
    println!("[vectors] header_limits: ceilings (rejections, controls) {pairs:?}");
}

#[test]
fn header_limits_cases_conform() {
    // Bytes that *declare* a length or count and then end. The ceiling is
    // decided at that word, before the payload is asked for, so the answer is
    // the ceiling's and it is terminal — never INCOMPLETE (§5.2.1, §6.2.1,
    // §6.3).
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let cases = doc["header_limits"]
        .as_array()
        .expect("the header_limits block");

    let mut ran: Vec<&str> = Vec::new();
    let mut gated: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut checks = 0;

    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let requires = parse_requires(case);
        if !header_case_supported(&requires) {
            // The skip is recorded and asserted below, not silent: a block that
            // is quietly ignored looks exactly like a block that passes.
            gated.push((name, requires));
            continue;
        }
        ran.push(name);
        checks += run_header_case(case);
    }

    println!(
        "[vectors] header_limits: {} cases ran, {} gated out by `requires`, {checks} checks",
        ran.len(),
        gated.len(),
    );
    println!("[vectors] header_limits: ran {ran:?}");
    for (name, requires) in &gated {
        let missing: Vec<&&str> = requires
            .iter()
            .filter(|r| !header_tag_satisfied(r))
            .collect();
        println!("[vectors] header_limits: skipped {name} — needs {missing:?}");
    }

    assert_eq!(
        ran.len() + gated.len(),
        cases.len(),
        "every case is either run or accounted for as a skip",
    );
    // A skip is only legitimate for a tag this build/profile really lacks.
    for (name, requires) in &gated {
        assert!(
            requires.iter().any(|r| !header_tag_satisfied(r)),
            "[{name}] was skipped although every tag it needs is satisfied",
        );
    }

    // What this profile owes, pinned exactly. `receiver_caps` is not declared
    // here — this profile refuses schema-unbounded fields at generate time, so
    // there is no §6.2.1 cap to exercise — but the schema-bounded pair needs no
    // cap and must run. It is also the pair that keeps the two categories apart:
    // `header_string_schema_bounded` carries the *identical* bytes to
    // `header_string_over_cap` and differs only in which ceiling is configured.
    #[cfg(feature = "fixlen")]
    {
        assert_eq!(
            ran,
            [
                "header_string_schema_bounded",
                "header_string_schema_bounded_in_bound"
            ],
            "the schema-bounded pair must run on this profile",
        );
        assert_eq!(gated.len(), 8, "the eight cap-bound cases are skipped");
        for (name, requires) in &gated {
            assert!(
                requires.contains(&"receiver_caps"),
                "[{name}] was skipped for something other than the undeclared \
                 `receiver_caps` — a wire construct this build lacks is a \
                 different kind of skip and should be named as such",
            );
        }
    }
    // Without `fixlen` there is no scalar length word to judge, and every
    // `array` case is cap-bound: the whole block gates out.
    #[cfg(not(feature = "fixlen"))]
    assert!(
        ran.is_empty(),
        "no header-ceiling case can run without `fixlen`",
    );
}

/// The negative control: with the ceiling lifted, the rejections fall back to
/// `INCOMPLETE`.
///
/// This is what shows the verdicts come from the ceiling and not from something
/// incidental — a decoder that rejected these bytes for its own reasons (a
/// length it dislikes, a truncated varint) would answer the same way with no
/// ceiling configured, and the block would be measuring nothing. It is also the
/// assertion that this corelib **enforces no limit of its own** (`error.rs`,
/// [`Error::LimitExceeded`]): schema bounds and receiver caps are the
/// consumer's, so with none configured the header word is merely the start of a
/// field whose payload has not arrived.
///
/// Gated on `fixlen` with the machinery it uses; a build without it runs no
/// header-ceiling case for there to be a control over.
#[cfg(feature = "fixlen")]
#[test]
fn header_limits_verdicts_come_from_the_ceiling() {
    let doc: Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let cases = doc["header_limits"].as_array().unwrap();

    let mut rejections = 0;
    let mut fell_back = 0;
    for case in cases {
        if !header_case_supported(&parse_requires(case)) {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let field_id = field_id_of_value(&case["field_id"]);
        let expected = HeaderOutcome::parse(case["expect"]["outcome"].as_str().unwrap());

        // `usize::MAX` is the ceiling lifted: no declared length can breach it.
        let (_, lifted) = feed_header_case(case, field_id, usize::MAX);
        if expected.is_rejection() {
            rejections += 1;
            assert_eq!(
                lifted,
                HeaderOutcome::Incomplete,
                "[{name}] rejected with no ceiling configured — the verdict does \
                 not come from the ceiling",
            );
            fell_back += 1;
        } else {
            assert_eq!(
                lifted, expected,
                "[{name}] an in-ceiling control changed when the ceiling was lifted",
            );
        }
    }
    println!(
        "[vectors] header_limits: ceilings lifted — {fell_back} of {rejections} rejection(s) \
         fell back to INCOMPLETE",
    );
    // On this profile exactly one rejection runs: the schema-bounded case. The
    // other five are cap-bound and gated out by `receiver_caps`.
    assert_eq!(
        rejections, 1,
        "expected the one schema-bounded rejection to run on this profile",
    );
}
