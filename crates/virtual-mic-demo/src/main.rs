mod audio;
mod capture;
mod selftest;

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use virtual_mic_core::{AudioFormat, Error, VirtualMic};
use windows::{Win32::System::Console::SetConsoleCtrlHandler, core::BOOL};

type AppResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn ctrl_handler(kind: u32) -> BOOL {
    if kind <= 1 {
        STOP.store(true, Ordering::Relaxed);
        BOOL(1)
    } else {
        BOOL(0)
    }
}

struct Options {
    command: String,
    file: Option<String>,
    format: AudioFormat,
    seconds: Option<f64>,
    strict: bool,
    shared: bool,
    matrix: bool,
}

fn options() -> AppResult<Options> {
    let mut args = std::env::args().skip(1);
    let mut options = Options {
        command: args.next().unwrap_or_else(|| "help".into()),
        file: None,
        format: AudioFormat::new(48000, 1, 16),
        seconds: None,
        strict: false,
        shared: false,
        matrix: false,
    };
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--rate" => {
                options.format.sample_rate =
                    args.next().ok_or("--rate requires a value")?.parse()?
            }
            "--bits" => {
                options.format.bits_per_sample =
                    args.next().ok_or("--bits requires a value")?.parse()?
            }
            "--channels" => {
                options.format.channels =
                    args.next().ok_or("--channels requires a value")?.parse()?
            }
            "--seconds" => {
                options.seconds = Some(args.next().ok_or("--seconds requires a value")?.parse()?)
            }
            "--strict" => options.strict = true,
            "--shared" => options.shared = true,
            "--matrix" => options.matrix = true,
            _ if !argument.starts_with('-') && options.file.is_none() => {
                options.file = Some(argument)
            }
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }
    options.format.validate()?;
    if options
        .seconds
        .is_some_and(|seconds| !seconds.is_finite() || seconds <= 0.0 || seconds > 86400.0)
    {
        return Err("--seconds must be in (0, 86400]".into());
    }
    Ok(options)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}

fn run() -> AppResult<()> {
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_handler), true)?;
    }
    let options = options()?;
    match options.command.as_str() {
        "status" => {
            println!("{:#?}", VirtualMic::inspect()?);
        }
        "tone" => {
            let mut frame = 0;
            let total =
                (options.seconds.unwrap_or(10.0) * options.format.sample_rate as f64) as u64;
            transmit(options.format, options.strict, || {
                if frame >= total {
                    return Ok(None);
                }
                let count = (total - frame).min((options.format.sample_rate / 100) as u64) as usize;
                let data = audio::tone(options.format, frame, count);
                frame += count as u64;
                Ok(Some(data))
            })?;
        }
        "play" => {
            let path = options.file.ok_or("play requires a PCM WAV file")?;
            let mut reader = hound::WavReader::open(path)?;
            let spec = reader.spec();
            if spec.sample_format != hound::SampleFormat::Int {
                return Err("WAV must contain signed integer PCM".into());
            }
            let format = AudioFormat::new(spec.sample_rate, spec.channels, spec.bits_per_sample);
            format.validate()?;
            let mut samples = reader.samples::<i32>();
            transmit(format, options.strict, || {
                let mut data = Vec::new();
                for _ in 0..format.sample_rate / 100 * format.channels as u32 {
                    let Some(sample) = samples.next() else {
                        break;
                    };
                    data.extend_from_slice(
                        &sample?.to_le_bytes()[..format.bits_per_sample as usize / 8],
                    );
                }
                Ok(if data.is_empty() { None } else { Some(data) })
            })?;
        }
        "capture" => capture::record(
            options
                .file
                .as_deref()
                .ok_or("capture requires an output WAV path")?,
            options.format,
            options.seconds.unwrap_or(10.0),
            options.shared,
        )?,
        "selftest" => selftest::run(
            options.matrix,
            options.shared,
            options.seconds.unwrap_or(1.0),
        )?,
        "help" | "--help" | "-h" => println!(
            "virtual-mic-demo status\n\
            virtual-mic-demo tone [--strict] [--rate 48000 --bits 16 --channels 1] [--seconds 10]\n\
            virtual-mic-demo play audio.wav [--strict]\n\
            virtual-mic-demo capture output.wav [--shared] [--rate 48000 --bits 16 --channels 1] [--seconds 10]\n\
            virtual-mic-demo selftest [--matrix | --shared] [--seconds 1]\n\n\
            Start recording Virtual Mic in your meeting application before sending audio. Ctrl+C stops the demo."
        ),
        _ => return Err(format!("unknown command: {}", options.command).into()),
    }
    Ok(())
}

fn transmit(
    format: AudioFormat,
    strict: bool,
    mut next: impl FnMut() -> AppResult<Option<Vec<u8>>>,
) -> AppResult<()> {
    let mic = VirtualMic::open()?;
    println!(
        "Connected. Input: {format}. Mode: {}",
        if strict { "strict" } else { "convert" }
    );
    let mut sink = if strict {
        Sink::Strict(mic)
    } else {
        Sink::Converting(mic.into_converting(format)?)
    };
    let mut previous = None;
    while !STOP.load(Ordering::Relaxed) {
        let Some(data) = next()? else {
            break;
        };
        let mut offset = 0;
        while offset < data.len() && !STOP.load(Ordering::Relaxed) {
            let status = sink.status()?;
            let state = (status.generation, status.is_capturing());
            if previous != Some(state) {
                if status.is_capturing() {
                    println!(
                        "Capturing: {} (generation {})",
                        status.format, status.generation
                    );
                } else {
                    println!("Waiting for Virtual Mic capture; Ctrl+C to stop.");
                }
                previous = Some(state);
            }
            if !status.is_capturing() || status.queued_duration() >= Duration::from_millis(20) {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            match sink.write(format, &data[offset..]) {
                Ok(0)
                | Err(Error::WouldBlock)
                | Err(Error::NotCapturing)
                | Err(Error::StreamChanged) => std::thread::sleep(Duration::from_millis(2)),
                Ok(frames) => offset += frames * format.frame_bytes(),
                Err(error) => return Err(error.into()),
            }
        }
    }
    if !STOP.load(Ordering::Relaxed) {
        sink.finish(Duration::from_secs(3))?;
    }
    println!("Finished.");
    Ok(())
}

pub(crate) enum Sink {
    Strict(VirtualMic),
    Converting(virtual_mic_core::ConvertingWriter),
}
impl Sink {
    fn status(&self) -> virtual_mic_core::Result<virtual_mic_core::Status> {
        match self {
            Self::Strict(mic) => mic.status(),
            Self::Converting(writer) => writer.status(),
        }
    }
    fn write(&mut self, format: AudioFormat, data: &[u8]) -> virtual_mic_core::Result<usize> {
        match self {
            Self::Strict(mic) => mic.write_strict(format, data),
            Self::Converting(writer) => writer.write(data),
        }
    }
    fn finish(self, timeout: Duration) -> virtual_mic_core::Result<()> {
        match self {
            Self::Strict(mic) => mic.drain(timeout),
            Self::Converting(writer) => writer.finish(timeout),
        }
    }
}
