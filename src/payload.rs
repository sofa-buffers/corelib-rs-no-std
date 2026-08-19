//! Chunk reassembly for streamed `string` / `blob` payloads.
//!
//! This is the **generated-code support layer**. What lives here carries no
//! schema knowledge whatsoever — every bound arrives as an argument or as a
//! const parameter, exactly as [`crate::Visitor`]'s own callbacks take one — so
//! it has the same shape for every schema and is written once, here, instead of
//! being emitted into every crate `sofabgen` produces. Nothing in this module
//! touches the wire: it works purely on bytes the decoder has already handed
//! over.
//!
//! Storage stays where the rest of this port keeps it: in the caller. The
//! accumulator owns a `[u8; N]` whose size the caller chooses, so it allocates
//! nothing, links no allocator, and can sit in a `static`, in a decoder struct
//! or on the stack. That is the one substantive difference from the `std` twin
//! ([`corelib-rs`]'s `PayloadAcc`, which grows a `Vec`): storage here is finite,
//! so "the announced payload does not fit" is a real outcome and is reported as
//! [`Error::BufferFull`] rather than folded into "not complete yet".
//!
//! Like the rest of the crate it is written **panic-free**: the copy goes
//! through `get`/`get_mut` and `zip` rather than range indexing and
//! `copy_from_slice`, whose bounds and length asserts would link
//! `core::panicking` into an image that is meant not to have it (README
//! "Footprint"). It is also **zero-cost when unused**: `N` makes it a generic,
//! so a build that never names it monomorphises nothing at all.
//!
//! [`corelib-rs`]: https://github.com/sofa-buffers/corelib-rs

use crate::error::{Error, Result};

/// Reassembles a `string` or `blob` payload that arrives in more than one chunk.
///
/// [`crate::Visitor::string`] and [`crate::Visitor::blob`] deliver a payload as
/// `(total, offset, chunk)` — one call carrying the whole field when the bytes
/// happen to be contiguous in what was fed, and any number of contiguous pieces
/// once a transport has torn the field apart (CORELIB_PLAN §5.2). A consumer
/// that wants the field as one value — a `&str`, a byte array — has to put it
/// back together, and that is the same handful of lines for every field of every
/// schema.
///
/// `N` is the largest payload that can be *reassembled*. In generated code it
/// comes from the schema's widest `string`/`blob` bound — and that parameter is
/// the whole of the type's schema dependence, which is what lets one accumulator
/// serve every schema instead of one being emitted per crate.
///
/// [`PayloadAcc::feed`] hands the payload back exactly once, on the call that
/// completes it, and `Ok(None)` while bytes are still outstanding:
///
/// ```
/// use sofab::PayloadAcc;
///
/// let mut acc = PayloadAcc::<8>::new();
/// assert_eq!(acc.feed(5, 0, b"so"), Ok(None));                // more to come
/// assert_eq!(acc.feed(5, 2, b"fab"), Ok(Some(&b"sofab"[..]))); // complete
/// ```
///
/// The whole-payload case costs nothing: a chunk that already holds the field is
/// returned **borrowed from the input buffer**, with no copy into the
/// accumulator at all — and, because that path never touches the buffer, it
/// serves a payload far larger than `N`. A message decoded from one contiguous
/// slice takes it for every field.
///
/// One accumulator serves a whole message: a payload always starts at
/// `offset == 0`, which is where the previous one is dropped, so the buffer is
/// reused field after field and message after message.
///
/// # What it deliberately does not do
///
/// * **It does not validate.** The bytes come back raw, and the materialization
///   verdict stays with the caller — for a `string`, `core::str::from_utf8` on
///   what `feed` returned, whose `Err` is the `INVALID` decode outcome
///   (MESSAGE_SPEC §8, CORELIB_PLAN §6.4). That verdict belongs on the
///   *assembled* payload rather than on each chunk: a multi-byte sequence may
///   straddle a chunk boundary, so validating per chunk would reject valid text
///   and — worse — could accept a broken sequence whose halves each look
///   plausible. Feeding first and judging once is what makes the verdict
///   independent of where the chunk boundaries fell.
/// * **It does not judge a schema bound.** `total` is measured against `N`, the
///   storage it has, and nothing else. A `maxlen` is a separate, earlier
///   judgement — generated code latches that on [`crate::Visitor::fixlen_begin`]
///   or on the first chunk, before a byte is ever fed here, because an
///   over-length field is `INVALID` (MESSAGE_SPEC §7.1) whereas a payload too
///   large for *this* buffer is [`Error::BufferFull`], the same verdict a
///   fixed-capacity destination field yields when it overflows.
/// * **It does not act on an announced `total`.** `total` is decoded input: a
///   hostile message announces a gigabyte and then sends three bytes. Nothing
///   is reserved, cleared or moved on the strength of that number — only the
///   bytes that actually arrive are ever written — so an announcement that
///   never materializes costs the refusal and nothing else.
pub struct PayloadAcc<const N: usize> {
    /// Bytes of the current payload seen so far, `buf[..len]`. Untouched
    /// whenever the payload arrived whole in one chunk, which is the case that
    /// skips this buffer entirely.
    buf: [u8; N],
    /// How much of `buf` is live. Also the offset the next chunk of this payload
    /// must carry, which is what makes a mismatched chunk detectable.
    len: usize,
    /// Set once the current payload has been handed back, so a stray further
    /// chunk of it cannot yield a second (and then truncated) copy.
    complete: bool,
}

