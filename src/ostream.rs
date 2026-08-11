//! Streaming output stream encoder (port of `ostream.c`).
//!
//! [`OStream`] writes Sofab fields into a caller-owned byte buffer. When the
//! buffer fills it hands the bytes to an optional [`Flush`] sink and resumes at
//! the start of the buffer, so messages larger than the buffer (or larger than
//! RAM) can be streamed out. With no sink, a full buffer yields
//! [`Error::BufferFull`].

use core::cell::Cell;

use crate::error::{Error, Result};
use crate::types::*;
use crate::varint::zigzag_encode;
use crate::{Id, Signed, Unsigned};

/// Sink that receives buffered bytes when the output buffer is flushed.
///
/// Any `FnMut(&[u8])` closure implements this trait, so callbacks can be passed
/// directly. Implement it manually to avoid a closure capture on bare-metal.
///
/// A sink either **copies** the bytes it was handed or **takes** the buffer
/// (queues it for a transport, hands it to DMA), and the encoder cannot tell the
/// two apart — the contract rests on what the callback does before it returns
/// (CORELIB_PLAN §5.1):
///
/// * returning **without** installing a buffer means it copied: the active
///   buffer stays active and the encoder resumes writing into it at offset `0`;
/// * a sink that **takes** the buffer **MUST** install a replacement before
///   returning. That is what [`Handover`] is for — a stream built with
///   [`OStream::with_handover`] passes the channel to the sink, which calls
///   [`Handover::install`] from inside the callback and picks the buffer it took
///   back up with [`Handover::taken`].
pub trait Flush {
    /// Whether this sink actually drains the buffer. `true` for a real sink (a
    /// full buffer flushes and writing resumes); [`NoFlush`] overrides it to
    /// `false` so a full buffer is [`Error::BufferFull`] instead. It is a
    /// compile-time constant, so a [`NoFlush`] encoder monomorphizes the
    /// flush branch out of `push_byte` entirely (no field, no dead code).
    const SINKS: bool = true;

    /// Consume `data` (e.g. push to a transport or storage). Called with the
    /// bytes accumulated since the last flush.
    fn flush(&mut self, data: &[u8]);
}

impl<T: FnMut(&[u8])> Flush for T {
    #[inline]
    fn flush(&mut self, data: &[u8]) {
        self(data)
    }
}

/// A [`Flush`] sink that does nothing. Used as the default when the stream is
/// constructed without a sink; a full buffer then returns [`Error::BufferFull`].
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFlush;

impl Flush for NoFlush {
    const SINKS: bool = false;
    #[inline]
    fn flush(&mut self, _data: &[u8]) {}
}

