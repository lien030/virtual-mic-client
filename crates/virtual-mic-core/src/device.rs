use crate::{AudioFormat, ConvertingWriter};
use std::{
    fmt,
    fs::{File, OpenOptions},
    io,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    time::{Duration, Instant},
};
use windows::Win32::{Foundation::HANDLE, System::IO::DeviceIoControl};

pub type Result<T> = std::result::Result<T, Error>;
const VERSION: u32 = 1;
const STATUS: u32 = (0x8000 << 16) | (1 << 14) | (0x800 << 2);
const WRITE: u32 = (0x8000 << 16) | (2 << 14) | (0x801 << 2);
const CLEAR: u32 = (0x8000 << 16) | (2 << 14) | (0x802 << 2);
const MAX_PACKET_BYTES: usize = 16384;
const WRITE_HEADER_BYTES: usize = 24;

#[derive(Debug)]
pub enum Error {
    Os(io::Error),
    Windows(windows::core::Error),
    UnsupportedFormat(AudioFormat),
    FormatMismatch {
        expected: AudioFormat,
        provided: AudioFormat,
    },
    UnalignedData,
    NotCapturing,
    WouldBlock,
    StreamChanged,
    ProtocolMismatch,
    Timeout,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Os(e) => write!(f, "{e}"),
            Self::Windows(e) => write!(f, "{e}"),
            Self::UnsupportedFormat(format) => write!(f, "unsupported format: {format}"),
            Self::FormatMismatch { expected, provided } => {
                write!(f, "capture requires {expected}; supplied {provided}")
            }
            Self::UnalignedData => f.write_str("PCM data must contain complete frames"),
            Self::NotCapturing => f.write_str("waiting for an application to capture Virtual Mic"),
            Self::WouldBlock => f.write_str("audio buffer is full; retry the unconsumed input"),
            Self::StreamChanged => f.write_str("capture format or stream generation changed"),
            Self::ProtocolMismatch => f.write_str("incompatible Virtual Mic driver protocol"),
            Self::Timeout => f.write_str("timed out draining audio"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Os(e)
    }
}
impl From<windows::core::Error> for Error {
    fn from(e: windows::core::Error) -> Self {
        Self::Windows(e)
    }
}

/// A consistent snapshot of the driver. Frame counters are cumulative across stream restarts.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct Status {
    version: u32,
    size: u32,
    pub generation: u64,
    pub format: AudioFormat,
    capturing: u32,
    pub queued_frames: u32,
    pub capacity_frames: u32,
    pub max_packet_bytes: u32,
    pub written_frames: u64,
    pub read_frames: u64,
    pub underrun_frames: u64,
}

impl Status {
    pub fn is_capturing(self) -> bool {
        self.capturing != 0
    }
    pub fn queued_duration(self) -> Duration {
        Duration::from_secs_f64(self.queued_frames as f64 / self.format.sample_rate as f64)
    }
}

/// Owns the driver's single writer connection. Dropping it clears queued audio.
pub struct VirtualMic {
    file: File,
}

impl VirtualMic {
    /// Opens the single writer connection and checks the driver protocol.
    pub fn open() -> Result<Self> {
        let mic = Self {
            file: OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(3)
                .open(r"\\.\VirtualMic")?,
        };
        mic.status()?;
        Ok(mic)
    }

    /// Queries status without taking the writer connection.
    pub fn inspect() -> Result<Status> {
        Self {
            file: OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(r"\\.\VirtualMic")?,
        }
        .status()
    }

    /// Returns the current capture format, stream generation, queue, and frame counters.
    pub fn status(&self) -> Result<Status> {
        let mut status = Status::default();
        let output = unsafe {
            std::slice::from_raw_parts_mut(
                (&mut status as *mut Status).cast::<u8>(),
                size_of::<Status>(),
            )
        };
        let returned = self.ioctl(STATUS, &[], output)?;
        if returned != size_of::<Status>()
            || status.version != VERSION
            || status.size as usize != size_of::<Status>()
            || status.max_packet_bytes == 0
            || status.max_packet_bytes as usize > MAX_PACKET_BYTES
            || status.format.validate().is_err()
            || status.queued_frames > status.capacity_frames
        {
            return Err(Error::ProtocolMismatch);
        }
        Ok(status)
    }

    /// Returns input frames accepted, possibly fewer than supplied. Never converts samples.
    pub fn write_strict(&mut self, format: AudioFormat, pcm: &[u8]) -> Result<usize> {
        format.validate_pcm(pcm)?;
        if pcm.is_empty() {
            return Ok(0);
        }
        let status = self.status()?;
        if !status.is_capturing() {
            return Err(Error::NotCapturing);
        }
        if status.format != format {
            return Err(Error::FormatMismatch {
                expected: status.format,
                provided: format,
            });
        }
        self.write_for(status, pcm)
    }

    /// Converts input PCM to the active capture format as each packet is written.
    pub fn into_converting(self, input: AudioFormat) -> Result<ConvertingWriter> {
        ConvertingWriter::new(self, input)
    }

    /// Discards audio queued in the driver.
    pub fn clear(&mut self) -> Result<()> {
        self.ioctl(CLEAR, &[], &mut []).map(|_| ())
    }

