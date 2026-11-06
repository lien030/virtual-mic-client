use virtual_mic_core::AudioFormat;

pub fn tone(format: AudioFormat, first_frame: u64, frames: usize) -> Vec<u8> {
    let mut data = Vec::with_capacity(frames * format.frame_bytes());
    for frame in first_frame..first_frame + frames as u64 {
        for channel in 0..format.channels {
            let frequency = if channel == 0 { 997.0 } else { 1499.0 };
            let value = ((frame as f64 * frequency * std::f64::consts::TAU
                / format.sample_rate as f64)
                .sin()
                * 0.2
                * i32::MAX as f64) as i32;
            data.extend_from_slice(&value.to_le_bytes()[4 - format.bits_per_sample as usize / 8..]);
        }
    }
    data
}

pub fn decode(bytes: &[u8], bits: u16) -> f64 {
    match bits {
        16 => i16::from_le_bytes(bytes.try_into().unwrap()) as f64 / 32768.0,
        24 => ((i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]])) >> 8) as f64 / 8388608.0,
        32 => i32::from_le_bytes(bytes.try_into().unwrap()) as f64 / 2147483648.0,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_pcm_encoding_and_tone() {
        assert_eq!(decode(&[0, 128], 16), -1.0);
        assert_eq!(decode(&[0, 0, 128], 24), -1.0);
        assert_eq!(decode(&[0, 0, 0, 128], 32), -1.0);
        for format in virtual_mic_core::SUPPORTED_FORMATS {
            let data = tone(format, 0, 1000);
            assert_eq!(data.len(), format.frame_bytes() * 1000);
            assert!(data.iter().any(|value| *value != 0));
            for sample in data.chunks_exact(format.bits_per_sample as usize / 8) {
                assert!(decode(sample, format.bits_per_sample).abs() <= 0.201);
            }
        }
    }
}