/// Where the encoder looks for a replacement buffer once a [`Flush`] callback
/// has returned, and where it drops the buffer that callback took.
///
/// The two implementations are the whole story: [`NoHandoff`] — the default,
/// zero-sized, `TAKES == false`, so a stream that cannot be handed a replacement
/// compiles the take-and-replace path away entirely and keeps
/// `size_of::<OStream>()` exactly what it was — and `&`[`Handover`], the channel
/// a taking sink writes into. It is a trait rather than a plain field so the
/// choice costs nothing when it is not used, which is what a footprint profile
/// needs: an unconditional `Option<&Handover>` field would add a pointer to
/// **every** encoder, including the one-shot `NoFlush` one that can never flush.
pub trait Handoff<'a> {
    /// Whether a sink on this stream can install a replacement buffer.
    /// A compile-time constant, so `false` folds the whole handover path out of
    /// the flush path (no field access, no branch, no dead code).
    const TAKES: bool = true;

    /// The buffer a sink installed during the callback that just returned, with
    /// the offset writing resumes at. `None` means the sink copied.
    fn installed(&self) -> Option<(&'a mut [u8], usize)>;

    /// Hand the buffer the sink took back to it — the encoder has stopped
    /// writing into it and gives up its borrow here.
    fn retire(&self, buffer: &'a mut [u8]);
}

/// The [`Handoff`] of a stream whose sink cannot install a replacement buffer:
/// zero-sized, and every flush resumes at `0` in the active buffer.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHandoff;

impl<'a> Handoff<'a> for NoHandoff {
    const TAKES: bool = false;
    #[inline]
    fn installed(&self) -> Option<(&'a mut [u8], usize)> {
        None
    }
    #[inline]
    fn retire(&self, _buffer: &'a mut [u8]) {}
}

/// The take-and-replace channel between a [`Flush`] sink and its encoder
/// (CORELIB_PLAN §5.1).
///
/// A sink that **takes** the buffer it was handed — passes it to a transport,
/// queues it for an asynchronous write, hands it to DMA — **MUST** install a
/// replacement before returning, or the encoder would keep writing into storage
/// the transport now owns. Inside the callback the encoder is mutably borrowed
/// by the very call that invoked the sink, so [`OStream::buffer_set`] is out of
/// reach; the sink installs through this channel instead, which the caller
/// creates, hands to [`OStream::with_handover`], and shares with the sink:
///
/// ```
/// use sofab::{Handover, OStream};
///
/// let mut first = [0u8; 32];
/// let mut spare = [0u8; 32];
/// let mut pool: Vec<&mut [u8]> = vec![&mut spare];
/// let mut packets: Vec<Vec<u8>> = Vec::new();
/// let handover = Handover::new();
/// {
///     let mut os = OStream::with_handover(
///         &mut first,
///         4, // framing-header room, re-armed by every installation
///         |packet: &[u8]| {
///             packets.push(packet.to_vec()); // or: start the DMA on it
///             // Took it: install a replacement before returning.
///             handover.install(pool.pop().expect("pool"), 4).unwrap();
///             // The buffer taken at the previous handover is ours again.
///             if let Some(done) = handover.taken() {
///                 pool.push(done);
///             }
///         },
///         &handover,
///     )
///     .unwrap();
///     os.write_unsigned(1, 42).unwrap();
///     os.flush();
/// }
/// ```
///
/// **What the encoder does with an installation.** After the callback returns,
/// the buffer it installed becomes the active one and writing resumes at *that
/// call's* offset — the offset belongs to the installation, so this is also how
/// a sink re-arms framing-header room in every flushed unit. The buffer it
/// replaced is handed back through [`Handover::taken`], which is what lets a
/// pool recycle it: until the encoder gives up that borrow, nobody else can
/// touch the storage. Reclaim it in every callback — the channel holds one
/// retired buffer, and retiring another drops the first (only the borrow ends;
/// the storage itself is untouched and returns to its owner when the stream
/// does).
///
/// A callback that installs nothing leaves the channel empty, which is the
/// copy-and-continue shape: same buffer, resume at `0`.
#[derive(Default)]
pub struct Handover<'a> {
    /// The replacement a sink installed during the current callback.
    next: Cell<Option<(&'a mut [u8], usize)>>,
    /// The buffer the encoder stopped writing into, waiting to be reclaimed.
    retired: Cell<Option<&'a mut [u8]>>,
}

impl<'a> Handover<'a> {
    /// Create an empty channel.
    ///
    /// Not a `const fn`: the slots hold `&mut [u8]`, and a mutable reference in
    /// a constant function is stable only from Rust 1.83, well past this
    /// crate's MSRV. It compiles to two null pointers either way — and a
    /// `Handover` is a stack value that lives beside the encoder, not a
    /// `static`, since its buffers are borrowed rather than owned.
    #[inline]
    pub fn new() -> Self {
        Handover {
            next: Cell::new(None),
            retired: Cell::new(None),
        }
    }

    /// Install `buffer` as the encoder's next output buffer, resuming writes at
    /// `offset` in it. Call this from inside a [`Flush`] callback that took the
    /// buffer it was handed.
    ///
    /// Checked exactly as every other installation is, and **where it is handed
    /// over** rather than partway through a message (§5.1): [`Error::Argument`]
    /// if `offset` lies past the end of `buffer`, or if the remaining room
    /// `buffer.len() - offset` is below [`MIN_OUTPUT_BUFFER`] — a channel only
    /// exists on a stream that has a sink, so the streaming minimum always
    /// applies. A rejected buffer is **not** installed and the channel keeps
    /// whatever it held, so the encoder simply carries on in the active buffer;
    /// the sink learns of the rejection here, in time to install another one.
    ///
    /// Installing twice in one callback keeps the **last** buffer.
    pub fn install(&self, buffer: &'a mut [u8], offset: usize) -> Result<()> {
        check_install(buffer.len(), offset, true)?;
        self.next.set(Some((buffer, offset)));
        Ok(())
    }

    /// Take back the buffer the encoder stopped writing into after this sink
    /// installed a replacement — the buffer this sink took, now free to be
    /// recycled, scrubbed or re-queued. `None` if nothing was retired since the
    /// last call.
    #[inline]
    pub fn taken(&self) -> Option<&'a mut [u8]> {
        self.retired.take()
    }
}

/// Written by hand rather than derived: the slots hold `&mut [u8]`, which a
/// `Cell` cannot lend out for printing, and the buffers are not this type's
/// business anyway — what a caller wants to see is whether the channel is armed.
impl core::fmt::Debug for Handover<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Take-and-put-back: a `Cell` has no shared read, and both slots are
        // restored before this returns, so the channel is left as it was found.
        let next = self.next.take();
        let armed = next.is_some();
        self.next.set(next);
        let retired = self.retired.take();
        let pending = retired.is_some();
        self.retired.set(retired);
        f.debug_struct("Handover")
            .field("installed", &armed)
            .field("retired", &pending)
            .finish()
    }
}

impl<'a> Handoff<'a> for &'a Handover<'a> {
    #[inline]
    fn installed(&self) -> Option<(&'a mut [u8], usize)> {
        self.next.take()
    }
    #[inline]
    fn retire(&self, buffer: &'a mut [u8]) {
        self.retired.set(Some(buffer));
    }
}

