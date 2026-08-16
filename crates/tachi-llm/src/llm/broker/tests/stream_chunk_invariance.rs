//! `decode(rechunk(bytes, any_split)) == decode(bytes)`.
//!
//! # The one property a hand-rolled stream reader always gets wrong
//!
//! A network read boundary is not an event boundary, and a decoder that
//! accidentally treats it as one is *not* broken in an obvious way — it works
//! for every fixture written by hand, because a person writing a fixture puts
//! whole events in chunks. It breaks in production, intermittently, on the
//! provider whose TLS record happens to split a `\r\n` or a multi-byte
//! character. That is why the frozen design names this property for fuzzing
//! rather than trusting the transcript corpus alone.
//!
//! # What is fuzzed, and why it is deterministic
//!
//! Every transcript in the corpus is re-decoded under a family of chunkings:
//! whole, one byte at a time, several fixed sizes, every single split point,
//! and pseudo-random multi-splits from a fixed seed. No new dependency and no
//! randomness that cannot be replayed — a property test that fails once on CI
//! and never again is worse than no property test, because it teaches everyone
//! to re-run the job.
//!
//! The comparison is the decoder's whole observable answer: the event sequence,
//! the decode failure and the terminal disposition. Comparing only the events
//! would miss the interesting half — a decoder can produce identical events and
//! seal a different disposition depending on where the bytes were cut.

use super::stream_transcripts::{
    decode_transcript, transcript_chunks, transcript_terminal, DecodedTranscript, TerminalInput,
    STREAM_CORPORA,
};
use super::*;

/// Transcripts up to this size get every single split point tried, which is
/// quadratic and therefore bounded.
const EXHAUSTIVE_SPLIT_LIMIT: usize = 4096;

/// Transcripts up to this size get the one-byte-at-a-time chunking.
const BYTE_AT_A_TIME_LIMIT: usize = 65_536;

/// How many pseudo-random multi-splits each transcript gets.
const RANDOM_SPLIT_RUNS: usize = 48;

/// A deterministic xorshift, so a failure is reproducible from the seed in the
/// assertion message instead of being "it went red on CI once".
struct Rng(u64);

impl Rng {
    /// The next value in the sequence.
    fn step(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        self.0 = state;
        state
    }

    /// A value in `0..bound`.
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.step() % bound as u64) as usize
    }
}

/// The bytes of a transcript, as one sequence.
fn flatten(chunks: &[Vec<u8>]) -> Vec<u8> {
    chunks.concat()
}

/// Splits `bytes` at the given offsets, which must be sorted and in range.
fn split_at(bytes: &[u8], cuts: &[usize]) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    let mut previous = 0;
    for cut in cuts {
        chunks.push(bytes[previous..*cut].to_vec());
        previous = *cut;
    }
    chunks.push(bytes[previous..].to_vec());
    chunks
}

/// Splits `bytes` into fixed-size chunks.
fn fixed(bytes: &[u8], size: usize) -> Vec<Vec<u8>> {
    bytes.chunks(size.max(1)).map(<[u8]>::to_vec).collect()
}

