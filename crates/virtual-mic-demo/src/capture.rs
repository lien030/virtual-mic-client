use crate::{AppResult, STOP, audio};
use std::{
    ptr::null_mut,
    time::{Duration, Instant},
};
use virtual_mic_core::AudioFormat;
use windows::{
    Win32::{
        Foundation::PROPERTYKEY,
        Media::{Audio::*, MediaFoundation::MFAudioFormat_PCM},
        System::{
            Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc},
            Com::*,
        },
    },
    core::GUID,
};

struct Com;
impl Com {
    fn new() -> AppResult<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub struct Capture {
    reader: IAudioCaptureClient,
    client: IAudioClient,
    format: AudioFormat,
    _com: Com,
}

impl Capture {
    pub fn open(format: AudioFormat, shared: bool) -> AppResult<Self> {
        format.validate()?;
        let com = Com::new()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)?;
            let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
            let name_key = PROPERTYKEY {
                fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
                pid: 14,
            };
            let mut selected = None;
            for index in 0..devices.GetCount()? {
                let device = devices.Item(index)?;
                let properties = device.OpenPropertyStore(STGM_READ)?;
                let mut value = properties.GetValue(&name_key)?;
                let text = PropVariantToStringAlloc(&value);
                PropVariantClear(&mut value)?;
                let text = text?;
                let name = text.to_string();
                CoTaskMemFree(Some(text.0.cast()));
                if name?.contains("Virtual Mic") {
                    if selected.is_some() {
                        return Err("multiple Virtual Mic endpoints found".into());
                    }
                    selected = Some(device);
                }
            }
            let device = selected.ok_or("Virtual Mic capture endpoint not found")?;
            let client: IAudioClient = device.Activate(CLSCTX_INPROC_SERVER, None)?;
            let wave = WAVEFORMATEXTENSIBLE {
                Format: WAVEFORMATEX {
                    wFormatTag: 0xfffe,
                    nChannels: format.channels,
                    nSamplesPerSec: format.sample_rate,
                    nAvgBytesPerSec: format.sample_rate * format.frame_bytes() as u32,
                    nBlockAlign: format.frame_bytes() as u16,
                    wBitsPerSample: format.bits_per_sample,
                    cbSize: 22,
                },
                Samples: WAVEFORMATEXTENSIBLE_0 {
                    wValidBitsPerSample: format.bits_per_sample,
                },
                dwChannelMask: if format.channels == 1 { 4 } else { 3 },
                SubFormat: MFAudioFormat_PCM,
            };
            let mode = if shared {
                AUDCLNT_SHAREMODE_SHARED
            } else {
                AUDCLNT_SHAREMODE_EXCLUSIVE
            };
            let flags = if shared {
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
            } else {
                0
            };
            client.Initialize(
                mode,
                flags,
                1_000_000,
                0,
                std::ptr::addr_of!(wave.Format),
                None,
            )?;
            let reader: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok(Self {
                reader,
                client,
                format,
                _com: com,
            })
        }
    }

    pub fn read(&self) -> AppResult<Vec<u8>> {
        let mut data = Vec::new();
        unsafe {
            while self.reader.GetNextPacketSize()? > 0 {
                let mut pointer = null_mut();
                let mut frames = 0;
                let mut flags = 0;
                self.reader
                    .GetBuffer(&mut pointer, &mut frames, &mut flags, None, None)?;
                if frames == 0 {
                    break;
                }
                let bytes = frames as usize * self.format.frame_bytes();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                    data.resize(data.len() + bytes, 0);
                } else {
                    data.extend_from_slice(std::slice::from_raw_parts(pointer, bytes));
                }
                self.reader.ReleaseBuffer(frames)?;
            }
        }
        Ok(data)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

pub fn record(path: &str, format: AudioFormat, seconds: f64, shared: bool) -> AppResult<()> {
    let capture = Capture::open(format, shared)?;
    let mut wav = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: format.channels,
            sample_rate: format.sample_rate,
            bits_per_sample: format.bits_per_sample,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    println!("Capturing Virtual Mic: {format} -> {path}");
    let deadline = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < deadline && !STOP.load(std::sync::atomic::Ordering::Relaxed) {
        let data = capture.read()?;
        for sample in data.chunks_exact(format.bits_per_sample as usize / 8) {
            let value = match format.bits_per_sample {
                16 => i16::from_le_bytes(sample.try_into()?) as i32,
                24 => i32::from_le_bytes([0, sample[0], sample[1], sample[2]]) >> 8,
                32 => i32::from_le_bytes(sample.try_into()?),
                _ => unreachable!(),
            };
            wav.write_sample(value)?;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    wav.finalize()?;
    Ok(())
}

pub fn signal_amplitude(data: &[u8], format: AudioFormat, channel: usize, frequency: f64) -> f64 {
    let sample_bytes = format.bits_per_sample as usize / 8;
    let mut real = 0.0;
    let mut imaginary = 0.0;
    let mut count = 0;
    for (i, frame) in data.chunks_exact(format.frame_bytes()).enumerate() {
        let value = audio::decode(
            &frame[channel * sample_bytes..(channel + 1) * sample_bytes],
            format.bits_per_sample,
        );
        let phase = i as f64 * frequency * std::f64::consts::TAU / format.sample_rate as f64;
        real += value * phase.cos();
        imaginary += value * phase.sin();
        count += 1;
    }
    2.0 * real.hypot(imaginary) / count.max(1) as f64
}
