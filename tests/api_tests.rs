//! Tests for streaming/buffer-management API surface: reserve-offset,
//! buffer swapping, and large chunked payload streaming.

mod common;

use common::{Event, Recorder};
use sofab::{Error, IStream, OStream, MIN_OUTPUT_BUFFER};

// --- the buffer-installation contract (CORELIB_PLAN §5.1, §7.2 item 4) -------
//
// `MIN_OUTPUT_BUFFER` binds a buffer installed **with a flush sink**, at
// installation and at every mid-stream buffer-set, and binds nothing else. The
// tests below pin both halves of that, plus the offset range check that guards
// every installation path.

// §5.1: a declaration MUST NOT exceed 20 — a header varint and its value, the
// largest reservation any port makes and also the smallest message a schema can
// bound. Checked at compile time, since the value is a constant.
const _: () = assert!(
    MIN_OUTPUT_BUFFER >= 1 && MIN_OUTPUT_BUFFER <= 20,
    "MIN_OUTPUT_BUFFER must lie in 1..=20 (CORELIB_PLAN §5.1)"
);

#[test]
fn encode_at_exactly_min_output_buffer_matches_one_shot() {
    // §7.2 item 4: drive the sink through a buffer of exactly the declared
    // minimum and assert the concatenated output is byte-identical to the
    // one-shot bytes. The string is far longer than the buffer, so the
    // divisible-run split (§5.1) is exercised whatever the declared value is.
    const TEXT: &str = "a string payload much longer than the output buffer";

    let mut one_shot = [0u8; 128];
    let n = {
        let mut os = OStream::new(&mut one_shot);
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, TEXT).unwrap();
        os.write_signed(3, -7).unwrap();
        os.bytes_used()
    };

    let mut streamed: Vec<u8> = Vec::new();
    let mut window = [0u8; MIN_OUTPUT_BUFFER];
    {
        let mut os = OStream::with_flush(&mut window, 0, |c: &[u8]| streamed.extend_from_slice(c))
            .expect("a buffer of exactly MIN_OUTPUT_BUFFER must be accepted");
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, TEXT).unwrap();
        os.write_signed(3, -7).unwrap();
        os.flush();
    }
    assert_eq!(streamed, one_shot[..n]);
}

#[test]
fn a_sink_buffer_below_the_minimum_is_rejected_at_installation() {
    // One byte short of the minimum — for a port declaring 1, the zero-room
    // buffer. Rejected **there**, by the port's out-of-range mechanism, rather
    // than partway through a message.
    let mut sink = |_: &[u8]| {};

    let mut buf = [0u8; MIN_OUTPUT_BUFFER - 1];
    assert_eq!(
        OStream::with_flush(&mut buf, 0, &mut sink).err(),
        Some(Error::Argument),
    );

    // Same shortfall produced by the offset rather than the length.
    let mut buf = [0u8; 8];
    assert_eq!(
        OStream::with_flush(&mut buf, 8 - (MIN_OUTPUT_BUFFER - 1), &mut sink).err(),
        Some(Error::Argument),
    );

    // And at a mid-stream buffer-set, which is an installation like any other.
    let mut active = [0u8; 8];
    let mut short = [0u8; MIN_OUTPUT_BUFFER - 1];
    let mut os = OStream::with_flush(&mut active, 0, &mut sink).unwrap();
    assert_eq!(os.buffer_set(&mut short, 0).err(), Some(Error::Argument));
}

#[test]
fn the_same_undersized_buffer_without_a_sink_is_accepted() {
    // The converse §7.2 item 4 requires: the minimum is a *streaming* constant
    // and must not become a floor on the one-shot path. A message that fits
    // encodes into a buffer the sink path would have refused.
    let mut buf = [0u8; MIN_OUTPUT_BUFFER - 1];
    let os = OStream::with_offset(&mut buf, 0);
    assert!(os.is_ok(), "no sink means no minimum");

    // Exactness, at the smallest size that can hold anything: `{id 0: 0}` is two
    // bytes and encodes into a two-byte buffer.
    let mut exact = [0u8; 2];
    let used = {
        let mut os = OStream::new(&mut exact);
        os.write_unsigned(0, 0).unwrap();
        os.bytes_used()
    };
    assert_eq!((used, &exact[..]), (2, &[0x00, 0x00][..]));
}