/// How many nested sequence headers can be held back at once (see
/// [`OStream::write_sequence_begin_lazy`]). A run deeper than this is framed
/// eagerly: still valid, just not canonical — an all-default sequence nested
/// deeper keeps its empty frame, which a decoder accepts and normalizes away
/// (MESSAGE_SPEC §2). Deliberately far below the format's [`MAX_DEPTH`] ceiling:
/// the array costs `4 * LAZY_SEQ_DEPTH` bytes of encoder state, and a heap-free
/// target pays that in RAM — measured on Cortex-M0, the `OStream` grows from
/// 16 B to 52 B at 8.
///
/// It is **fixed at 8 for every build of this crate**: there is no Cargo feature,
/// no `cfg` and no environment variable that changes it, so a target that cannot
/// spare the 36 B has to edit this constant in a patched or vendored copy of the
/// crate. A schema nesting deeper than the window still encodes correctly either
/// way; it just keeps the empty frame of the sequences beyond it.
///
/// This bound is the **heap-free profile allowance** of CORELIB_PLAN §6 ("How
/// deep the hold-back reaches"): an implementation that can allocate MUST hold
/// back to the full [`MAX_DEPTH`] and is canonical at every depth; a heap-free
/// one MAY bound the run, and **MUST document the bound**, because two encoders
/// that disagree about it disagree about bytes — not about validity. That
/// documentation, with the measured RAM cost next to it, is the README's
/// "Sequence framing" section; keep the two in sync when changing this value.
#[cfg(feature = "sequence")]
pub const LAZY_SEQ_DEPTH: usize = 8;

/// Smallest output buffer this port accepts **for streaming** — a buffer
/// installed together with a [`Flush`] sink (CORELIB_PLAN §5.1).
///
/// **This port declares `1`**, the strictest value the spec allows and the one
/// it calls "the right choice for a footprint profile". Every byte this encoder
/// produces goes through a single-byte push primitive that flushes and resumes
/// on its own, so **no atomic unit has to land contiguously** — not a field
/// header, not a `fixlen_word`, not a float element. A caller can therefore
/// stream a message of any size through a one-byte window, and the bytes are
/// identical to the one-shot encoding (asserted in `tests/api_tests.rs`, over a
/// payload far longer than the window).
///
/// The constant binds a buffer handed to [`OStream::with_flush`] or
/// [`OStream::buffer_set`]: such a buffer **MUST** satisfy
/// `buffer.len() - offset >= MIN_OUTPUT_BUFFER` and is rejected with
/// [`Error::Argument`] **where it is handed over**, never partway through a
/// message.
///
/// **A buffer installed without a sink is subject to no minimum.** No flush can
/// occur, so nothing can be split and the constant has nothing to say: the
/// buffer either holds the message or reports [`Error::BufferFull`]. That is the
/// case a caller sizes from a generated `MAX_SIZE`, and it stays exact — a
/// message that encodes to two bytes encodes into a two-byte buffer.
pub const MIN_OUTPUT_BUFFER: usize = 1;

/// Validate a buffer/offset pair at the point it is handed to the encoder
/// (CORELIB_PLAN §5.1).
///
/// Two independent conditions, both reported as [`Error::Argument`]:
///
/// * **the offset is in range** — `offset > buffer.len()` is an out-of-range
///   offset on *every* installation path, sink or not. Left unchecked it is not
///   merely a bad argument: the first write sees `offset >= len`, flushes the
///   whole stale buffer downstream as if those bytes were message content, and
///   resumes at 0 — silently prepending garbage to the message;
/// * **the streaming minimum** — only when a sink is installed, per §5.1.
#[inline]
fn check_install(len: usize, offset: usize, sinks: bool) -> Result<()> {
    let room = match len.checked_sub(offset) {
        Some(room) => room,
        None => return Err(Error::Argument),
    };
    if sinks && room < MIN_OUTPUT_BUFFER {
        return Err(Error::Argument);
    }
    Ok(())
}

/// Streaming Sofab encoder writing into a caller-provided buffer.
pub struct OStream<'a, F: Flush = NoFlush, H: Handoff<'a> = NoHandoff> {
    buffer: &'a mut [u8],
    offset: usize,
    /// The flush sink. Whether it drains a full buffer or errors is decided at
    /// compile time by [`Flush::SINKS`]; [`NoFlush`] is zero-sized and folds the
    /// flush branch away, so a no-sink encoder carries no runtime sink state.
    flush: F,
    /// Where a sink hands a replacement buffer back (§5.1 take-and-replace).
    /// [`NoHandoff`] is zero-sized with `TAKES == false`, so a stream without a
    /// [`Handover`] carries no state for it and no branch either.
    handoff: H,
    /// Currently-open nested-sequence depth, capped at [`MAX_DEPTH`].
    #[cfg(feature = "sequence")]
    depth: u32,
    /// Ids of the innermost open sequences whose header has not been written yet
    /// (MESSAGE_SPEC §2 lazy framing). Always a contiguous suffix of the open
    /// sequences: writing any field commits the whole run at once.
    #[cfg(feature = "sequence")]
    pending: [Id; LAZY_SEQ_DEPTH],
    /// Number of valid entries in [`Self::pending`].
    #[cfg(feature = "sequence")]
    npending: usize,
}

