//! `ID_MAX` is the id range **this build** can actually use.
//!
//! The format ceiling is `INT32_MAX` (documentation §6.2), but the field header
//! is a varint of `(id << 3) | type` accumulated in [`sofab::Unsigned`], so a
//! `value64`-off build cannot carry an id whose top three bits fall off the
//! value type. `ID_MAX` is the lower of the two, and every guard in the crate
//! reads it — so this suite is width-agnostic on purpose: it compiles and runs
//! under `--no-default-features` exactly as under `--all-features`, and pins the
//! same properties at whichever ceiling the build has.

mod common;

use common::{feed, push_varint, Event};
use sofab::{Error, Id, OStream, Status, Unsigned, ID_MAX};

/// Encode with a fresh stack buffer and return the produced bytes.
fn encode<F: FnOnce(&mut OStream)>(f: F) -> Vec<u8> {
    let mut buf = [0u8; 64];
    let used = {
        let mut os = OStream::new(&mut buf);
        f(&mut os);
        os.bytes_used()
    };
    buf[..used].to_vec()
}

// The `as u64` widening is load-bearing on a 32-bit (`value64`-off) build, where
// `Unsigned` is `u32`, and a no-op on the 64-bit default — the same compile-time
// split the constant itself carries.
#[allow(clippy::unnecessary_cast)]
#[test]
fn id_max_is_the_lower_of_the_format_ceiling_and_the_width_bound() {
    let by_format = i32::MAX as u64;
    let by_width = (Unsigned::MAX >> 3) as u64;
    assert_eq!(ID_MAX as u64, by_format.min(by_width));

    // And the header of that id fits the value type with its 3 type bits — the
    // property the ceiling exists to guarantee.
    assert!((ID_MAX as u64) <= by_width);
}

#[test]
fn id_max_encodes_and_round_trips() {
    let bytes = encode(|os| os.write_unsigned(ID_MAX, 0).unwrap());
    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Ok(Status::Complete));
    assert_eq!(events, [Event::Unsigned(ID_MAX, 0)]);
}

#[test]
fn id_above_id_max_is_rejected_not_truncated() {
    let mut buf = [0u8; 16];
    let mut os = OStream::new(&mut buf);
    assert_eq!(os.write_unsigned(ID_MAX + 1, 0), Err(Error::Argument));
    // Nothing reached the wire: a truncated id would have been written here.
    assert_eq!(os.bytes_used(), 0);
}

#[cfg(feature = "sequence")]
#[test]
fn sequence_id_above_id_max_is_rejected() {
    let mut buf = [0u8; 16];
    let mut os = OStream::new(&mut buf);
    // The hold-back opener guards the id on the way *in*, because it is shifted
    // left by 3 later — in `commit_pending` — where a truncation would be silent.
    assert_eq!(
        os.write_sequence_begin_lazy(ID_MAX + 1),
        Err(Error::Argument)
    );
    assert_eq!(os.bytes_used(), 0);
}

#[test]
fn decoding_an_id_above_id_max_is_invalid() {
    // `(id << 3) | type`, type tag 0 = unsigned, then a one-byte value.
    let mut bytes = Vec::new();
    push_varint(&mut bytes, (ID_MAX as u64 + 1) << 3);
    bytes.push(0x00);

    let (outcome, _) = feed(&bytes);
    assert_eq!(outcome, Err(Error::InvalidMsg));
}

#[test]
fn decoding_exactly_id_max_is_valid() {
    let mut bytes = Vec::new();
    push_varint(&mut bytes, (ID_MAX as u64) << 3);
    bytes.push(0x00);

    let (outcome, events) = feed(&bytes);
    assert_eq!(outcome, Ok(Status::Complete));
    assert_eq!(events, [Event::Unsigned(ID_MAX as Id, 0)]);
}