#[test]
fn an_offset_past_the_end_is_rejected_on_every_installation_path() {
    // Left unchecked this is not merely a bad argument: the first write sees
    // `offset >= len`, flushes the whole stale buffer downstream as message
    // content and resumes at 0, prepending garbage to the message — and
    // `flush()` would slice past the buffer outright.
    let mut sink = |_: &[u8]| {};

    let mut buf = [0u8; 4];
    assert_eq!(
        OStream::with_offset(&mut buf, 5).err(),
        Some(Error::Argument)
    );

    let mut buf = [0u8; 4];
    assert_eq!(
        OStream::with_flush(&mut buf, 5, &mut sink).err(),
        Some(Error::Argument),
    );

    let mut active = [0u8; 8];
    let mut other = [0u8; 4];
    let mut os = OStream::with_flush(&mut active, 0, &mut sink).unwrap();
    assert_eq!(os.buffer_set(&mut other, 5).err(), Some(Error::Argument));

    // `offset == len` is *not* out of range: it is zero room, legal without a
    // sink, and simply reports buffer-full on the first write.
    let mut buf = [0u8; 4];
    let mut os = OStream::with_offset(&mut buf, 4).unwrap();
    assert_eq!(os.write_unsigned(0, 0), Err(Error::BufferFull));
}

#[test]
fn a_rejected_buffer_set_leaves_the_previous_buffer_installed() {
    // The swap must be all-or-nothing: a rejected buffer cannot strand the
    // encoder on storage it no longer has, nor lose bytes already written.
    let mut collected: Vec<u8> = Vec::new();
    let mut active = [0u8; 16];
    let mut bad = [0u8; MIN_OUTPUT_BUFFER - 1];
    {
        let mut os =
            OStream::with_flush(&mut active, 0, |c: &[u8]| collected.extend_from_slice(c)).unwrap();
        os.write_unsigned(1, 42).unwrap();
        assert_eq!(os.buffer_set(&mut bad, 0).err(), Some(Error::Argument));
        // The stream is untouched and keeps writing into the original buffer.
        os.write_unsigned(2, 7).unwrap();
        os.flush();
    }
    assert_eq!(collected, [0x08, 0x2A, 0x10, 0x07]);
}

#[test]
fn a_mid_stream_buffer_set_drains_the_pending_bytes_to_the_sink() {
    // §5.1: the bytes written since the last flush sit in the buffer that is
    // being replaced, and the caller does not get that buffer back — the swap
    // consumes it. With a sink installed they therefore have to reach the sink
    // at the swap, or the emitted message silently loses everything written
    // since the previous flush while every call still reports `Ok`.
    let mut one_shot = [0u8; 16];
    let n = {
        let mut os = OStream::new(&mut one_shot);
        os.write_unsigned(1, 42).unwrap();
        os.write_unsigned(2, 7).unwrap();
        os.bytes_used()
    };

    let mut collected: Vec<u8> = Vec::new();
    let mut a = [0u8; 16];
    let mut b = [0u8; 16];
    {
        let mut os =
            OStream::with_flush(&mut a, 0, |c: &[u8]| collected.extend_from_slice(c)).unwrap();
        os.write_unsigned(1, 42).unwrap();
        assert_eq!(os.bytes_used(), 2, "buffered, not yet flushed");
        os.buffer_set(&mut b, 0).unwrap();
        assert_eq!(os.bytes_used(), 0, "the swap drained what was pending");
        os.write_unsigned(2, 7).unwrap();
        os.flush();
    }
    assert_eq!(collected, one_shot[..n]);
}

#[test]
fn a_drained_buffer_set_resumes_at_its_own_offset() {
    // The drain happens **once**, at the swap, and the new installation's offset
    // is what the cursor resumes at (§5.1: the offset belongs to the
    // installation) — the packet pattern, where every flushed unit re-arms its
    // own framing-header room.
    let mut units: Vec<Vec<u8>> = Vec::new();
    let mut a = [0u8; 16];
    let mut b = [0xAAu8; 16]; // the reserved header room, prefilled by the caller
    {
        let mut os = OStream::with_flush(&mut a, 0, |c: &[u8]| units.push(c.to_vec())).unwrap();
        os.write_unsigned(1, 42).unwrap();
        os.buffer_set(&mut b, 3).unwrap();
        assert_eq!(os.bytes_used(), 3, "resumes at this call's offset");
        os.write_unsigned(2, 7).unwrap();
        os.flush();
    }
    assert_eq!(
        units.len(),
        2,
        "one unit per flush, and the swap is one of them"
    );
    assert_eq!(units[0], [0x08, 0x2A]); // drained at the swap, exactly once
    assert_eq!(units[1], [0xAA, 0xAA, 0xAA, 0x10, 0x07]); // header room + payload
}

