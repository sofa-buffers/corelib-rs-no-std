//! Shared test helpers: a recording [`Visitor`], the decode entry points built
//! on it, a tiny manual varint encoder for crafting malformed inputs, and a hex
//! reader for the shared vector file.
//!
//! Test vectors throughout the test suite are taken verbatim from the C
//! reference test suite (`test/c/test_ostream.c`).
//!
//! The feature-specific parts are `#[cfg]`-gated so this module — and the
//! `vectors_tests` suite that uses it — also compiles under reduced feature
//! sets (the vector file's `requires` tags drive which vectors actually run).

#![allow(dead_code)]

#[cfg(feature = "array")]
use sofab::ArrayKind;
use sofab::{Error, IStream, Id, Signed, Unsigned, Visitor};

/// One decoded event, recorded in order by [`Recorder`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Unsigned(Id, Unsigned),
    Signed(Id, Signed),
    /// Float stored as raw bits so comparisons are exact (incl. NaN payloads).
    #[cfg(feature = "fixlen")]
    Fp32(Id, u32),
    #[cfg(feature = "fp64")]
    Fp64(Id, u64),
    #[cfg(feature = "fixlen")]
    Str(Id, Vec<u8>),
    #[cfg(feature = "fixlen")]
    Blob(Id, Vec<u8>),
    #[cfg(feature = "array")]
    ArrayBegin(Id, ArrayKind, usize),
    #[cfg(feature = "sequence")]
    SequenceBegin(Id),
    #[cfg(feature = "sequence")]
    SequenceEnd,
}

/// A [`Visitor`] that records every decoded field as an [`Event`], reassembling
/// chunked string/blob payloads into whole buffers.
#[derive(Default)]
pub struct Recorder {
    pub events: Vec<Event>,
    // in-progress chunked string/blob accumulator: (id, is_blob, buffer)
    #[cfg(feature = "fixlen")]
    pending: Option<(Id, bool, Vec<u8>)>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(feature = "fixlen")]
    fn accumulate(&mut self, id: Id, is_blob: bool, total: usize, offset: usize, chunk: &[u8]) {
        if offset == 0 {
            self.pending = Some((id, is_blob, Vec::with_capacity(total)));
        }
        let done = {
            let (_, _, buf) = self.pending.as_mut().expect("chunk without begin");
            buf.extend_from_slice(chunk);
            buf.len() == total
        };
        if done {
            let (pid, pblob, buf) = self.pending.take().unwrap();
            self.events.push(if pblob {
                Event::Blob(pid, buf)
            } else {
                Event::Str(pid, buf)
            });
        }
    }
}

impl Visitor for Recorder {
    fn unsigned(&mut self, id: Id, value: Unsigned) {
        self.events.push(Event::Unsigned(id, value));
    }
    fn signed(&mut self, id: Id, value: Signed) {
        self.events.push(Event::Signed(id, value));
    }
    #[cfg(feature = "fixlen")]
    fn fp32(&mut self, id: Id, value: f32) {
        self.events.push(Event::Fp32(id, value.to_bits()));
    }
    #[cfg(feature = "fp64")]
    fn fp64(&mut self, id: Id, value: f64) {
        self.events.push(Event::Fp64(id, value.to_bits()));
    }
    #[cfg(feature = "fixlen")]
    fn string(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.accumulate(id, false, total, offset, chunk);
    }
    #[cfg(feature = "fixlen")]
    fn blob(&mut self, id: Id, total: usize, offset: usize, chunk: &[u8]) {
        self.accumulate(id, true, total, offset, chunk);
    }
    #[cfg(feature = "array")]
    fn array_begin(&mut self, id: Id, kind: ArrayKind, count: usize) {
        self.events.push(Event::ArrayBegin(id, kind, count));
    }
    #[cfg(feature = "sequence")]
    fn sequence_begin(&mut self, id: Id) {
        self.events.push(Event::SequenceBegin(id));
    }
    #[cfg(feature = "sequence")]
    fn sequence_end(&mut self) {
        self.events.push(Event::SequenceEnd);
    }
}

/// Feed `bytes` in one shot; return the three-valued outcome (§7) *and* every
/// event the visitor saw. Both halves matter to a suite asserting what was
/// announced before the bytes ran out.
pub fn feed(bytes: &[u8]) -> (Result<(), Error>, Vec<Event>) {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    let outcome = is.feed(bytes, &mut rec);
    (outcome, rec.events)
}

/// Decode `bytes` in one shot and return the recorded events, insisting the
/// outcome is `COMPLETE`.
pub fn decode(bytes: &[u8]) -> Vec<Event> {
    let (outcome, events) = feed(bytes);
    outcome.expect("decode failed");
    events
}

/// Decode `bytes` one byte per `feed` call and return the recorded events.
///
/// Chunks that end mid-field report INCOMPLETE (§7) — expected while streaming
/// byte-by-byte; only a genuine INVALID is a failure.
pub fn decode_one_byte_at_a_time(bytes: &[u8]) -> Vec<Event> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for &b in bytes {
        match is.feed(&[b], &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("chunked decode: {e:?}"),
        }
    }
    rec.events
}

/// Read a lowercase hex string (as the shared vector file stores wire bytes).
pub fn hex_to_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd hex length");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
        .collect()
}

/// Append a base-128 varint of `value` to `out` (for crafting raw test inputs).
pub fn push_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut b = (value as u8) & 0x7F;
        value >>= 7;
        if value != 0 {
            b |= 0x80;
        }
        out.push(b);
        if value == 0 {
            break;
        }
    }
}