impl<'a> OStream<'a, NoFlush> {
    /// Create an encoder over `buffer` with no flush sink. Writing past the end
    /// of the buffer returns [`Error::BufferFull`].
    ///
    /// Infallible: the cursor starts at `0`, which is in range for every buffer
    /// including an empty one, and no sink means no [`MIN_OUTPUT_BUFFER`]
    /// (§5.1). Use [`OStream::with_offset`] to reserve header room.
    #[inline]
    pub fn new(buffer: &'a mut [u8]) -> Self {
        OStream::install(buffer, 0, NoFlush, NoHandoff)
    }

    /// Like [`OStream::new`] but begin writing at `offset` bytes into the
    /// buffer, reserving space for a lower-layer protocol header.
    ///
    /// Returns [`Error::Argument`] if `offset` lies past the end of `buffer`.
    /// No minimum applies — a buffer installed without a sink can be any size
    /// (§5.1), so `offset == buffer.len()` is accepted and simply leaves no room
    /// (the first write reports [`Error::BufferFull`]).
    #[inline]
    pub fn with_offset(buffer: &'a mut [u8], offset: usize) -> Result<Self> {
        check_install(buffer.len(), offset, NoFlush::SINKS)?;
        Ok(OStream::install(buffer, offset, NoFlush, NoHandoff))
    }
}

impl<'a, F: Flush> OStream<'a, F, NoHandoff> {
    /// Create an encoder with a flush `sink`, starting at `offset`. When the
    /// buffer fills, the accumulated bytes are passed to `sink` and writing
    /// resumes at the start of the buffer.
    ///
    /// The buffer is checked **here**, where it is handed over, rather than
    /// partway through a message (§5.1): [`Error::Argument`] if `offset` lies
    /// past the end of `buffer`, or if the remaining room
    /// `buffer.len() - offset` is below [`MIN_OUTPUT_BUFFER`].
    ///
    /// The sink of such a stream **copies**: it has no channel to install a
    /// replacement buffer through, so every flush resumes at `0` in the same
    /// buffer. A sink that wants to *take* the buffer builds the stream with
    /// [`OStream::with_handover`] instead.
    #[inline]
    pub fn with_flush(buffer: &'a mut [u8], offset: usize, sink: F) -> Result<Self> {
        check_install(buffer.len(), offset, F::SINKS)?;
        Ok(OStream::install(buffer, offset, sink, NoHandoff))
    }
}

impl<'a, F: Flush> OStream<'a, F, &'a Handover<'a>> {
    /// Create an encoder with a flush `sink` that may **take** the buffer it is
    /// handed, installing a replacement through `handover` before it returns
    /// (§5.1, the take-and-replace half of the returning-callback contract).
    ///
    /// `buffer` and `offset` are checked exactly as in [`OStream::with_flush`],
    /// and so is every buffer the sink later installs. The sink and this stream
    /// share the same [`Handover`]: pass `&handover` to both.
    ///
    /// A callback that installs nothing still means "I copied" — a handover
    /// stream is a superset of a copying one, and costs one pointer of encoder
    /// state for the channel.
    #[inline]
    pub fn with_handover(
        buffer: &'a mut [u8],
        offset: usize,
        sink: F,
        handover: &'a Handover<'a>,
    ) -> Result<Self> {
        check_install(buffer.len(), offset, F::SINKS)?;
        Ok(OStream::install(buffer, offset, sink, handover))
    }
}

