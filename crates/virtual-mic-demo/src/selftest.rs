use crate::{
    AppResult, STOP, Sink, audio,
    capture::{Capture, signal_amplitude},
};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use virtual_mic_core::{AudioFormat, Error, SUPPORTED_FORMATS, VirtualMic};

pub fn run(matrix: bool, shared: bool, seconds: f64) -> AppResult<()> {
    let mut passed = 0;
    let outputs = if shared {
        &SUPPORTED_FORMATS[..1]
    } else {
        &SUPPORTED_FORMATS[..]
    };
    for &output in outputs {
        if !shared {
            case(output, output, true, false, seconds)?;
            passed += 1;
        }
        if matrix || shared {
            for input in SUPPORTED_FORMATS {
                case(input, output, false, shared, seconds)?;
                passed += 1;
            }
        }
    }
    println!(
        "PASS: {passed} WASAPI {} injection cases",
        if shared { "shared" } else { "exclusive" }
    );
    Ok(())
}

fn case(
    input: AudioFormat,
    output: AudioFormat,
    strict: bool,
    shared: bool,
    seconds: f64,
) -> AppResult<()> {
    if STOP.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("interrupted".into());
    }
    let mut mic = VirtualMic::open()?;
    if VirtualMic::open().is_ok() {
        return Err("driver accepted a second writer".into());
    }
    VirtualMic::inspect()?;
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let recording = std::thread::spawn(move || -> AppResult<Vec<u8>> {
        let capture = match Capture::open(output, shared) {
            Ok(capture) => capture,
            Err(error) => {
                let _ = ready_tx.send(Err(error.to_string()));
                return Err(error);
            }
        };
        ready_tx.send(Ok(()))?;
        let deadline = Instant::now() + Duration::from_secs_f64(seconds + 0.5);
        let mut recorded = Vec::new();
        while Instant::now() < deadline {
            recorded.extend(capture.read()?);
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(recorded)
    });
    let result = (|| -> AppResult<()> {
        ready_rx
            .recv_timeout(Duration::from_secs(10))?
            .map_err(|error| format!("capture {output}: {error}"))?;
        let start = mic.status()?;
        if !start.is_capturing() || start.format != output {
            return Err(format!("incorrect capture status: {start:?}").into());
        }
        if strict {
            let wrong = SUPPORTED_FORMATS
                .into_iter()
                .find(|format| *format != output)
                .unwrap();
            if !matches!(
                mic.write_strict(wrong, &vec![0; wrong.frame_bytes()]),
                Err(Error::FormatMismatch { .. })
            ) {
                return Err("strict writer accepted a different format".into());
            }
            let silence = vec![
                0;
                start.max_packet_bytes as usize / output.frame_bytes()
                    * output.frame_bytes()
            ];
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match mic.write_strict(output, &silence) {
                    Err(Error::WouldBlock) => break,
                    Ok(_) if Instant::now() < deadline => {}
                    other => return Err(format!("buffer-full check: {other:?}").into()),
                }
            }
            mic.clear()?;
            if mic.status()?.queued_frames != 0 {
                return Err("clear did not empty the audio queue".into());
            }
        }
        let base = mic.status()?.written_frames;
        let mut sink = if strict {
            Sink::Strict(mic)
        } else {
            Sink::Converting(mic.into_converting(input)?)
        };
        let mut sent = 0u64;
        let total = (seconds * input.sample_rate as f64) as u64;
        let deadline = Instant::now() + Duration::from_secs_f64(seconds + 0.3);
        while sent < total && Instant::now() < deadline {
            if STOP.load(std::sync::atomic::Ordering::Relaxed) {
                return Err("interrupted".into());
            }
            let count = (total - sent).min((input.sample_rate / 100) as u64) as usize;
            let data = audio::tone(input, sent, count);
            if sink.status()?.queued_duration() >= Duration::from_millis(20) {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            match sink.write(input, &data) {
                Ok(frames) => sent += frames as u64,
                Err(Error::WouldBlock) => std::thread::sleep(Duration::from_millis(2)),
                Err(error) => return Err(error.into()),
            }
        }
        if sent != total {
            return Err(format!("send deadline: {sent}/{total} frames").into());
        }
        // Converted audio must reach the driver without waiting for another write; the resampler keeps under 1 ms.
        let expected = total * output.sample_rate as u64 / input.sample_rate as u64;
        let held = expected.saturating_sub(sink.status()?.written_frames - base);
        if held > output.sample_rate as u64 / 200 {
            return Err(format!("{input} -> {output}: writer held back {held} frames").into());
        }
        sink.finish(Duration::from_secs(1))?;
        Ok(())
    })();
    let recorded = recording.join().map_err(|_| "capture thread panicked")??;
    result?;
    for channel in 0..output.channels as usize {
        let frequencies: &[f64] = if input.channels == 2 && output.channels == 1 {
            &[997.0, 1499.0]
        } else if channel == 1 && input.channels == 2 {
            &[1499.0]
        } else {
            &[997.0]
        };
        for frequency in frequencies {
            let amplitude = signal_amplitude(&recorded, output, channel, *frequency);
            if amplitude < 0.015 {
                return Err(format!("{input} -> {output}: channel {channel}, {frequency} Hz amplitude {amplitude:.5}").into());
            }
        }
    }
    println!(
        "PASS {}: {input} -> {output}",
        if strict { "strict" } else { "convert" }
    );
    Ok(())
}
