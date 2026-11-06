use crate::{AudioFormat, Result};
use std::{marker::PhantomData, mem::ManuallyDrop, rc::Rc};
use windows::{
    Win32::{Media::MediaFoundation::*, System::Com::*},
    core::Interface,
};

struct Runtime {
    cookie: CO_MTA_USAGE_COOKIE,
    _thread: PhantomData<Rc<()>>,
}

impl Runtime {
    fn new() -> Result<Self> {
        unsafe {
            let cookie = CoIncrementMTAUsage()?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoDecrementMTAUsage(cookie)?;
                return Err(error.into());
            }
            Ok(Self {
                cookie,
                _thread: PhantomData,
            })
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            let _ = CoDecrementMTAUsage(self.cookie);
        }
    }
}

pub(crate) struct Resampler {
    transform: IMFTransform,
    _runtime: Runtime,
    input: AudioFormat,
    output: AudioFormat,
    input_frames: u64,
}

impl Resampler {
    pub fn new(input: AudioFormat, output: AudioFormat) -> Result<Self> {
        input.validate()?;
        output.validate()?;
        let runtime = Runtime::new()?;
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CResamplerMediaObject, None, CLSCTX_INPROC_SERVER)?;
            transform.SetInputType(0, &media_type(input)?, 0)?;
            transform.SetOutputType(0, &media_type(output)?, 0)?;
            let properties: IWMResamplerProps = transform.cast()?;
            properties.SetHalfFilterLength(30)?;
            if input.channels != output.channels {
                let mut matrix = if input.channels == 1 {
                    [1.0, 1.0]
                } else {
                    [0.5, 0.5]
                };
                properties.SetUserChannelMtx(matrix.as_mut_ptr())?;
            }
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(Self {
                transform,
                _runtime: runtime,
                input,
                output,
                input_frames: 0,
            })
        }
    }

    pub fn push(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        self.input.validate_pcm(data)?;
        unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut pointer = std::ptr::null_mut();
            buffer.Lock(&mut pointer, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            let frames = (data.len() / self.input.frame_bytes()) as u64;
            sample.SetSampleTime(
                (self.input_frames * 10_000_000 / self.input.sample_rate as u64) as i64,
            )?;
            sample
                .SetSampleDuration((frames * 10_000_000 / self.input.sample_rate as u64) as i64)?;
            self.transform.ProcessInput(0, &sample, 0)?;
            self.input_frames += frames;
        }
        self.receive()
    }

    pub fn finish(&mut self) -> Result<Vec<u8>> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)?;
        }
        self.receive()
    }

    fn receive(&mut self) -> Result<Vec<u8>> {
        let mut result = Vec::new();
        unsafe {
            let info = self.transform.GetOutputStreamInfo(0)?;
            let capacity = info
                .cbSize
                .max(self.output.sample_rate / 10 * self.output.frame_bytes() as u32);
            loop {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(capacity)?)?;
                let mut output = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(sample)),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let processed = self.transform.ProcessOutput(0, &mut output, &mut status);
                let sample = ManuallyDrop::take(&mut output[0].pSample);
                ManuallyDrop::drop(&mut output[0].pEvents);
                if let Err(error) = processed {
                    if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                        break;
                    }
                    return Err(error.into());
                }
                let buffer = sample
                    .expect("resampler output sample")
                    .ConvertToContiguousBuffer()?;
                let mut pointer = std::ptr::null_mut();
                let mut length = 0;
                buffer.Lock(&mut pointer, None, Some(&mut length))?;
                if length > 0 {
                    result.extend_from_slice(std::slice::from_raw_parts(pointer, length as usize));
                }
                buffer.Unlock()?;
                if length == 0 {
                    break;
                }
            }
        }
        self.output.validate_pcm(&result)?;
        Ok(result)
    }
}

unsafe fn media_type(format: AudioFormat) -> Result<IMFMediaType> {
    unsafe {
        let media = MFCreateMediaType()?;
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
        media.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
        media.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, format.channels as u32)?;
        media.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, format.sample_rate)?;
        media.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, format.bits_per_sample as u32)?;
        media.SetUINT32(
            &MF_MT_AUDIO_VALID_BITS_PER_SAMPLE,
            format.bits_per_sample as u32,
        )?;
        media.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, format.frame_bytes() as u32)?;
        media.SetUINT32(
            &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
            format.sample_rate * format.frame_bytes() as u32,
        )?;
        media.SetUINT32(
            &MF_MT_AUDIO_CHANNEL_MASK,
            if format.channels == 1 { 4 } else { 3 },
        )?;
        media.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        Ok(media)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SUPPORTED_FORMATS;

    fn signal(format: AudioFormat, frames: usize) -> Vec<u8> {
        let mut pcm = Vec::new();
        for i in 0..frames {
            let value = ((i as f64 * 1000.0 * std::f64::consts::TAU / format.sample_rate as f64)
                .sin()
                * 0.25
                * i32::MAX as f64) as i32;
            let bytes = value.to_le_bytes();
            for _ in 0..format.channels {
                pcm.extend_from_slice(&bytes[4 - format.bits_per_sample as usize / 8..]);
            }
        }
        pcm
    }

    #[test]
    fn every_format_pair_streams_and_drains() {
        for input in SUPPORTED_FORMATS {
            for output in SUPPORTED_FORMATS {
                let mut converter = Resampler::new(input, output).unwrap();
                let data = signal(input, input.sample_rate as usize / 100);
                let mut converted = Vec::new();
                for _ in 0..10 {
                    converted.extend(converter.push(&data).unwrap());
                }
                converted.extend(converter.finish().unwrap());
                let frames = converted.len() / output.frame_bytes();
                let expected = output.sample_rate as usize / 10;
                // Interpolation can omit the final input-sample interval at end of stream.
                let tolerance = output.sample_rate.div_ceil(input.sample_rate) as usize;
                assert!(
                    frames.abs_diff(expected) <= tolerance,
                    "{input} -> {output}: {frames} != {expected}"
                );
                assert!(
                    converted.iter().any(|value| *value != 0),
                    "{input} -> {output}: silence"
                );
                if output.channels == 2 {
                    let sample_bytes = output.bits_per_sample as usize / 8;
                    for frame in converted.chunks_exact(output.frame_bytes()) {
                        assert_eq!(&frame[..sample_bytes], &frame[sample_bytes..]);
                    }
                }
            }
        }
    }
}