#[test]
fn a_buffer_set_without_a_sink_still_leaves_the_bytes_with_the_caller() {
    // The converse: with no sink there is nothing to drain to, the caller still
    // owns the buffer it handed over, and the recovery path (install a bigger
    // buffer, retry the failed write) must keep working byte for byte.
    let mut small = [0u8; 2];
    let mut big = [0u8; 16];
    let (used_small, used_big) = {
        let mut os = OStream::new(&mut small);
        os.write_unsigned(1, 42).unwrap();
        assert_eq!(os.write_unsigned(2, 7), Err(Error::BufferFull));
        let used_small = os.bytes_used();
        os.buffer_set(&mut big, 0).unwrap();
        os.write_unsigned(2, 7).unwrap();
        (used_small, os.bytes_used())
    };
    let mut got = small[..used_small].to_vec();
    got.extend_from_slice(&big[..used_big]);
    assert_eq!(got, [0x08, 0x2A, 0x10, 0x07]);
}

// --- the returning-callback handover contract (§5.1, §7.2 item 4) -----------
//
// A sink either **copies** the bytes it was handed — returns without installing
// anything, and the encoder resumes in the same buffer at 0 — or **takes** the
// buffer, in which case it MUST install a replacement before returning. Both
// halves are tested here, against the same message, and both must produce
// exactly the one-shot bytes.
//
// The take-and-replace channel is opt-in and must stay free for the streams
// that do not use it: a footprint port cannot pay a pointer of encoder state
// per `OStream` for a capability a one-shot encode never reaches. `NoHandoff`
// being zero-sized is what makes `size_of::<OStream<NoFlush>>()` — the number
// `tools/footprint.sh` reports as encoder RAM — independent of it.
const _: () = assert!(
    core::mem::size_of::<sofab::NoHandoff>() == 0,
    "the default handoff must cost no encoder state"
);

#[test]
fn a_taking_sink_installs_a_replacement_buffer_from_inside_the_callback() {
    // §7.2 item 4, the zero-copy half: a flush callback that installs a
    // *different* buffer on every call and scrubs the one it was handed before
    // returning. An encoder that kept writing into the buffer it gave away
    // reads back the fill pattern; a port on which the callback cannot install
    // at all cannot express the DMA / packet hand-off §5.1 exists for.
    const TEXT: &str = "a string payload much longer than the output buffer";

    let mut one_shot = [0u8; 128];
    let n = {
        let mut os = OStream::new(&mut one_shot);
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, TEXT).unwrap();
        os.write_signed(3, -7).unwrap();
        os.bytes_used()
    };

    // A pool of distinct replacement buffers, each prefilled with a pattern
    // that is not part of the message.
    let mut storage: Vec<[u8; 4]> = vec![[0xAA; 4]; 64];
    let mut pool: Vec<&mut [u8]> = storage.iter_mut().map(|b| &mut b[..]).collect();
    let mut first = [0xAAu8; 4];

    let mut streamed: Vec<u8> = Vec::new();
    let mut handovers = 0usize;
    let mut reclaimed = 0usize;
    // Address range of the buffer installed at the previous handover: what the
    // encoder must be writing into now.
    let mut expected: Option<(usize, usize)> = None;
    let handover = sofab::Handover::new();
    {
        let mut os = OStream::with_handover(
            &mut first,
            0,
            |chunk: &[u8]| {
                if let Some((base, len)) = expected {
                    let at = chunk.as_ptr() as usize;
                    assert!(
                        at >= base && at + chunk.len() <= base + len,
                        "the encoder must write into the buffer the sink installed"
                    );
                }
                streamed.extend_from_slice(chunk);
                // Take the buffer: install a replacement before returning.
                let next = pool.pop().expect("pool exhausted");
                expected = Some((next.as_ptr() as usize, next.len()));
                handover
                    .install(next, 0)
                    .expect("a pool buffer is a legal installation");
                handovers += 1;
                // The buffer the encoder gave up at the previous handover is
                // ours now: scrub it, so an encoder that kept writing into a
                // buffer it handed away shows up in the emitted bytes.
                if let Some(old) = handover.taken() {
                    old.fill(0xAA);
                    reclaimed += 1;
                }
            },
            &handover,
        )
        .unwrap();
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, TEXT).unwrap();
        os.write_signed(3, -7).unwrap();
        os.flush();
    }

    assert!(handovers > 1, "the window must have flushed repeatedly");
    assert_eq!(
        reclaimed,
        handovers - 1,
        "every installation retires the buffer the sink took"
    );
    assert_eq!(streamed, one_shot[..n]);
}

