//! Server-sent-event framing — bytes to frames, and nothing above that.
//!
//! # Why framing is its own layer
//!
//! Two of the six provider grammars in #1682's census ride SSE (OpenAI's
//! `delta` chunks and Anthropic's typed `content_block_delta` events) and they
//! disagree about *everything above the frame*: what an event name means, where
//! the terminator lives, how a tool call is split. They agree exactly on the
//! transport. Writing the transport twice is how the two decoders end up with
//! two different chunk-boundary bugs, so it is written once here and the
//! grammars are what differ — which is also the test of whether the canonical
//! vocabulary is OpenAI-biased.
//!
//! # Chunk boundaries are not line boundaries
//!
//! The whole file is a state machine over a byte sequence: bytes accumulate
//! into a line, lines into a frame, and nothing is decided until the byte that
//! decides it arrives.
//!
//! The line terminator is where that gets subtle. The SSE grammar admits all
//! three of CRLF, bare LF and bare CR, so a `\r` ends a line *on its own* — a
//! deployment behind a proxy that rewrites line endings, or a provider written
//! against a classic-Mac-era library, is not malformed and must not decode as
//! one long unterminated line. What must not happen is the opposite mistake:
//! treating the LF of a CRLF as a second, empty line. So a CR ends the line and
//! sets [`SseFramer::after_cr`], and an LF arriving while that flag is set is
//! swallowed. The flag lives on the framer rather than in the loop because "the
//! read ended between the CR and the LF" is the single most common way a
//! hand-rolled reader produces a phantom empty line and dispatches half an
//! event.
//!
//! # Strict, not browser-lenient
//!
//! The WHATWG `EventSource` grammar ignores any line it does not recognise,
//! because a browser must survive a server that invents fields. This decoder is
//! not a browser: it is reading one known provider grammar over a channel that
//! is also, occasionally, a proxy's error page or a different protocol
//! entirely. Ignoring unrecognised field lines there means an NDJSON body — the
//! Ollama-shaped grammar, whose lines are bare JSON documents — decodes as a
//! long run of ignorable garbage followed by "the stream ended without its
//! terminator", which reads as *the provider truncated* when the truth is *this
//! is not SSE at all*. So an unknown field name, or a line with no `:` in it,
//! is a typed [`StreamDecodeErrorKind::MalformedFrame`], and the mistake is
//! visible where it happens.
//!
//! # Why `push` returns frames *and* an error instead of `Result`
//!
//! A chunk can contain three good frames followed by a broken one. If the
//! signature were `Result<Vec<SseFrame>, _>`, the three good frames would be
//! dropped on the floor whenever the break happened to land in the same
//! chunk — and *whether it does* depends on where the network split the bytes.
//! That is precisely the chunk-boundary invariance the design requires this
//! layer to hold, so the frames that were legitimately parsed are always
//! returned, with the failure alongside them.

use super::stream::{StreamDecodeError, StreamDecodeErrorKind};

/// The most bytes one frame (or one un-terminated line) may accumulate.
///
/// A stream is attacker-influenced input and an accumulator without a ceiling
/// is a memory bug waiting for a hostile provider: a server that answers with
/// one megabyte-long `data:` line, or with no newline at all, must not get to
/// choose how much of this process's heap it consumes.
///
/// It is a ceiling on the *frame*, not on one of its fields: every byte the
/// frame under construction retains is charged against it, whichever field
/// carried it. A per-field ceiling would be no ceiling at all — a frame with a
/// near-limit `event:` name and a near-limit `data:` value passes every
/// individual check and retains twice what this constant says.
pub(super) const MAX_FRAME_BYTES: usize = 1 << 20;

/// One dispatched SSE frame.
///
/// `data` is `Option` rather than `String` because "the frame carried no data
/// field" and "the frame carried an empty data field" are different events in
/// both grammars — Anthropic's `ping` is the first, an empty text delta is the
/// second — and collapsing them would make a grammar decide from an ambiguity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SseFrame {
    /// The `event:` field, when the frame set one.
    pub(super) event: Option<String>,
    /// Every `data:` line of the frame, joined with `\n`, as the SSE grammar
    /// specifies.
    pub(super) data: Option<String>,
}

