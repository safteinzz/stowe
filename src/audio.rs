//! Audio fingerprinting: a content hash of a file's *decoded PCM*, not its
//! container bytes.
//!
//! The point is identity that survives **renames and tag edits**. blake3 of the
//! raw file changes the moment you touch an ID3/Vorbis tag; blake3 of the
//! decoded samples does not — the audio is the same, so the fingerprint is the
//! same. That's what lets `diff` recognise a re-tagged song as a move rather
//! than an add+remove.
//!
//! It does *not* survive re-encoding or trimming (those change the samples).
//! Those fall back to being treated as new content — an accepted trade for a
//! single pure-Rust dependency and zero external binaries.

use anyhow::Result;
use std::path::Path;

use symphonia::core::codecs::{CodecParameters, audio::AudioDecoderOptions};
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Extensions we attempt to fingerprint. Anything else gets no `fp`.
const AUDIO_EXTS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "m4a", "mp4", "aac", "alac", "wav", "wave", "aiff", "aif",
];

/// True if `path`'s extension looks like audio we can decode.
pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// blake3 of the decoded PCM samples of an audio file.
///
/// Returns `Ok(None)` when the file isn't decodable audio (unknown codec,
/// corrupt, etc.) — a fingerprint is a *best-effort bonus*, never required, so
/// a failure here must not fail the scan.
pub fn fingerprint(path: &Path) -> Result<Option<String>> {
    if !is_audio(path) {
        return Ok(None);
    }
    Ok(decode_hash(path).unwrap_or(None))
}

/// The fallible core: decode every packet and fold the samples into a blake3
/// hash. Hashes the sample rate + channel count first so two clips that share
/// samples at different rates/layouts don't collide.
fn decode_hash(path: &Path) -> Result<Option<String>> {
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = match symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    ) {
        Ok(f) => f,
        Err(_) => return Ok(None),
    };

    let track = match format.default_track(TrackType::Audio) {
        Some(t) => t,
        None => return Ok(None),
    };
    let track_id = track.id;
    let params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p,
        _ => return Ok(None),
    };

    let mut decoder = match symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
    {
        Ok(d) => d,
        Err(_) => return Ok(None),
    };

    let mut hasher = blake3::Hasher::new();
    let mut header_done = false;
    let mut interleaved: Vec<i16> = Vec::new();
    // Reused byte buffer: we feed blake3 one big slice per packet rather than
    // one call per sample (a song is tens of millions of samples — per-sample
    // `update` calls dominated the runtime). Little-endian explicitly so the
    // fingerprint is identical across machines of either endianness.
    let mut bytes: Vec<u8> = Vec::new();

    // `next_packet` yields `Ok(None)` at clean end-of-stream and `Err` on a
    // truncated/garbled tail — both end the loop, hashing whatever we read.
    while let Ok(Some(packet)) = format.next_packet() {
        if packet.track_id != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(_) => break,
        };

        if !header_done {
            let spec = decoded.spec();
            hasher.update(&spec.rate().to_le_bytes());
            hasher.update(&(spec.channels().count() as u32).to_le_bytes());
            header_done = true;
        }

        let n = decoded.samples_interleaved();
        if interleaved.len() < n {
            interleaved.resize(n, 0);
        }
        decoded.copy_to_slice_interleaved::<i16, _>(&mut interleaved[..n]);

        bytes.clear();
        bytes.reserve(n * 2);
        for s in &interleaved[..n] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        hasher.update(&bytes);
    }

    // Nothing decoded (e.g. a 0-byte file slipping past the extension check) —
    // report "no fingerprint" rather than hashing nothing, which would alias
    // every empty file together.
    if !header_done {
        return Ok(None);
    }
    Ok(Some(hasher.finalize().to_hex().to_string()))
}
