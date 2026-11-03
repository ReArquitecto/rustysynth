#![allow(dead_code)]

use std::io::Read;

use crate::binary_reader::BinaryReader;
use crate::error::SoundFontError;
use crate::four_cc::FourCC;
use crate::read_counter::ReadCounter;

#[cfg(feature = "sf3")]
use crate::sample_header::SampleHeader;

#[non_exhaustive]
pub(crate) enum SoundFontSampleData {
    Sf2 {
        bits_per_sample: i32,
        wave_data: Vec<i16>,
    },
    #[cfg(feature = "sf3")]
    Sf3 {
        raw_data: Vec<u8>,
    },
}

impl SoundFontSampleData {
    pub(crate) fn new<R: Read>(reader: &mut R, is_sf3: bool) -> Result<Self, SoundFontError> {
        let chunk_id = BinaryReader::read_four_cc(reader)?;
        if chunk_id != b"LIST" {
            return Err(SoundFontError::ListChunkNotFound);
        }

        let end = BinaryReader::read_u32(reader)? as usize;
        let reader = &mut ReadCounter::new(reader);

        let list_type = BinaryReader::read_four_cc(reader)?;
        if list_type != b"sdta" {
            return Err(SoundFontError::InvalidListChunkType {
                expected: FourCC::from_bytes(*b"sdta"),
                actual: list_type,
            });
        }

        let mut wave_data: Option<Vec<i16>> = None;
        #[cfg(feature = "sf3")]
        let mut raw_data: Option<Vec<u8>> = None;

        while reader.bytes_read() < end {
            let id = BinaryReader::read_four_cc(reader)?;
            let size = BinaryReader::read_u32(reader)? as usize;

            match id.as_bytes() {
                b"smpl" => {
                    if is_sf3 {
                        // SF3 stores each sample as an Ogg Vorbis stream. Keep the
                        // raw bytes; they are decoded once the sample headers (shdr)
                        // have been read, because the shdr start/end are byte offsets
                        // into this buffer.
                        #[cfg(feature = "sf3")]
                        {
                            let mut data = vec![0u8; size];
                            reader.read_exact(&mut data)?;
                            raw_data = Some(data);
                        }
                        #[cfg(not(feature = "sf3"))]
                        {
                            return Err(SoundFontError::UnsupportedSampleFormat);
                        }
                    } else {
                        wave_data = Some(BinaryReader::read_wave_data(reader, size)?);
                    }
                }
                b"sm24" => BinaryReader::discard_data(reader, size)?,
                _ => return Err(SoundFontError::ListContainsUnknownId(id)),
            }

            BinaryReader::skip_pad(reader, size)?;
        }

        if let Some(wave_data) = wave_data {
            if wave_data.len() < 2 {
                return Err(SoundFontError::SampleDataNotFound);
            }

            // An SF2 whose sample data is Ogg-compressed is actually an SF3 whose
            // `ifil` major version was not bumped. Reject it rather than
            // interpreting compressed bytes as PCM.
            let four_cc = unsafe { std::slice::from_raw_parts(wave_data.as_ptr() as *const u8, 4) };
            if four_cc == b"OggS" {
                return Err(SoundFontError::UnsupportedSampleFormat);
            }

            return Ok(Self::Sf2 {
                bits_per_sample: 16,
                wave_data,
            });
        }

        #[cfg(feature = "sf3")]
        if let Some(raw_data) = raw_data {
            return Ok(Self::Sf3 { raw_data });
        }

        Err(SoundFontError::SampleDataNotFound)
    }
}

/// Decodes SF3 sample data: each sample header's `start`/`end` are byte offsets
/// into `raw_data` delimiting an Ogg Vorbis stream. Decodes each stream to mono
/// PCM and rewrites the headers to frame offsets into the returned `wave_data`.
#[cfg(feature = "sf3")]
pub(crate) fn decode_sf3(
    raw_data: &[u8],
    sample_headers: &mut [SampleHeader],
) -> Result<Vec<i16>, SoundFontError> {
    use std::io::Cursor;

    use lewton::inside_ogg::OggStreamReader;

    // SF2 reserves 46 zero frames after each sample for interpolation padding.
    const SAMPLE_PADDING_FRAMES: usize = 46;

    let mut wave_data: Vec<i16> = Vec::new();

    for header in sample_headers.iter_mut() {
        let start = header.start as usize;
        let end = header.end as usize;

        if start > end || end > raw_data.len() {
            return Err(SoundFontError::SanityCheckFailed);
        }

        let mut reader =
            OggStreamReader::new(Cursor::new(&raw_data[start..end])).map_err(sf3_error)?;
        let channels = reader.ident_hdr.audio_channels as usize;

        let mut interleaved: Vec<i16> = Vec::new();
        loop {
            match reader.read_dec_packet_itl().map_err(sf3_error)? {
                Some(packet) => interleaved.extend_from_slice(&packet),
                None => break,
            }
        }

        let samples = if channels <= 1 {
            interleaved
        } else {
            downmix_to_mono(&interleaved, channels)
        };

        let sample_start = wave_data.len() as i32;
        let sample_len = samples.len() as i32;

        // SF3 shdr loop points are relative to the sample start; SF2 uses
        // absolute frame offsets, so translate them and clamp into [start, end].
        let start_loop = header.start_loop;
        let end_loop = header.end_loop;
        header.start = sample_start;
        header.end = sample_start + sample_len;
        header.start_loop = (sample_start + start_loop).clamp(sample_start, header.end);
        header.end_loop = (sample_start + end_loop).clamp(sample_start, header.end);

        wave_data.extend_from_slice(&samples);
        wave_data.resize(wave_data.len() + SAMPLE_PADDING_FRAMES, 0);
    }

    Ok(wave_data)
}

#[cfg(feature = "sf3")]
fn downmix_to_mono(interleaved: &[i16], channels: usize) -> Vec<i16> {
    let mut mono = Vec::with_capacity(interleaved.len() / channels);
    for frame in interleaved.chunks_exact(channels) {
        let sum: i32 = frame.iter().map(|&sample| sample as i32).sum();
        mono.push((sum / channels as i32) as i16);
    }
    mono
}

#[cfg(feature = "sf3")]
fn sf3_error(error: lewton::VorbisError) -> SoundFontError {
    SoundFontError::IoError(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        error.to_string(),
    ))
}