#[test]
fn a_copying_sink_returns_without_installing_and_resumes_at_zero() {
    // The other half of the same contract: returning **without** installing
    // means the sink copied, the active buffer stays active, and the encoder
    // resumes writing into it at offset 0 — the same bytes, over a stream that
    // *could* have taken the buffer.
    let mut one_shot = [0u8; 64];
    let n = {
        let mut os = OStream::new(&mut one_shot);
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, "copied, not taken").unwrap();
        os.bytes_used()
    };

    let mut window = [0u8; 4];
    let mut streamed: Vec<u8> = Vec::new();
    let handover = sofab::Handover::new();
    {
        let mut os = OStream::with_handover(
            &mut window,
            0,
            |chunk: &[u8]| streamed.extend_from_slice(chunk),
            &handover,
        )
        .unwrap();
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, "copied, not taken").unwrap();
        os.flush();
    }
    assert_eq!(streamed, one_shot[..n]);
    assert!(
        handover.taken().is_none(),
        "nothing was taken, so nothing is retired"
    );
}

#[test]
fn every_installed_replacement_re_arms_its_own_header_room() {
    // §5.1: the start offset belongs to the **installation**, not to the
    // buffer, and that is how a taking sink gets framing-header room in *every*
    // flushed unit — one header per packet. Two buffers ping-pong through the
    // channel: the one the encoder gives up comes back at the next handover and
    // is installed again, at its own offset.
    const RESERVED: usize = 3;

    let mut one_shot = [0u8; 64];
    let n = {
        let mut os = OStream::new(&mut one_shot);
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, "two buffers, one header each").unwrap();
        os.bytes_used()
    };

    let mut a = [0xAAu8; 8];
    let mut b = [0xAAu8; 8];
    let mut pool: Vec<&mut [u8]> = vec![&mut b[..]];
    let mut units: Vec<Vec<u8>> = Vec::new();
    let handover = sofab::Handover::new();
    {
        let mut os = OStream::with_handover(
            &mut a,
            RESERVED,
            |packet: &[u8]| {
                units.push(packet.to_vec());
                // Recycle the buffer given up at the previous handover, then
                // take this one and install a replacement with fresh header
                // room.
                if let Some(done) = handover.taken() {
                    pool.push(done);
                }
                handover
                    .install(pool.pop().expect("ping-pong pool"), RESERVED)
                    .unwrap();
            },
            &handover,
        )
        .unwrap();
        os.write_unsigned(1, 300).unwrap();
        os.write_str(2, "two buffers, one header each").unwrap();
        os.flush();
    }

    assert!(units.len() > 2, "the window must have flushed repeatedly");
    let mut payload: Vec<u8> = Vec::new();
    for unit in &units {
        assert_eq!(
            &unit[..RESERVED],
            &[0xAA; RESERVED],
            "every unit carries its own untouched header room"
        );
        payload.extend_from_slice(&unit[RESERVED..]);
    }
    assert_eq!(payload, one_shot[..n]);
}

#[test]
fn a_replacement_below_the_minimum_is_rejected_where_it_is_handed_over() {
    // A buffer installed from inside the callback is an installation like any
    // other (§5.1): it is checked *there*, not partway through the message, and
    // a rejected one leaves the active buffer in place so the encode still
    // produces the one-shot bytes.
    let mut one_shot = [0u8; 32];
    let n = {
        let mut os = OStream::new(&mut one_shot);
        os.write_unsigned(1, 300).unwrap();
        os.write_unsigned(2, 7).unwrap();
        os.bytes_used()
    };

    let mut window = [0u8; 2];
    let mut too_small = [0u8; MIN_OUTPUT_BUFFER - 1];
    let mut undersized: Option<&mut [u8]> = Some(&mut too_small[..]);
    let mut streamed: Vec<u8> = Vec::new();
    let mut rejected = 0usize;
    let handover = sofab::Handover::new();
    {
        let mut os = OStream::with_handover(
            &mut window,
            0,
            |chunk: &[u8]| {
                streamed.extend_from_slice(chunk);
                if let Some(bad) = undersized.take() {
                    assert_eq!(handover.install(bad, 0).err(), Some(Error::Argument));
                    rejected += 1;
                }
            },
            &handover,
        )
        .unwrap();
        os.write_unsigned(1, 300).unwrap();
        os.write_unsigned(2, 7).unwrap();
        os.flush();
    }
    assert_eq!(rejected, 1);
    assert_eq!(streamed, one_shot[..n]);
}

