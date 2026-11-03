#![allow(dead_code)]

use std::io::Read;

use crate::binary_reader::BinaryReader;
use crate::error::SoundFontError;
use crate::four_cc::FourCC;
use crate::instrument::Instrument;
use crate::preset::Preset;
use crate::sample_header::SampleHeader;
use crate::soundfont_info::SoundFontInfo;
use crate::soundfont_parameters::SoundFontParameters;
use crate::soundfont_sampledata::SoundFontSampleData;
#[cfg(feature = "sf3")]
use crate::soundfont_sampledata::decode_sf3;

/// Reperesents a SoundFont.
#[derive(Debug)]
#[non_exhaustive]
pub struct SoundFont {
    pub(crate) info: SoundFontInfo,
    pub(crate) bits_per_sample: i32,
    pub(crate) wave_data: Vec<i16>,
    pub(crate) sample_headers: Vec<SampleHeader>,
    pub(crate) presets: Vec<Preset>,
    pub(crate) instruments: Vec<Instrument>,
}

impl SoundFont {
    /// Loads a SoundFont from the stream.
    ///
    /// # Arguments
    ///
    /// * `reader` - The data stream used to load the SoundFont.
    pub fn new<R: Read>(reader: &mut R) -> Result<Self, SoundFontError> {
        let chunk_id = BinaryReader::read_four_cc(reader)?;
        if chunk_id != b"RIFF" {
            return Err(SoundFontError::RiffChunkNotFound);
        }

        let _size = BinaryReader::read_i32(reader)?;

        let form_type = BinaryReader::read_four_cc(reader)?;
        if form_type != b"sfbk" {
            return Err(SoundFontError::InvalidRiffChunkType {
                expected: FourCC::from_bytes(*b"sfbk"),
                actual: form_type,
            });
        }

        let info = SoundFontInfo::new(reader)?;
        let is_sf3 = info.get_version().get_major() == 3;
        let sample_data = SoundFontSampleData::new(reader, is_sf3)?;
        #[cfg(feature = "sf3")]
        let mut parameters = SoundFontParameters::new(reader)?;
        #[cfg(not(feature = "sf3"))]
        let parameters = SoundFontParameters::new(reader)?;

        let (bits_per_sample, wave_data) = match sample_data {
            SoundFontSampleData::Sf2 {
                bits_per_sample,
                wave_data,
            } => (bits_per_sample, wave_data),
            #[cfg(feature = "sf3")]
            SoundFontSampleData::Sf3 { raw_data } => {
                let wave_data = decode_sf3(&raw_data, &mut parameters.sample_headers)?;
                // Instrument regions copied the raw (byte-offset) shdr values when
                // they were built; refresh them so playback and the sanity check
                // below see the decoded frame offsets.
                Self::refresh_region_offsets(&mut parameters.instruments, &parameters.sample_headers);
                (16, wave_data)
            }
        };

        let sound_font = Self {
            info,
            bits_per_sample,
            wave_data,
            sample_headers: parameters.sample_headers,
            presets: parameters.presets,
            instruments: parameters.instruments,
        };

        sound_font.sanity_check()?;

        Ok(sound_font)
    }

    /// Refreshes the sample offsets each instrument region copied from the shdr at
    /// build time, so they reflect decoded frame offsets rather than the raw
    /// byte offsets SF3 stores in the `shdr` chunk.
    #[cfg(feature = "sf3")]
    fn refresh_region_offsets(instruments: &mut [Instrument], sample_headers: &[SampleHeader]) {
        for instrument in instruments {
            for region in &mut instrument.regions {
                if let Some(header) = sample_headers.get(region.get_sample_id()) {
                    region.sample_start = header.start;
                    region.sample_end = header.end;
                    region.sample_start_loop = header.start_loop;
                    region.sample_end_loop = header.end_loop;
                }
            }
        }
    }

    fn sanity_check(&self) -> Result<(), SoundFontError> {
        // https://github.com/sinshu/rustysynth/issues/22
        // https://github.com/sinshu/rustysynth/issues/33
        // https://github.com/sinshu/rustysynth/pull/51
        for instrument in &self.instruments {
            for region in &instrument.regions {
                let start = region.get_sample_start();
                let end = region.get_sample_end();
                let start_loop = region.get_sample_start_loop();
                let end_loop = region.get_sample_end_loop();
                // let loop_mode = region.get_sample_modes();

                if start < 0
                    || start_loop < 0
                    || end as usize >= self.wave_data.len()
                    || end_loop as usize >= self.wave_data.len()
                    || end <= start
                // An invalid loop range no longer rejects the whole file.
                // The oscillator plays such a region once without looping.
                // https://github.com/sinshu/rustysynth/issues/55
                // || (loop_mode != LoopMode::NoLoop && start_loop >= end_loop)
                {
                    return Err(SoundFontError::SanityCheckFailed);
                }
            }
        }

        Ok(())
    }

