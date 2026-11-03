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
}