#[test]
fn decoding_is_invariant_under_every_chunk_boundary() {
    let mut transcripts = 0;
    let mut chunkings = 0;

    for (dir, grammar) in STREAM_CORPORA {
        for (name, fixture) in load_fixtures(dir) {
            let recorded = transcript_chunks(&fixture, &name);
            let terminal = transcript_terminal(&fixture, &name);
            let bytes = flatten(&recorded);
            let baseline = decode_transcript(grammar, &recorded, terminal);
            transcripts += 1;

            // Each chunking is built, run and dropped one at a time: holding
            // every rechunking of a megabyte-sized transcript at once would
            // make this test the memory hog of the suite for no benefit.
            let mut check = |how: String, chunks: Vec<Vec<u8>>| {
                assert_eq!(
                    flatten(&chunks),
                    bytes,
                    "the rechunking {how} of {name} changed the bytes; the test is \
                     wrong, not the decoder"
                );
                let rechunked = decode_transcript(grammar, &chunks, terminal);
                assert_transcripts_agree(&name, &how, &baseline, &rechunked);
                chunkings += 1;
            };

            check("whole".to_string(), vec![bytes.clone()]);
            for size in [2usize, 3, 7, 64, 997, 4096] {
                check(format!("fixed({size})"), fixed(&bytes, size));
            }
            if bytes.len() <= BYTE_AT_A_TIME_LIMIT {
                check("one byte at a time".to_string(), fixed(&bytes, 1));
            }
            if bytes.len() <= EXHAUSTIVE_SPLIT_LIMIT {
                for cut in 0..=bytes.len() {
                    check(format!("split at {cut}"), split_at(&bytes, &[cut]));
                }
            }
            // A seed derived from the fixture name, so every fixture gets a
            // different split family and every run gets the same one.
            let seed = name
                .bytes()
                .fold(0x2545_f491_4f6c_dd1d_u64, |acc, byte| {
                    acc.wrapping_mul(31).wrapping_add(u64::from(byte))
                })
                .max(1);
            let mut rng = Rng(seed);
            for run in 0..RANDOM_SPLIT_RUNS {
                let mut cuts: Vec<usize> = (0..rng.below(8) + 1)
                    .map(|_| rng.below(bytes.len() + 1))
                    .collect();
                cuts.sort_unstable();
                let chunks = split_at(&bytes, &cuts);
                check(format!("random(seed {seed}, run {run})"), chunks);
            }
        }
    }

    assert!(
        transcripts >= 47,
        "only {transcripts} transcripts were fuzzed"
    );
    assert!(
        chunkings >= 2_000,
        "only {chunkings} chunkings were tried — the fuzz is not actually running"
    );
}

/// Compares two runs field by field, because `assert_eq!` on the whole struct
/// prints two walls of JSON and leaves the reader to diff them.
#[track_caller]
fn assert_transcripts_agree(
    name: &str,
    how: &str,
    baseline: &DecodedTranscript,
    rechunked: &DecodedTranscript,
) {
    assert_eq!(
        rechunked.events,
        baseline.events,
        "{name}: rechunking {how} changed the event sequence\n  rechunked: {}\n  recorded:  {}",
        serde_json::to_string(&rechunked.events).unwrap_or_default(),
        serde_json::to_string(&baseline.events).unwrap_or_default(),
    );
    assert_eq!(
        rechunked.error, baseline.error,
        "{name}: rechunking {how} changed the decode failure"
    );
    assert_eq!(
        rechunked.disposition, baseline.disposition,
        "{name}: rechunking {how} changed the terminal disposition"
    );
}

#[test]
fn a_multibyte_character_split_across_reads_still_decodes() {
    // The failure this catches directly: validating UTF-8 per *chunk* instead
    // of per line. Every one of these splits lands inside a character, which is
    // legal on the wire and which a per-chunk check would reject as invalid
    // UTF-8 — a real provider answering in Chinese would fail intermittently
    // depending on the network.
    let body = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"café 世界 🌍\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .as_bytes();

    let whole = decode_transcript(
        "openai_compat_sse",
        &[body.to_vec()],
        TerminalInput::Finish(StreamEof::Clean),
    );
    assert_eq!(
        whole.events.len(),
        3,
        "sanity: started, one text delta, completed"
    );
    assert_eq!(whole.error, None);

    for cut in 0..=body.len() {
        let chunks = split_at(body, &[cut]);
        let rechunked = decode_transcript(
            "openai_compat_sse",
            &chunks,
            TerminalInput::Finish(StreamEof::Clean),
        );
        assert_transcripts_agree("multibyte", &format!("split at {cut}"), &whole, &rechunked);
    }
}

#[test]
fn an_empty_read_changes_nothing() {
    // A zero-length read is legal — it happens on a stalled connection that
    // recovers — and a decoder that treats it as end-of-body would report a
    // truncation on a stream that is merely slow.
    let body = concat!(
        "data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":",
        "{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .as_bytes();

    let whole = decode_transcript(
        "openai_compat_sse",
        &[body.to_vec()],
        TerminalInput::Finish(StreamEof::Clean),
    );

    let mut padded: Vec<Vec<u8>> = vec![Vec::new()];
    for byte in body {
        padded.push(vec![*byte]);
        padded.push(Vec::new());
    }
    let interleaved = decode_transcript(
        "openai_compat_sse",
        &padded,
        TerminalInput::Finish(StreamEof::Clean),
    );
    assert_transcripts_agree(
        "empty reads",
        "byte-wise with empty reads",
        &whole,
        &interleaved,
    );
}
