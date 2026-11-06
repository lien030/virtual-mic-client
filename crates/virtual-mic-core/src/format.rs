use crate::{Error, Result};

/// Interleaved, signed little-endian PCM; 24-bit samples occupy exactly three bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
}

pub const SUPPORTED_FORMATS: [AudioFormat; 10] = [
    AudioFormat::new(48000, 1, 16),
    AudioFormat::new(44100, 1, 16),
    AudioFormat::new(96000, 1, 24),
    AudioFormat::new(192000, 1, 24),
    AudioFormat::new(48000, 1, 32),
    AudioFormat::new(48000, 2, 16),
    AudioFormat::new(44100, 2, 16),
    AudioFormat::new(96000, 2, 24),
    AudioFormat::new(192000, 2, 24),
    AudioFormat::new(48000, 2, 32),
];

impl AudioFormat {
    pub const fn new(sample_rate: u32, channels: u16, bits_per_sample: u16) -> Self {
        Self {
            sample_rate,
            channels,
            bits_per_sample,
        }
    }

    pub fn validate(self) -> Result<()> {
        if SUPPORTED_FORMATS.contains(&self) {
            Ok(())
        } else {
            Err(Error::UnsupportedFormat(self))
        }
    }

    pub const fn frame_bytes(self) -> usize {
        self.channels as usize * (self.bits_per_sample as usize / 8)
    }

    pub(crate) fn validate_pcm(self, data: &[u8]) -> Result<()> {
        self.validate()?;
        if !data.len().is_multiple_of(self.frame_bytes()) {
            return Err(Error::UnalignedData);
        }
        Ok(())
    }
}

impl std::fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} Hz / {}-bit PCM / {} ch",
            self.sample_rate, self.bits_per_sample, self.channels
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn format_contract_and_alignment() {
        for format in SUPPORTED_FORMATS {
            format.validate().unwrap();
            assert!(
                format
                    .validate_pcm(&vec![0; format.frame_bytes() * 11])
                    .is_ok()
            );
            assert!(
                format
                    .validate_pcm(&vec![0; format.frame_bytes() * 11 - 1])
                    .is_err()
            );
        }
        for format in [
            AudioFormat::new(48000, 0, 16),
            AudioFormat::new(44100, 2, 32),
            AudioFormat::new(192000, 3, 24),
        ] {
            assert!(format.validate().is_err());
        }
    }
}
