//! Strict UTF-8 conformance for `string` fields (issue #85, MESSAGE_SPEC §8,
//! CORELIB_PLAN §6.4).
//!
//! Rust's `String`/`&str` is a **Unicode string type**, so it is *always
//! strict*: `SOFAB_STRICT_UTF8` is a no-op for this port (always ON) and there
//! is no primitive to expose. The division of responsibility per §6.4 is:
//!
//! * **Encode** is strict *by construction* — [`OStream::write_str`] takes
//!   `&str`, which the Rust type system already guarantees is valid UTF-8, so a
//!   `string` field can never carry invalid bytes. There is nothing to check and
//!   no way to construct a counter-example.
//! * **Decode** — the corelib delivers a `string` field's *raw bytes* to the
//!   [`Visitor::string`] callback and never builds a `str`/`String` itself.
//!   Strictness is enforced by **generated code**, which materializes the field
//!   with `core::str::from_utf8` (an `Err` becomes the sticky `inv` flag →
//!   `Error::InvalidMsg`, the `INVALID` decode outcome). This subsumes generator
//!   #80 and makes std and no_std agree.
//!
//! The `string` field requires the `fixlen` wire type, so the whole suite is
//! gated on that feature (it is on by default); a `--no-default-features` build
//! without `fixlen` compiles it away.

#![cfg(feature = "fixlen")]

mod common;

use common::{Event, Recorder};
use serde_json::Value;
use sofab::{Error, IStream, OStream};

/// The shared vectors, embedded from the verbatim asset copy.
const VECTORS_JSON: &str = include_str!("../assets/test_vectors.json");

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len() % 2 == 0, "odd hex length");
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex byte"))
        .collect()
}

/// The shared `invalid_utf8` negative vectors (tracks corelib-c-cpp#97).
fn invalid_utf8_vectors() -> Vec<Value> {
    let doc: Value = serde_json::from_str(VECTORS_JSON).expect("parse test_vectors.json");
    doc["invalid_utf8"]
        .as_array()
        .expect("invalid_utf8 array")
        .clone()
}

/// Decode `bytes` through the corelib (in one feed) and materialize every
/// `string` field with `core::str::from_utf8`, exactly as generated Rust code
/// does. Returns `Err` with the decode outcome when the frame is malformed
/// (corelib error) or a `string` payload is not valid UTF-8 (`Error::InvalidMsg`,
/// the `INVALID` outcome the generated `inv`-flag path reports).
fn decode_and_materialize(bytes: &[u8]) -> Result<Vec<Event>, Error> {
    let mut rec = Recorder::new();
    IStream::new().feed(bytes, &mut rec)?; // structural frame validity: corelib's job
    materialize(&rec.events)?;
    Ok(rec.events)
}

/// Same, but fed one byte at a time to prove chunk boundaries never change the
/// outcome (§6.4 cross-chunk semantics).
fn decode_and_materialize_chunked(bytes: &[u8]) -> Result<Vec<Event>, Error> {
    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for &b in bytes {
        match is.feed(&[b], &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => return Err(e),
        }
    }
    is.feed(&[], &mut rec)?; // clean boundary or Incomplete
    materialize(&rec.events)?;
    Ok(rec.events)
}

/// Generated-code materialization: `core::str::from_utf8(buf).map_err(|_| inv)?`.
fn materialize(events: &[Event]) -> Result<(), Error> {
    for e in events {
        if let Event::Str(_, buf) = e {
            core::str::from_utf8(buf).map_err(|_| Error::InvalidMsg)?;
        }
    }
    Ok(())
}

#[test]
fn invalid_utf8_group_present() {
    let vs = invalid_utf8_vectors();
    assert!(
        vs.len() >= 8,
        "expected the shared invalid_utf8 negative vectors (saw {})",
        vs.len()
    );
    for v in &vs {
        assert_eq!(v["group"], "invalid/utf8");
        assert_eq!(v["decode_outcome"], "invalid");
        assert_eq!(v["encode_outcome"], "invalid_argument");
    }
}

#[test]
fn invalid_utf8_vectors_decode_to_invalid() {
    for v in invalid_utf8_vectors() {
        let name = v["name"].as_str().unwrap();
        let bytes = hex_to_bytes(v["serialized_hex"].as_str().unwrap());

        assert_eq!(
            decode_and_materialize(&bytes),
            Err(Error::InvalidMsg),
            "[{name}] expected INVALID from from_utf8 materialization",
        );
        assert_eq!(
            decode_and_materialize_chunked(&bytes),
            Err(Error::InvalidMsg),
            "[{name}] chunked: expected INVALID",
        );
    }
}

#[test]
fn corelib_frame_itself_stays_valid() {
    // The corelib does NOT enforce UTF-8: for these structurally well-formed
    // frames its own decode succeeds and hands the raw bytes to the visitor —
    // strictness is the generated code's from_utf8, not the corelib's. Pins the
    // division of responsibility (§6.4): the decode side is corelib-untouched.
    for v in invalid_utf8_vectors() {
        let name = v["name"].as_str().unwrap();
        let bytes = hex_to_bytes(v["serialized_hex"].as_str().unwrap());
        let mut rec = Recorder::new();
        assert!(
            IStream::new().feed(&bytes, &mut rec).is_ok(),
            "[{name}] corelib frame should be structurally valid",
        );
        let want = hex_to_bytes(v["string_hex"].as_str().unwrap());
        match rec.events.as_slice() {
            [Event::Str(0, got)] => assert_eq!(*got, want, "[{name}] raw bytes"),
            other => panic!("[{name}] unexpected events {other:?}"),
        }
    }
}

#[test]
fn from_utf8_rejects_each_invalid_form() {
    for v in invalid_utf8_vectors() {
        let name = v["name"].as_str().unwrap();
        let raw = hex_to_bytes(v["string_hex"].as_str().unwrap());
        assert!(
            core::str::from_utf8(&raw).is_err(),
            "[{name}] from_utf8 must reject",
        );
    }
}

#[test]
fn embedded_nul_roundtrips() {
    // U+0000 is valid UTF-8; a `string` carrying an embedded NUL must round-trip
    // byte-exact — never rejected, never truncated (§8, §6.4). The overlong
    // C0 80 form is a different thing and stays rejected.
    let original = "a\u{0}b\u{0}"; // interior + trailing NUL
    let mut buf = [0u8; 32];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_str(9, original).unwrap();
        os.bytes_used()
    };

    let events = decode_and_materialize(&buf[..used]).expect("valid UTF-8 with NUL");
    match events.as_slice() {
        [Event::Str(9, bytes)] => {
            assert_eq!(bytes.as_slice(), original.as_bytes());
            assert_eq!(core::str::from_utf8(bytes).unwrap(), original);
        }
        other => panic!("unexpected events {other:?}"),
    }

    // The overlong NUL (C0 80) is a different thing and stays rejected.
    assert!(core::str::from_utf8(&hex_to_bytes("c080")).is_err());
}