/// The SSE byte-to-frame state machine.
#[derive(Debug, Default)]
pub(super) struct SseFramer {
    /// Bytes of the line currently being read, without its terminator.
    line: Vec<u8>,
    /// The `event:` field of the frame being accumulated.
    event: Option<String>,
    /// The `data:` lines of the frame being accumulated.
    data: Option<String>,
    /// How many bytes the frame being accumulated retains, across every field
    /// of it, for the ceiling.
    frame_bytes: usize,
    /// Whether the previous byte was a CR that ended a line, so the LF of a
    /// CRLF does not end a second, empty one. It is framer state and not loop
    /// state because a read may end between the two.
    after_cr: bool,
}

impl SseFramer {
    /// A framer with no bytes seen.
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Feed the next chunk of body bytes, in order.
    ///
    /// Returns every frame the chunk completed, in order, plus the first
    /// framing failure it hit — see the module note on why both.
    pub(super) fn push(&mut self, chunk: &[u8]) -> (Vec<SseFrame>, Option<StreamDecodeError>) {
        let mut frames = Vec::new();
        for byte in chunk {
            // Taken unconditionally: any byte other than the LF of a CRLF
            // clears the flag, including a data byte, which is the case where
            // the CR ended a line and the LF simply never came.
            let after_cr = std::mem::take(&mut self.after_cr);
            match *byte {
                b'\n' if after_cr => continue,
                b'\n' | b'\r' => {
                    self.after_cr = *byte == b'\r';
                    match self.take_line() {
                        Ok(Some(frame)) => frames.push(frame),
                        Ok(None) => {}
                        Err(error) => return (frames, Some(error)),
                    }
                }
                byte => {
                    if self.line.len() >= MAX_FRAME_BYTES {
                        return (frames, Some(oversized_line()));
                    }
                    self.line.push(byte);
                }
            }
        }
        (frames, None)
    }

    /// The byte source ended.
    ///
    /// `dispatch` says whether a frame that never got its blank line still
    /// counts. On a clean EOF it does: the transport stated the body ended
    /// exactly there, so a server that omitted the trailing blank line after
    /// its terminator sent a complete frame and refusing it would turn a
    /// finished stream into a fake truncation. On a truncated EOF it does not:
    /// bytes are missing, so whatever is half-read is half of something, and
    /// dispatching it would invent an event the provider never finished.
    pub(super) fn finish(
        &mut self,
        dispatch: bool,
    ) -> (Option<SseFrame>, Option<StreamDecodeError>) {
        if !dispatch {
            self.line.clear();
            self.event = None;
            self.data = None;
            self.frame_bytes = 0;
            self.after_cr = false;
            return (None, None);
        }
        if !self.line.is_empty() {
            match self.take_line() {
                // A non-empty line never dispatches a frame — only a blank one
                // does — so this arm is unreachable; it is written out rather
                // than `unreachable!()` because a panic on the spend path to
                // save one line of code is a bad trade.
                Ok(Some(frame)) => return (Some(frame), None),
                Ok(None) => {}
                Err(error) => return (None, Some(error)),
            }
        }
        (self.dispatch_pending(), None)
    }

    /// Consumes the buffered line, returning a frame if it closed one.
    ///
    /// The line holds no terminator: a CR ends a line in [`Self::push`] rather
    /// than being buffered and stripped here, so there is no trailing byte to
    /// undo. The buffer itself is moved out and back so its allocation is
    /// reused for the next line instead of being freed once per line.
    fn take_line(&mut self) -> Result<Option<SseFrame>, StreamDecodeError> {
        let mut line = std::mem::take(&mut self.line);
        let outcome = self.consume_line(&line);
        line.clear();
        self.line = line;
        outcome
    }