impl<'a, F: Flush, H: Handoff<'a>> OStream<'a, F, H> {
    /// Build the stream once the buffer/offset pair has been accepted. Private,
    /// so the checks above are the only way in.
    #[inline]
    fn install(buffer: &'a mut [u8], offset: usize, flush: F, handoff: H) -> Self {
        OStream {
            buffer,
            offset,
            flush,
            handoff,
            #[cfg(feature = "sequence")]
            depth: 0,
            #[cfg(feature = "sequence")]
            pending: [0; LAZY_SEQ_DEPTH],
            #[cfg(feature = "sequence")]
            npending: 0,
        }
    }

    /// Number of bytes written to the active buffer since the last flush.
    #[inline]
    pub fn bytes_used(&self) -> usize {
        self.offset
    }

    /// Flush any pending bytes to the sink (if one is set) and report how many
    /// bytes were pending. With no sink the buffer is left intact.
    pub fn flush(&mut self) -> usize {
        let used = self.offset;
        // `F::SINKS` is a compile-time constant: for a `NoFlush` encoder the
        // whole body folds away and this is just `self.offset`.
        if used > 0 && F::SINKS {
            self.drain();
        }
        used
    }

    /// Hand the buffered bytes to the sink and re-establish where writing
    /// continues — the one place the returning-callback contract of §5.1 is
    /// implemented, shared by [`Self::flush`] and [`Self::push_byte`].
    ///
    /// The callback decides which of the two shapes is in effect, and the
    /// encoder does not guess:
    ///
    /// * it installed nothing → it **copied**. The active buffer stays active
    ///   and writing resumes at offset `0`;
    /// * it installed a replacement → it **took** the buffer. The replacement
    ///   becomes active at *its* offset (which is how each flushed unit re-arms
    ///   its own framing-header room), and the buffer it took is handed back
    ///   through the channel so the sink can recycle it — the encoder gives up
    ///   its borrow there and never writes into it again.
    ///
    /// [`Handoff::TAKES`] is a compile-time constant, so on the default
    /// [`NoHandoff`] stream everything after the callback folds down to
    /// `self.offset = 0`.
    ///
    /// Only ever called with a sink installed (`F::SINKS`).
    #[inline]
    fn drain(&mut self) {
        // `min` proves the slice end in-bounds. `check_install` already
        // guarantees `offset <= len` on every installation path and `push_byte`
        // only advances the cursor into a slot it obtained, so the clamp never
        // fires — spelling it this way is what keeps `core::panicking` out of
        // the image (README "Footprint") rather than resting the no-panic
        // property on that reasoning.
        let end = self.offset.min(self.buffer.len());
        self.flush.flush(&self.buffer[..end]);
        self.offset = 0;
        if H::TAKES {
            if let Some((next, offset)) = self.handoff.installed() {
                // `check_install` ran inside `Handover::install`, where the
                // buffer was handed over — never partway through a message.
                let taken = core::mem::replace(&mut self.buffer, next);
                self.handoff.retire(taken);
                self.offset = offset;
            }
        }
    }

    /// Replace the active buffer, resuming writes at `offset` in the new buffer.
    ///
    /// Checked exactly as the installing constructors are, because it *is* an
    /// installation (§5.1): [`Error::Argument`] if `offset` lies past the end of
    /// `buffer`, or — when this stream has a flush sink — if the remaining room
    /// is below [`MIN_OUTPUT_BUFFER`]. On an error the previous buffer stays
    /// installed untouched, so a rejected swap cannot strand the encoder.
    ///
    /// The start offset belongs to the **installation**, not to the buffer: this
    /// call's `offset` is consumed once, and any later flush that returns
    /// without a new installation resumes at `0`. Passing the *same* buffer is a
    /// new installation like any other — that is how a caller re-arms header
    /// room for the next unit.
    ///
    /// **With a sink installed, the outgoing buffer is drained first.** The bytes
    /// written since the last flush live in the buffer being replaced, and the
    /// swap consumes that buffer, so they are handed to the sink here: a
    /// mid-stream swap keeps the emitted stream byte-identical to the one-shot
    /// encoding instead of dropping everything buffered since the last flush.
    /// [`Flush::SINKS`] is a compile-time constant, so a [`NoFlush`] encoder
    /// folds this away entirely — with no sink there is nothing to drain to, the
    /// caller still owns the buffer it handed over, and the documented
    /// [`Error::BufferFull`] recovery (install a bigger buffer, retry the failed
    /// write) is unchanged.
    ///
    /// On a stream with a [`Handover`] the drain is an ordinary flush, so the
    /// sink may take the outgoing buffer and install a replacement from inside
    /// it — that buffer is then **superseded by this call's**, which is the last
    /// installation and wins. Drive the swap from one side or the other, not
    /// both at once.
    #[inline]
    pub fn buffer_set(&mut self, buffer: &'a mut [u8], offset: usize) -> Result<()> {
        check_install(buffer.len(), offset, F::SINKS)?;
        // After the check, never before it: a rejected swap must leave the
        // encoder exactly as it was, buffered bytes included.
        if F::SINKS {
            self.flush();
        }
        self.buffer = buffer;
        self.offset = offset;
        Ok(())
    }

    // --- primitives ---------------------------------------------------------

    fn push_byte(&mut self, b: u8) -> Result<()> {
        if self.offset >= self.buffer.len() {
            // `F::SINKS` is a compile-time constant. A `NoFlush` encoder folds
            // this to an unconditional `BufferFull` (no flush field access, no
            // dead sink call); a real sink keeps the flush-and-resume path.
            if !F::SINKS {
                return Err(Error::BufferFull);
            }
            self.drain();
        }
        // `get_mut` folds the buffer-full guard and the store into one checked
        // access: `None` only for a zero-length buffer, reported as `BufferFull`
        // instead of panicking.
        match self.buffer.get_mut(self.offset) {
            Some(slot) => {
                *slot = b;
                self.offset += 1;
                Ok(())
            }
            None => Err(Error::BufferFull),
        }
    }

    #[cfg_attr(not(feature = "fixlen"), allow(dead_code))]
    fn push_raw(&mut self, data: &[u8]) -> Result<()> {
        for &b in data {
            self.push_byte(b)?;
        }
        Ok(())
    }

    fn write_varint(&mut self, mut value: Unsigned) -> Result<()> {
        loop {
            let mut b = (value as u8) & 0x7F;
            value >>= 7;
            if value != 0 {
                b |= 0x80;
            }
            self.push_byte(b)?;
            if value == 0 {
                return Ok(());
            }
        }
    }

    fn write_id_type(&mut self, id: Id, wire_type: u8) -> Result<()> {
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        // The single choke point every field write passes through, so also where a
        // held-back sequence run is committed: the field about to be written is
        // content, which means every enclosing sequence must be framed after all.
        //
        // Unconditional on purpose — no wire-type exemption. Dropping a sequence
        // is the one case that must *not* commit, and it is expressed by not
        // reaching this function at all: `write_sequence_end` returns before the
        // call, and `write_sequence_begin_lazy` never routes through it. Anything
        // that does reach here is content, including the `SEQUENCE_END` of a kept
        // frame (whose run `write_sequence_end_keep` therefore no longer has to
        // commit itself). Exempting a wire type here would move completeness of
        // the choke point onto its callers' discipline.
        #[cfg(feature = "sequence")]
        if self.npending != 0 {
            self.commit_pending()?;
        }
        self.write_varint(((id as Unsigned) << 3) | wire_type as Unsigned)
    }

    /// Write out the held-back sequence headers, outermost first.
    ///
    /// Cold: it runs at most once per non-default sequence, never per field.
    ///
    /// Only the headers that actually **reached the buffer** are dropped from the
    /// run. If the buffer fills mid-run with no sink to drain it, the sequences
    /// not written yet stay pending — still the innermost contiguous suffix of the
    /// open sequences, so the type's invariant holds and a caller that installs a
    /// bigger buffer ([`OStream::buffer_set`]) resumes exactly where the cut fell.
    /// Clearing the run up front instead would destroy those ids: their
    /// `SEQUENCE_START` markers would never be emitted while their `SEQUENCE_END`s
    /// still are, i.e. a structurally broken stream rather than a truncated one.
    ///
    /// The recovery this buys is real but **bounded**: it only reconstructs the
    /// stream when the cut lands on a *header-varint boundary*. No writer in this
    /// encoder is atomic on failure — a multi-byte header (id > 15) can be cut in
    /// half by the same buffer end, and its leading bytes are already in the
    /// buffer while the id is still pending, so resuming re-emits it whole. As
    /// everywhere else in this encoder, `BufferFull` without a sink leaves a
    /// partial message behind; what is fixed here is that it no longer leaves an
    /// *inconsistent* one on the boundary case.
    #[cfg(feature = "sequence")]
    #[cold]
    #[inline(never)]
    fn commit_pending(&mut self) -> Result<()> {
        let mut written = 0;
        let mut result = Ok(());
        for i in 0..self.npending {
            // `get` rather than `self.pending[i]`: `i < npending <= LAZY_SEQ_DEPTH`
            // holds by construction, but the indexing form still emits a
            // `core::panicking::panic_bounds_check` path that the linker then
            // keeps in the image. The whole codec is meant to link without
            // `core::panicking` (README "Footprint"), so prove the access
            // in-bounds instead of asserting it.
            let id = match self.pending.get(i) {
                Some(&id) => id,
                None => break,
            };
            if let Err(e) =
                self.write_varint(((id as Unsigned) << 3) | T_SEQUENCE_START as Unsigned)
            {
                result = Err(e);
                break;
            }
            written += 1;
        }
        self.drop_front(written);
        result
    }

    /// Drop the outermost `k` entries of the pending run, keeping the rest as the
    /// innermost suffix. Panic-free by the same rule as [`Self::commit_pending`]:
    /// `copy_within` carries a range assert, so the shift is spelled out with
    /// `get`/`get_mut` instead.
    #[cfg(feature = "sequence")]
    fn drop_front(&mut self, k: usize) {
        if k >= self.npending {
            self.npending = 0;
            return;
        }
        let remaining = self.npending - k;
        for i in 0..remaining {
            let id = match self.pending.get(i + k) {
                Some(&id) => id,
                None => break,
            };
            match self.pending.get_mut(i) {
                Some(slot) => *slot = id,
                None => break,
            }
        }
        self.npending = remaining;
    }

    // --- scalar writers -----------------------------------------------------

    /// Write an unsigned-integer field.
    pub fn write_unsigned(&mut self, id: Id, value: Unsigned) -> Result<()> {
        self.write_id_type(id, T_VARINT_UNSIGNED)?;
        self.write_varint(value)
    }

    /// Write a signed-integer field (ZigZag + varint).
    pub fn write_signed(&mut self, id: Id, value: Signed) -> Result<()> {
        self.write_id_type(id, T_VARINT_SIGNED)?;
        self.write_varint(zigzag_encode(value))
    }

    /// Write a boolean as an unsigned `0` / `1`.
    #[inline]
    pub fn write_boolean(&mut self, id: Id, value: bool) -> Result<()> {
        self.write_unsigned(id, value as Unsigned)
    }

    // --- fixed-length writers ----------------------------------------------

    /// Write a fixed-length field: header, `(len << 3) | subtype` varint, then
    /// the raw `data` bytes (already in wire/little-endian order for floats).
    #[cfg(feature = "fixlen")]
    pub fn write_fixlen(&mut self, id: Id, data: &[u8], subtype: FixlenType) -> Result<()> {
        self.write_id_type(id, T_FIXLEN)?;
        self.write_varint(((data.len() as Unsigned) << 3) | subtype as Unsigned)?;
        self.push_raw(data)
    }

    /// Write a 32-bit float field.
    #[cfg(feature = "fixlen")]
    #[inline]
    pub fn write_fp32(&mut self, id: Id, value: f32) -> Result<()> {
        self.write_fixlen(id, &value.to_le_bytes(), FixlenType::Fp32)
    }

    /// Write a 64-bit float field.
    #[cfg(feature = "fp64")]
    #[inline]
    pub fn write_fp64(&mut self, id: Id, value: f64) -> Result<()> {
        self.write_fixlen(id, &value.to_le_bytes(), FixlenType::Fp64)
    }

    /// Write a string field (raw UTF-8 bytes, no NUL on the wire).
    ///
    /// The input is `&str`, so it is **already valid UTF-8** by the type system
    /// — encode is strict by construction and can never emit non-UTF-8 bytes
    /// (MESSAGE_SPEC §8, CORELIB_PLAN §6.4). For arbitrary bytes use
    /// [`OStream::write_blob`]. Embedded `U+0000` is permitted and written
    /// verbatim (the wire is length-framed, no NUL terminator).
    #[cfg(feature = "fixlen")]
    #[inline]
    pub fn write_str(&mut self, id: Id, text: &str) -> Result<()> {
        self.write_fixlen(id, text.as_bytes(), FixlenType::Str)
    }

    /// Write a binary blob field.
    #[cfg(feature = "fixlen")]
    #[inline]
    pub fn write_blob(&mut self, id: Id, data: &[u8]) -> Result<()> {
        self.write_fixlen(id, data, FixlenType::Blob)
    }

    // --- array writers ------------------------------------------------------

    /// Write an array of unsigned integers (`u8`/`u16`/`u32`/`u64` elements).
    ///
    /// Element width is fixed by the type at compile time, so the invalid
    /// element-size error from the C API is impossible here.
    #[cfg(feature = "array")]
    pub fn write_array_unsigned<T: UnsignedElem>(&mut self, id: Id, data: &[T]) -> Result<()> {
        self.write_id_type(id, T_VARINTARRAY_UNSIGNED)?;
        self.write_varint(data.len() as Unsigned)?;
        for e in data {
            self.write_varint(e.widen())?;
        }
        Ok(())
    }

    /// Write an array of signed integers (`i8`/`i16`/`i32`/`i64` elements).
    #[cfg(feature = "array")]
    pub fn write_array_signed<T: SignedElem>(&mut self, id: Id, data: &[T]) -> Result<()> {
        self.write_id_type(id, T_VARINTARRAY_SIGNED)?;
        self.write_varint(data.len() as Unsigned)?;
        for e in data {
            self.write_varint(zigzag_encode(e.widen()))?;
        }
        Ok(())
    }

    /// Write an array of 32-bit floats.
    ///
    /// The `fixlen_word` is **always** present — even for a zero-count array —
    /// so an empty `fp32` array stays distinguishable from an empty `fp64` one
    /// (§4.8): the field is `[ header ][ count ][ fixlen_word ][ payload… ]`,
    /// where an empty array simply has no payload.
    #[cfg(all(feature = "array", feature = "fixlen"))]
    pub fn write_array_fp32(&mut self, id: Id, data: &[f32]) -> Result<()> {
        self.write_id_type(id, T_FIXLENARRAY)?;
        self.write_varint(data.len() as Unsigned)?;
        self.write_varint((4 << 3) | FixlenType::Fp32 as Unsigned)?;
        for &e in data {
            self.push_raw(&e.to_le_bytes())?;
        }
        Ok(())
    }

    /// Write an array of 64-bit floats.
    ///
    /// The `fixlen_word` is **always** present — even for a zero-count array —
    /// so an empty `fp64` array stays distinguishable from an empty `fp32` one
    /// (§4.8): the field is `[ header ][ count ][ fixlen_word ][ payload… ]`,
    /// where an empty array simply has no payload.
    #[cfg(all(feature = "array", feature = "fp64"))]
    pub fn write_array_fp64(&mut self, id: Id, data: &[f64]) -> Result<()> {
        self.write_id_type(id, T_FIXLENARRAY)?;
        self.write_varint(data.len() as Unsigned)?;
        self.write_varint((8 << 3) | FixlenType::Fp64 as Unsigned)?;
        for &e in data {
            self.push_raw(&e.to_le_bytes())?;
        }
        Ok(())
    }

    // --- sequence writers ---------------------------------------------------

    /// Open a nested sequence whose header is **held back** until the sequence
    /// turns out to have content.
    ///
    /// MESSAGE_SPEC §2 omits a sequence-typed field whose value equals its declared
    /// default, and "not one child was written" is exactly that condition —
    /// evaluated per child field, recursively, for free. A sequence closed with
    /// nothing in it therefore emits **nothing** instead of a two-byte empty frame,
    /// and an all-default message becomes the empty byte string.
    ///
    /// The predicate is never a byte image of the object, so struct padding cannot
    /// influence it and a non-zero nested default is handled by the caller's
    /// ordinary per-field test.
    ///
    /// This is the only way to open a sequence. How it closes decides whether a
    /// contentless one survives: [`OStream::write_sequence_end`] drops it,
    /// [`OStream::write_sequence_end_keep`] forces the frame out.
    ///
    /// Costs no output bytes and no allocation; the held-back ids live in a
    /// [`LAZY_SEQ_DEPTH`]-slot array inside the encoder. That window is the one
    /// place this heap-free port is not canonical: opening a sequence while the
    /// window is full commits the pending run and frames this one **eagerly**, so
    /// an all-default sequence nested deeper than [`LAZY_SEQ_DEPTH`] keeps the
    /// empty frame §2 would have omitted — well-formed, decodes to the same
    /// value, documented in the README (CORELIB_PLAN §6).
    #[cfg(feature = "sequence")]
    #[inline]
    pub fn write_sequence_begin_lazy(&mut self, id: Id) -> Result<()> {
        if self.depth >= MAX_DEPTH {
            return Err(Error::Argument);
        }
        if id > ID_MAX {
            return Err(Error::Argument);
        }
        // `get_mut` is the panic-free spelling of `self.npending < LAZY_SEQ_DEPTH`
        // followed by an index: `None` *is* the window-full case.
        if let Some(slot) = self.pending.get_mut(self.npending) {
            *slot = id;
            self.npending += 1;
        } else {
            // Deeper than the hold-back window: commit the run and frame eagerly,
            // which keeps the suffix invariant above. Valid, just not canonical if
            // this sequence turns out to be all-default (CORELIB_PLAN §6, the
            // heap-free allowance — see [`LAZY_SEQ_DEPTH`]).
            self.commit_pending()?;
            self.write_varint(((id as Unsigned) << 3) | T_SEQUENCE_START as Unsigned)?;
        }
        self.depth += 1;
        Ok(())
    }

    /// Close the most recently opened nested sequence, letting it **vanish** if it
    /// received no content.
    ///
    /// Use it wherever absence encodes the same value as an empty frame: a
    /// `struct`/`union` field, and an array field whose declared `default` is the
    /// empty collection (MESSAGE_SPEC §2). Where the frame must be visible, close
    /// with [`OStream::write_sequence_end_keep`] instead.
    #[cfg(feature = "sequence")]
    #[inline]
    pub fn write_sequence_end(&mut self) -> Result<()> {
        if self.npending != 0 {
            // The innermost open sequence is the last held-back one: drop it.
            self.npending -= 1;
            self.depth = self.depth.saturating_sub(1);
            return Ok(());
        }
        self.write_id_type(0, T_SEQUENCE_END)?;
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }

    /// Close the most recently opened nested sequence, **keeping** its frame even
    /// when it received no content.
    ///
    /// Behaves like a write: it first emits any held-back headers — this frame's
    /// and every enclosing one's — and then the end marker, so an empty sequence
    /// reaches the wire as `begin` + `end`.
    ///
    /// Required wherever the frame carries information beyond its contents:
    /// - a **wrapper-array element** (`struct`/`union`/nested row): element
    ///   presence is what carries a dynamic array's length — *highest present id +
    ///   1* (§5.1) — so dropping an all-default element would change the decoded
    ///   length, not just the bytes;
    /// - an array field already known to **differ from a non-empty declared
    ///   `default`**: absence would reconstruct that default, so the empty frame is
    ///   the only encoding of "explicitly empty" (§2, §3).
    ///
    /// The two failure directions are not symmetric, which is why this is the safe
    /// choice when in doubt: using it where [`OStream::write_sequence_end`] would
    /// do costs one non-canonical empty frame that a decoder normalizes away, while
    /// the reverse silently changes an array's length.
    #[cfg(feature = "sequence")]
    #[inline]
    pub fn write_sequence_end_keep(&mut self) -> Result<()> {
        // The held-back run is committed by `write_id_type` itself — this closer
        // is an ordinary write, so it needs no commit of its own.
        self.write_id_type(0, T_SEQUENCE_END)?;
        self.depth = self.depth.saturating_sub(1);
        Ok(())
    }
}