    /// Waits until the currently queued frames have been consumed.
    pub fn drain(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let generation = self.status()?.generation;
        loop {
            let status = self.status()?;
            if status.generation != generation {
                return Err(Error::StreamChanged);
            }
            if status.queued_frames == 0 {
                return Ok(());
            }
            if !status.is_capturing() {
                return Err(Error::NotCapturing);
            }
            if Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub(crate) fn write_for(&self, status: Status, pcm: &[u8]) -> Result<usize> {
        let frame_bytes = status.format.frame_bytes();
        let limit = (status.max_packet_bytes as usize / frame_bytes) * frame_bytes;
        let bytes = pcm.len().min(limit);
        let mut request = Vec::with_capacity(WRITE_HEADER_BYTES + bytes);
        request.extend_from_slice(&VERSION.to_le_bytes());
        request.extend_from_slice(&(WRITE_HEADER_BYTES as u32).to_le_bytes());
        request.extend_from_slice(&status.generation.to_le_bytes());
        request.extend_from_slice(&status.format.sample_rate.to_le_bytes());
        request.extend_from_slice(&status.format.channels.to_le_bytes());
        request.extend_from_slice(&status.format.bits_per_sample.to_le_bytes());
        request.extend_from_slice(&pcm[..bytes]);
        let mut output = [0u8; 4];
        if self.ioctl(WRITE, &request, &mut output)? != 4 {
            return Err(Error::ProtocolMismatch);
        }
        let frames = u32::from_le_bytes(output) as usize;
        if frames > bytes / frame_bytes {
            return Err(Error::ProtocolMismatch);
        }
        Ok(frames)
    }

    fn ioctl(&self, code: u32, input: &[u8], output: &mut [u8]) -> Result<usize> {
        let mut returned = 0;
        let result = unsafe {
            DeviceIoControl(
                HANDLE(self.file.as_raw_handle()),
                code,
                if input.is_empty() {
                    None
                } else {
                    Some(input.as_ptr().cast())
                },
                input.len() as u32,
                if output.is_empty() {
                    None
                } else {
                    Some(output.as_mut_ptr().cast())
                },
                output.len() as u32,
                Some(&mut returned),
                None,
            )
        };
        match result {
            Ok(()) => Ok(returned as usize),
            Err(error) => Err(match error.code().0 as u32 & 0xffff {
                21 => Error::NotCapturing,
                170 => Error::WouldBlock,
                1237 => Error::StreamChanged,
                1306 => Error::ProtocolMismatch,
                _ => Error::Windows(error),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    #[ignore = "requires the installed Virtual Mic driver"]
    fn installed_driver_validates_protocol_and_writer_lifetime() {
        let mic = VirtualMic::open().unwrap();
        assert!(VirtualMic::open().is_err());
        VirtualMic::inspect().unwrap();
        assert!(
            !mic.status().unwrap().is_capturing(),
            "close other microphone consumers before this test"
        );
        let mut mic = mic;
        assert!(mic.file.read(&mut [0; 1]).is_err());
        assert!(mic.file.write(&[0; 1]).is_err());
        assert!(matches!(
            mic.write_strict(AudioFormat::new(48000, 1, 16), &[0; 2]),
            Err(Error::NotCapturing)
        ));
        let reader = VirtualMic {
            file: OpenOptions::new()
                .read(true)
                .share_mode(3)
                .open(r"\\.\VirtualMic")
                .unwrap(),
        };
        assert!(reader.ioctl(CLEAR, &[], &mut []).is_err());
        assert!(mic.ioctl(STATUS, &[], &mut [0; 1]).is_err());
        assert!(mic.ioctl(WRITE, &[], &mut [0; 4]).is_err());
        let mut header = Vec::new();
        header.extend_from_slice(&99u32.to_le_bytes());
        header.extend_from_slice(&(WRITE_HEADER_BYTES as u32).to_le_bytes());
        header.extend_from_slice(&0u64.to_le_bytes());
        header.extend_from_slice(&48000u32.to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&16u16.to_le_bytes());
        assert!(matches!(
            mic.ioctl(WRITE, &header, &mut [0; 4]),
            Err(Error::ProtocolMismatch)
        ));
        header[..4].copy_from_slice(&VERSION.to_le_bytes());
        header[20..22].copy_from_slice(&0u16.to_le_bytes());
        assert!(mic.ioctl(WRITE, &header, &mut [0; 4]).is_err());
        header[20..22].copy_from_slice(&1u16.to_le_bytes());
        header.push(1);
        assert!(mic.ioctl(WRITE, &header, &mut [0; 4]).is_err());
        header.resize(WRITE_HEADER_BYTES + MAX_PACKET_BYTES + 1, 0);
        assert!(mic.ioctl(WRITE, &header, &mut [0; 4]).is_err());
        drop(mic);
        VirtualMic::open().unwrap();
    }

    #[test]
    fn c_protocol_layout() {
        assert_eq!(size_of::<AudioFormat>(), 8);
        assert_eq!(size_of::<Status>(), 64);
        assert_eq!(std::mem::offset_of!(Status, generation), 8);
        assert_eq!(std::mem::offset_of!(Status, format), 16);
        assert_eq!(std::mem::offset_of!(Status, written_frames), 40);
        assert_eq!(STATUS, 0x80006000);
        assert_eq!(WRITE, 0x8000a004);
        assert_eq!(CLEAR, 0x8000a008);
    }
}
