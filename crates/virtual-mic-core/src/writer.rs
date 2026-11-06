use crate::{AudioFormat, Error, Result, Status, VirtualMic, resampler::Resampler};
use std::time::{Duration, Instant};

/// Converts a continuous input stream to the active capture format.
/// Calls consume at most 10 ms of input and retain converted output until accepted by the driver.
pub struct ConvertingWriter {
    mic: VirtualMic,
    input: AudioFormat,
    target: Option<Status>,
    converter: Option<Resampler>,
    pending: Vec<u8>,
    offset: usize,
}

impl ConvertingWriter {
    pub(crate) fn new(mic: VirtualMic, input: AudioFormat) -> Result<Self> {
        input.validate()?;
        Ok(Self {
            mic,
            input,
            target: None,
            converter: None,
            pending: Vec::new(),
            offset: 0,
        })
    }

    pub fn status(&self) -> Result<Status> {
        self.mic.status()
    }
    pub fn input_format(&self) -> AudioFormat {
        self.input
    }

    /// Returns input frames consumed. Retry only the remaining frames after a partial write.
    /// Stream restarts discard audio belonging to the previous capture generation.
    pub fn write(&mut self, pcm: &[u8]) -> Result<usize> {
        self.input.validate_pcm(pcm)?;
        if pcm.is_empty() {
            return Ok(0);
        }
        let status = self.status()?;
        if !status.is_capturing() {
            return Err(Error::NotCapturing);
        }
        if self
            .target
            .is_none_or(|target| target.generation != status.generation)
        {
            self.pending.clear();
            self.offset = 0;
            self.converter = if self.input == status.format {
                None
            } else {
                Some(Resampler::new(self.input, status.format)?)
            };
            self.target = Some(status);
        }
        self.flush_pending()?;
        let status = self.status()?;
        self.check_generation(status)?;
        if status.queued_frames >= status.format.sample_rate / 50 {
            return Err(Error::WouldBlock);
        }
        let frames =
            (pcm.len() / self.input.frame_bytes()).min(self.input.sample_rate as usize / 100);
        let chunk = &pcm[..frames * self.input.frame_bytes()];
        if let Some(converter) = &mut self.converter {
            self.pending = converter.push(chunk)?;
            self.offset = 0;
            Ok(frames)
        } else {
            self.mic.write_for(status, chunk)
        }
    }

    pub fn clear(&mut self) -> Result<()> {
        self.pending.clear();
        self.offset = 0;
        self.converter = None;
        self.target = None;
        self.mic.clear()
    }

    /// Drains the converter and driver before closing the writer connection.
    pub fn finish(mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        self.wait_pending(deadline)?;
        if let Some(converter) = &mut self.converter {
            self.pending = converter.finish()?;
            self.offset = 0;
            self.wait_pending(deadline)?;
        }
        self.mic
            .drain(deadline.saturating_duration_since(Instant::now()))
    }

    fn check_generation(&self, status: Status) -> Result<()> {
        if !status.is_capturing() {
            return Err(Error::NotCapturing);
        }
        if self
            .target
            .is_some_and(|target| target.generation != status.generation)
        {
            return Err(Error::StreamChanged);
        }
        Ok(())
    }

    fn flush_pending(&mut self) -> Result<()> {
        while self.offset < self.pending.len() {
            let status = self.status()?;
            self.check_generation(status)?;
            let available =
                (status.format.sample_rate / 50).saturating_sub(status.queued_frames) as usize;
            if available == 0 {
                return Err(Error::WouldBlock);
            }
            let end = self
                .pending
                .len()
                .min(self.offset + available * status.format.frame_bytes());
            let accepted = self
                .mic
                .write_for(status, &self.pending[self.offset..end])?;
            if accepted == 0 {
                return Err(Error::WouldBlock);
            }
            self.offset += accepted * status.format.frame_bytes();
        }
        self.pending.clear();
        self.offset = 0;
        Ok(())
    }

    fn wait_pending(&mut self, deadline: Instant) -> Result<()> {
        loop {
            match self.flush_pending() {
                Ok(()) => return Ok(()),
                Err(Error::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                Err(Error::WouldBlock) => return Err(Error::Timeout),
                Err(error) => return Err(error),
            }
        }
    }
}