/// Reports the state, not the storage: dumping `N` bytes of buffer — most of it
/// stale — into a log is never what a caller wanted, and on this port the log
/// may well be a serial line.
impl<const N: usize> core::fmt::Debug for PayloadAcc<N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PayloadAcc")
            .field("buffered", &self.len)
            .field("capacity", &N)
            .field("complete", &self.complete)
            .finish()
    }
}

impl<const N: usize> Default for PayloadAcc<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> PayloadAcc<N> {
    /// Create an empty accumulator over `N` bytes of inline storage.
    ///
    /// `const`, so one can live in a `static` on bare metal.
    #[inline]
    pub const fn new() -> Self {
        PayloadAcc {
            buf: [0; N],
            len: 0,
            complete: false,
        }
    }

    /// Accept one payload chunk, as delivered to [`crate::Visitor::string`] /
    /// [`crate::Visitor::blob`], and return the whole payload once it is
    /// complete.
    ///
    /// * `Ok(Some(bytes))` — `bytes` is the payload, exactly `total` bytes long.
    ///   It borrows either from `chunk` (whole-payload case, no copy) or from
    ///   the accumulator; either way it is valid until the next call.
    /// * `Ok(None)` — bytes are still outstanding; feed the next chunk.
    /// * `Err(Error::BufferFull)` — the payload is split *and* longer than the
    ///   `N` bytes this accumulator holds, so it can never be assembled here.
    ///   Reported on the chunk that reveals it, before a byte is copied, and
    ///   again for every further chunk of the same payload — never as a silent
    ///   `Ok(None)` that would look like "still waiting" forever, and never as a
    ///   truncated value. The same payload arriving *contiguously* is returned
    ///   fine: the fast path hands back the caller's own bytes and needs no
    ///   storage at all.
    ///
    /// `offset` is read for two purposes. `offset == 0` marks the start of a
    /// payload and drops whatever the accumulator still held — that is what
    /// makes it self-healing: a field abandoned half way (a skipped payload, a
    /// bound that turned the message INVALID) leaves no bytes to contaminate the
    /// next one, without the caller having to reset anything. A later chunk must
    /// then continue exactly where the accumulator stands, `offset ==
    /// buffered()`; one that does not belongs to a payload this accumulator is
    /// not assembling, and is refused with `Ok(None)` rather than spliced into
    /// the middle of an unrelated field.
    ///
    /// A payload is handed back **once**: further chunks of one already
    /// completed return `Ok(None)` rather than a second, shorter copy. The
    /// decoder does not deliver such a chunk — a chunk with `offset >= total` is
    /// not something [`crate::IStream`] emits — but a consumer that stacks this
    /// on another source, or hands the same accumulator two payloads at once,
    /// gets a defined answer instead of a truncated field.
    ///
    /// ```
    /// use sofab::{Error, PayloadAcc};
    ///
    /// let mut acc = PayloadAcc::<8>::new();
    ///
    /// // Whole payload in one chunk: returned borrowed from `chunk` itself.
    /// assert_eq!(acc.feed(3, 0, b"abc"), Ok(Some(&b"abc"[..])));
    ///
    /// // A payload abandoned half way is dropped when the next one starts.
    /// assert_eq!(acc.feed(6, 0, b"lost"), Ok(None));
    /// assert_eq!(acc.feed(2, 0, b"o"), Ok(None));
    /// assert_eq!(acc.feed(2, 1, b"k"), Ok(Some(&b"ok"[..])));
    ///
    /// // Split, and longer than the eight bytes of storage: refused rather
    /// // than truncated. The same payload arriving whole is fine.
    /// assert_eq!(acc.feed(9, 0, b"too"), Err(Error::BufferFull));
    /// assert_eq!(acc.feed(9, 0, b"nine byte"), Ok(Some(&b"nine byte"[..])));
    /// ```
    pub fn feed<'a>(
        &'a mut self,
        total: usize,
        offset: usize,
        chunk: &'a [u8],
    ) -> Result<Option<&'a [u8]>> {
        if offset == 0 {
            self.len = 0;
            self.complete = false;
            if let Some(whole) = chunk.get(..total) {
                // The whole field is here. Hand back the input slice: building
                // the value from it directly is what saves the copy, and it is
                // the common case — a message fed as one slice never splits a
                // payload at all. It is also the only path that works for a
                // payload larger than `N`, so the check below deliberately sits
                // after it rather than before.
                self.complete = true;
                return Ok(Some(whole));
            }
        } else if self.complete {
            return Ok(None);
        }
        if total > N {
            // Announced longer than this buffer, and it is not arriving in one
            // piece: say so now, on the first chunk, rather than accumulate `N`
            // bytes and then have to admit the field cannot be finished. Ahead
            // of the continuity test below, so every chunk of such a payload
            // gets the same answer instead of the first one alone.
            return Err(Error::BufferFull);
        }
        if offset != self.len {
            // Not the next piece of what this accumulator holds — a payload it
            // is not assembling. Splicing it in would fabricate a field out of
            // two unrelated halves. (After a reset above, `len` is `0`, so a
            // first chunk always passes.)
            return Ok(None);
        }
        // Take only what the field still owes, so an over-delivering source can
        // neither widen the value nor push past the storage. `saturating_sub`
        // rather than `-`: a caller that shrinks `total` mid-payload must not be
        // able to make this arithmetic wrap.
        let want = total.saturating_sub(self.len);
        let mut copied = 0;
        if let Some(dst) = self.buf.get_mut(self.len..) {
            for (slot, &b) in dst.iter_mut().zip(chunk.iter().take(want)) {
                *slot = b;
                copied += 1;
            }
        }
        self.len += copied;
        if self.len < total {
            return Ok(None);
        }
        self.complete = true;
        // Cut at `total` rather than handing back everything buffered: what a
        // previous, longer payload left behind is not part of this field.
        match self.buf.get(..total) {
            Some(payload) => Ok(Some(payload)),
            // Unreachable — `total <= N` was established above — but stated as
            // an outcome rather than as an index, which would put a bounds
            // check (and `core::panicking`) into the image.
            None => Ok(None),
        }
    }

    /// Drop a partially accumulated payload.
    ///
    /// Rarely needed — the next payload's first chunk does the same — but it is
    /// how a consumer explicitly drops the tail of an abandoned message before
    /// reusing this accumulator. ([`crate::IStream`] has no counterpart: a
    /// decoder is restarted by replacing it, `IStream::default()`.) The buffer is
    /// not scrubbed; the bytes are unreachable, since the next chunk accepted is
    /// the one at offset `0`.
    #[inline]
    pub fn reset(&mut self) {
        self.len = 0;
        self.complete = false;
    }

    /// Number of payload bytes currently held, which is also the `offset` the
    /// next chunk of this payload must carry.
    ///
    /// Zero for a payload that arrived whole in one chunk (nothing was buffered)
    /// and zero between messages; non-zero exactly while a split payload is
    /// still incomplete.
    #[inline]
    pub const fn buffered(&self) -> usize {
        self.len
    }

    /// Largest payload this accumulator can reassemble from more than one
    /// chunk, i.e. `N`. A contiguous payload is not bound by it.
    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }
}