    /// Applies one complete line to the frame under construction.
    fn consume_line(&mut self, line: &[u8]) -> Result<Option<SseFrame>, StreamDecodeError> {
        if line.is_empty() {
            return Ok(self.dispatch_pending());
        }
        // UTF-8 is validated per line rather than per chunk on purpose: a
        // multi-byte character split across two network reads is legal and
        // common, and a per-chunk check would reject it. A line, by
        // construction, holds whole characters or the stream really is
        // ill-formed.
        let Ok(text) = std::str::from_utf8(line) else {
            return Err(StreamDecodeError {
                kind: StreamDecodeErrorKind::InvalidUtf8,
                detail: "an SSE line was not valid UTF-8",
            });
        };
        if text.starts_with(':') {
            // A comment. Providers use these as keep-alives
            // (`: OPENROUTER PROCESSING`), so they must cost nothing.
            return Ok(None);
        }
        let Some((name, raw_value)) = text.split_once(':') else {
            return Err(StreamDecodeError {
                kind: StreamDecodeErrorKind::MalformedFrame,
                detail: "an SSE field line carried no ':' separator; \
                         a bare JSON line is NDJSON, not SSE",
            });
        };
        let value = raw_value.strip_prefix(' ').unwrap_or(raw_value);
        match name {
            "data" => {
                // The first `data:` line of a frame costs exactly its own
                // bytes; every line after it also costs the `\n` the grammar
                // joins it with below. Charging `+ 1` unconditionally would
                // bill a byte the frame never actually retains — the stored
                // string only grows a leading newline from the *second* line
                // onward — and a multi-line payload sized to land exactly on
                // `MAX_FRAME_BYTES` would be rejected one byte early.
                let joiner = if self.data.is_some() { 1 } else { 0 };
                self.charge(value.len().saturating_add(joiner))?;
                match &mut self.data {
                    Some(existing) => {
                        existing.push('\n');
                        existing.push_str(value);
                    }
                    None => self.data = Some(value.to_string()),
                }
            }
            "event" => {
                // Charged like `data:`, because the frame retains this string
                // too and the ceiling is on the frame. A repeat charges again
                // rather than refunding the value it replaces: the bytes
                // crossed the wire either way, and a provider that rewrites
                // one field a thousand times is not owed a fresh budget each
                // time it does.
                self.charge(value.len())?;
                self.event = Some(value.to_string());
            }
            // Reconnection fields. This decoder never reconnects — the
            // executor owns the connection and a resumed stream would be a
            // second invocation — so they are read and dropped rather than
            // rejected, because a provider that sends them is not malformed.
            // Dropped means not retained, so they are not charged against the
            // frame ceiling; the per-line ceiling in [`Self::push`] is what
            // bounds them.
            "id" | "retry" => {}
            _ => {
                return Err(StreamDecodeError {
                    kind: StreamDecodeErrorKind::MalformedFrame,
                    detail: "an SSE line named a field outside the grammar",
                })
            }
        }
        Ok(None)
    }

    /// Charges bytes the frame under construction will retain against the
    /// frame ceiling.
    fn charge(&mut self, bytes: usize) -> Result<(), StreamDecodeError> {
        let total = self.frame_bytes.saturating_add(bytes);
        if total > MAX_FRAME_BYTES {
            return Err(oversized_line());
        }
        self.frame_bytes = total;
        Ok(())
    }

    /// Emits the frame under construction, if it has any field at all.
    fn dispatch_pending(&mut self) -> Option<SseFrame> {
        if self.event.is_none() && self.data.is_none() {
            // Blank lines between frames, and runs of them, are not events.
            return None;
        }
        self.frame_bytes = 0;
        Some(SseFrame {
            event: self.event.take(),
            data: self.data.take(),
        })
    }
}

/// The ceiling failure, spelled once so both call sites cannot drift.
fn oversized_line() -> StreamDecodeError {
    StreamDecodeError {
        kind: StreamDecodeErrorKind::EventTooLarge,
        detail: "a single stream event exceeded the decoder's buffer ceiling",
    }
}