    /// Gets the information of the SoundFont.
    pub fn get_info(&self) -> &SoundFontInfo {
        &self.info
    }

    /// Gets the bits per sample of the sample data.
    pub fn get_bits_per_sample(&self) -> i32 {
        self.bits_per_sample
    }

    /// Gets the sample data.
    pub fn get_wave_data(&self) -> &[i16] {
        &self.wave_data[..]
    }

    /// Gets the samples of the SoundFont.
    pub fn get_sample_headers(&self) -> &[SampleHeader] {
        &self.sample_headers[..]
    }

    /// Gets the presets of the SoundFont.
    pub fn get_presets(&self) -> &[Preset] {
        &self.presets[..]
    }

    /// Gets the instruments of the SoundFont.
    pub fn get_instruments(&self) -> &[Instrument] {
        &self.instruments[..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{fs::File, path::PathBuf};

    fn samples_dir_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("samples")
    }

    #[cfg(feature = "sf3")]
    #[test]
    fn test_load_sf3() {
        let path = samples_dir_path().join("dummy.sf3");
        let mut file = File::open(&path).unwrap();
        let sound_font = SoundFont::new(&mut file).unwrap();

        let wave_data = sound_font.get_wave_data();
        assert!(!wave_data.is_empty());
        assert!(!sound_font.get_sample_headers().is_empty());
        assert!(!sound_font.get_presets().is_empty());
        assert!(!sound_font.get_instruments().is_empty());

        let wave_data_len = wave_data.len();
        for instrument in sound_font.get_instruments() {
            for region in instrument.get_regions() {
                assert!((region.get_sample_end() as usize) < wave_data_len);
                assert!((region.get_sample_end_loop() as usize) < wave_data_len);
            }
        }
    }

    #[cfg(not(feature = "sf3"))]
    #[test]
    fn test_load_reject_sf3() {
        let path = samples_dir_path().join("dummy.sf3");
        let mut file = File::open(&path).unwrap();
        assert!(matches!(
            SoundFont::new(&mut file),
            Err(SoundFontError::UnsupportedSampleFormat)
        ));
    }

    // smpl sub-chunk exists, but is zero-length.
    #[test]
    fn test_load_empty_samples() {
        let path = samples_dir_path().join("test_empty_samples.sf2");
        let mut file = File::open(&path).unwrap();
        assert!(matches!(
            SoundFont::new(&mut file),
            Err(SoundFontError::SampleDataNotFound)
        ));
    }

    // Regression test for the RIFF pad-byte bug: a subchunk whose `ckSize` is
    // odd is followed by a 1-byte pad (so every chunk starts on an even byte
    // boundary). The parser used to consume exactly `size` bytes and then read
    // the next subchunk id one byte early, yielding `ListContainsUnknownId`
    // (the "?LIS" failure seen with SF3's compressed, odd-sized `smpl`). SF2
    // never tripped it because every SF2 chunk is even-sized, so this builds a
    // minimal in-memory SF2 whose INFO `INAM` is 3 bytes (odd) to exercise the
    // pad-skip while keeping everything else valid.
    #[test]
    fn test_load_sf2_with_odd_sized_info_chunk() {
        let bytes = build_minimal_sf2_with_odd_inam();
        let sound_font = SoundFont::new(&mut std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(sound_font.get_info().get_bank_name(), "abc");
    }

    fn push_u16(buf: &mut Vec<u8>, value: u16) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(buf: &mut Vec<u8>, value: i32) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    fn fixed_string(buf: &mut Vec<u8>, value: &str, len: usize) {
        buf.extend_from_slice(value.as_bytes());
        buf.resize(buf.len() + len - value.len(), 0);
    }

    // A single RIFF subchunk, applying the spec-required pad byte for odd data.
    fn subchunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = Vec::with_capacity(8 + data.len() + 1);
        chunk.extend_from_slice(id);
        chunk.extend_from_slice(&(data.len() as u32).to_le_bytes());
        chunk.extend_from_slice(data);
        if data.len() % 2 == 1 {
            chunk.push(0);
        }
        chunk
    }

    fn list(list_type: &[u8; 4], subchunks: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(list_type);
        for chunk in subchunks {
            body.extend_from_slice(chunk);
        }

        let mut list = Vec::new();
        list.extend_from_slice(b"LIST");
        list.extend_from_slice(&(body.len() as u32).to_le_bytes());
        list.extend_from_slice(&body);
        list
    }

    fn build_minimal_sf2_with_odd_inam() -> Vec<u8> {
        // INFO list: ifil (2.0), an odd-sized INAM (bank name, 3 bytes) with its
        // pad byte, and a trailing isng so the pad-skip is observable.
        let mut ifil = Vec::new();
        push_u16(&mut ifil, 2); // major
        push_u16(&mut ifil, 0); // minor

        let info = list(
            b"INFO",
            &[
                subchunk(b"ifil", &ifil),
                subchunk(b"INAM", b"abc"),
                subchunk(b"isng", b"abcd"),
            ],
        );

        // sdta list: a single smpl chunk with two frames of silence.
        let sdta = list(b"sdta", &[subchunk(b"smpl", &[0, 0, 0, 0])]);

        // pdta list: a minimal but valid preset/instrument/sample table.
        let mut phdr = Vec::new();
        fixed_string(&mut phdr, "Preset", 20);
        push_u16(&mut phdr, 0); // patch
        push_u16(&mut phdr, 0); // bank
        push_u16(&mut phdr, 0); // zone_start_index
        push_i32(&mut phdr, 0); // library
        push_i32(&mut phdr, 0); // genre
        push_i32(&mut phdr, 0); // morphology
        fixed_string(&mut phdr, "EOP", 20); // terminal record
        push_u16(&mut phdr, 0xFFFF); // patch (terminal)
        push_u16(&mut phdr, 0xFFFF); // bank (terminal)
        push_u16(&mut phdr, 1); // zone_start_index (terminal)
        push_i32(&mut phdr, 0);
        push_i32(&mut phdr, 0);
        push_i32(&mut phdr, 0);

        let mut pbag = Vec::new();
        push_u16(&mut pbag, 0); // genNdx
        push_u16(&mut pbag, 0); // modNdx
        push_u16(&mut pbag, 0); // terminal genNdx
        push_u16(&mut pbag, 0); // terminal modNdx

        let pgen = subchunk(b"pgen", &[0, 0, 0, 0]); // one generator terminator

        let mut inst = Vec::new();
        fixed_string(&mut inst, "Instrument", 20);
        push_u16(&mut inst, 0); // zone_start_index
        fixed_string(&mut inst, "EOI", 20); // terminal record
        push_u16(&mut inst, 1); // zone_start_index (terminal)

        let mut ibag = Vec::new();
        push_u16(&mut ibag, 0); // genNdx
        push_u16(&mut ibag, 0); // modNdx
        push_u16(&mut ibag, 0); // terminal genNdx
        push_u16(&mut ibag, 0); // terminal modNdx

        let igen = subchunk(b"igen", &[0, 0, 0, 0]); // one generator terminator

        let mut shdr = Vec::new();
        fixed_string(&mut shdr, "Sample", 20);
        push_i32(&mut shdr, 0); // start
        push_i32(&mut shdr, 1); // end
        push_i32(&mut shdr, 0); // start_loop
        push_i32(&mut shdr, 1); // end_loop
        push_i32(&mut shdr, 44100); // sample_rate
        shdr.push(60); // original_pitch
        shdr.push(0); // pitch_correction
        push_u16(&mut shdr, 0); // link
        push_u16(&mut shdr, 1); // sample_type (mono)
        fixed_string(&mut shdr, "EOS", 20); // terminal record
        shdr.resize(shdr.len() + 26, 0); // 46 - 20 zero bytes

        let pdta = list(
            b"pdta",
            &[
                subchunk(b"phdr", &phdr),
                subchunk(b"pbag", &pbag),
                pgen,
                subchunk(b"inst", &inst),
                subchunk(b"ibag", &ibag),
                igen,
                subchunk(b"shdr", &shdr),
            ],
        );

        // Assemble the RIFF container.
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&((4 + info.len() + sdta.len() + pdta.len()) as u32).to_le_bytes());
        file.extend_from_slice(b"sfbk");
        file.extend_from_slice(&info);
        file.extend_from_slice(&sdta);
        file.extend_from_slice(&pdta);
        file
    }
}