#[test]
fn a_sink_never_receives_bytes_the_encoder_did_not_write() {
    // Regression: a stale buffer installed with a sink used to have its whole
    // previous content flushed downstream ahead of the message.
    let mut collected: Vec<u8> = Vec::new();
    let mut stale = [0xEEu8; 4]; // as any reused buffer would look
    {
        let mut os =
            OStream::with_flush(&mut stale, 0, |c: &[u8]| collected.extend_from_slice(c)).unwrap();
        os.write_unsigned(1, 42).unwrap();
        os.flush();
    }
    assert_eq!(collected, [0x08, 0x2A]);
}

#[test]
fn with_offset_reserves_header_space() {
    let mut buf = [0xAAu8; 16];
    let used = {
        let mut os = OStream::with_offset(&mut buf, 4).unwrap(); // reserve 4 header bytes
        os.write_unsigned(0, 42).unwrap();
        os.bytes_used()
    };
    assert_eq!(used, 6); // 4 reserved + 2 payload bytes
    assert_eq!(&buf[..4], &[0xAA, 0xAA, 0xAA, 0xAA]); // header space untouched
    assert_eq!(&buf[4..6], &[0x00, 0x2A]); // field id0 = 42
}

#[test]
fn buffer_set_switches_buffers() {
    let mut a = [0u8; 8];
    let mut b = [0u8; 8];
    let (used_a, used_b) = {
        let mut os = OStream::new(&mut a);
        os.write_unsigned(0, 1).unwrap();
        let ua = os.bytes_used();
        os.buffer_set(&mut b, 0).unwrap();
        os.write_unsigned(0, 2).unwrap();
        (ua, os.bytes_used())
    };
    assert_eq!((used_a, used_b), (2, 2));
    assert_eq!(&a[..2], &[0x00, 0x01]);
    assert_eq!(&b[..2], &[0x00, 0x02]);
}

#[test]
fn flush_without_sink_reports_pending_bytes() {
    let mut buf = [0u8; 8];
    let mut os = OStream::new(&mut buf);
    os.write_unsigned(0, 7).unwrap();
    // No sink: flush() reports the count but leaves the buffer in place.
    assert_eq!(os.flush(), 2);
    assert_eq!(os.bytes_used(), 2);
}

#[test]
fn large_blob_streams_in_small_chunks() {
    // 300-byte blob: larger than a typical MCU scratch buffer, exercising the
    // chunked string/blob delivery path across many feed() boundaries.
    let data: Vec<u8> = (0..300).map(|i| i as u8).collect();
    let mut buf = vec![0u8; 400];
    let used = {
        let mut os = OStream::new(&mut buf);
        os.write_blob(7, &data).unwrap();
        os.bytes_used()
    };

    let mut rec = Recorder::new();
    let mut is = IStream::new();
    for chunk in buf[..used].chunks(7) {
        // Mid-blob chunks report INCOMPLETE (§7); the final chunk completes it.
        match is.feed(chunk, &mut rec) {
            Ok(()) | Err(Error::Incomplete) => {}
            Err(e) => panic!("chunked blob decode: {e:?}"),
        }
    }
    assert_eq!(rec.events, [Event::Blob(7, data)]);
}

#[test]
fn default_constructors_work() {
    // Exercise IStream::default() and a manual NoFlush-typed stream.
    let mut buf = [0u8; 8];
    let mut os = OStream::new(&mut buf);
    os.write_boolean(1, false).unwrap();
    let used = os.bytes_used();

    let mut rec = Recorder::new();
    let mut is = IStream::default();
    is.feed(&buf[..used], &mut rec).unwrap();
    assert_eq!(rec.events, [Event::Unsigned(1, 0)]);
}

#[test]
fn api_version_is_one() {
    // Normative per the architecture spec: the library must expose version 1.
    assert_eq!(sofab::API_VERSION, 1);
}

#[test]
fn config_constants_reflect_features() {
    // The `config` module is the build-time introspection surface that
    // `require!` is built on; it must mirror how the crate was compiled. This
    // test runs under the default (all wire features, 64-bit) configuration.
    assert_eq!(
        [
            sofab::config::FIXLEN,
            sofab::config::ARRAY,
            sofab::config::SEQUENCE,
            sofab::config::FP64,
        ],
        [true, true, true, true]
    );
    assert_eq!(sofab::config::VALUE_BITS, 64);
    assert_eq!(sofab::config::VALUE_BITS, sofab::Unsigned::BITS);
}

// A satisfied `require!` must compile to nothing. (An unsatisfied one is a
// compile error, exercised in the macro's `compile_fail` doctest.)
sofab::require!(fixlen, array, sequence, fp64, value64);