/// Unsigned integer element that can be widened to the wire value type.
#[cfg(feature = "array")]
pub trait UnsignedElem: Copy {
    /// Zero-extend to [`Unsigned`].
    fn widen(self) -> Unsigned;
}

/// Signed integer element that can be widened to the wire value type.
#[cfg(feature = "array")]
pub trait SignedElem: Copy {
    /// Sign-extend to [`Signed`].
    fn widen(self) -> Signed;
}

#[cfg(feature = "array")]
macro_rules! impl_unsigned_elem {
    ($($t:ty),*) => {$(
        impl UnsignedElem for $t {
            #[inline]
            fn widen(self) -> Unsigned { self as Unsigned }
        }
    )*};
}

#[cfg(feature = "array")]
macro_rules! impl_signed_elem {
    ($($t:ty),*) => {$(
        impl SignedElem for $t {
            #[inline]
            fn widen(self) -> Signed { self as Signed }
        }
    )*};
}

#[cfg(feature = "array")]
impl_unsigned_elem!(u8, u16, u32);
#[cfg(all(feature = "array", feature = "value64"))]
impl_unsigned_elem!(u64);
#[cfg(feature = "array")]
impl_signed_elem!(i8, i16, i32);
#[cfg(all(feature = "array", feature = "value64"))]
impl_signed_elem!(i64);
