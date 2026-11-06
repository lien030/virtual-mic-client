# Virtual Mic Client

English | [简体中文](README.zh-CN.md)

A Windows user-mode Rust SDK and CLI for streaming PCM audio into Virtual Mic. Applications use the Core crate to send audio directly to the kernel driver; recording and meeting software receive it through the **Virtual Mic Driver by MTT** microphone endpoint.

```text
Your application -> virtual-mic-core -> Virtual Mic kernel driver -> Meeting software
```

## Workspace

| Component | Purpose |
| --- | --- |
| `crates/virtual-mic-core` | Driver connection, status queries, PCM writes, and automatic audio conversion. |
| `crates/virtual-mic-demo` | CLI examples for sending audio, recording through WASAPI, and checking the complete audio path. |

The kernel driver and its installation scripts live in the separate `Virtual-Audio-Driver` repository. This workspace provides the application-facing client API and demo.

## Requirements

- Windows x64 with Media Foundation. The client has been tested on Windows 11 x64.
- Rust stable with the MSVC toolchain and Visual Studio C++ Build Tools.
- The matching Virtual Mic driver, exposing protocol v1 at `\\.\VirtualMic`, installed on the machine running the client.

Building the kernel driver requires Visual Studio 2022 and the WDK. Its development package uses a test certificate, so installation requires administrator privileges and Windows test signing mode. Client applications can run as an ordinary desktop user.

## Quick start

Build from the workspace root:

```powershell
cargo build --release --workspace
```

Select **Virtual Mic Driver by MTT** in a recording or meeting application and start microphone capture. Then run:

```powershell
.\target\release\virtual-mic-demo.exe status
.\target\release\virtual-mic-demo.exe tone --seconds 10
.\target\release\virtual-mic-demo.exe play speech.wav
```

`tone` and `play` convert the input to the active capture format by default. The sender can also start first: it waits for capture to begin and sends audio at the consumer's pace. Press Ctrl+C to stop and release the connection.

Use `--strict` when the input already matches the driver's active capture format:

```powershell
.\target\release\virtual-mic-demo.exe tone --strict --rate 48000 --bits 16 --channels 1
```

## Audio formats

Both mono and stereo are supported for every combination below:

| Sample type | Sample rates |
| --- | --- |
| Signed 16-bit PCM | 44,100 Hz, 48,000 Hz |
| Signed 24-bit PCM | 96,000 Hz, 192,000 Hz |
| Signed 32-bit PCM | 48,000 Hz |

Samples are interleaved and little-endian. A 24-bit sample occupies exactly three bytes. The default capture format is mono, 16-bit, 48,000 Hz. WAV playback accepts integer PCM in these formats.

Automatic conversion uses the Windows Media Foundation Resampler. It handles sample rates and bit depths, duplicates mono into stereo, and averages stereo into mono. Keep a converting writer on the thread that created it.

The sending application controls gain and mute. The driver's existing Windows topology volume and mute controls are not applied to injected PCM. The driver fills missing audio with silence; `clear()` discards queued audio for an immediate mute.

## Use the Core API

Add a path dependency, adjusting the path to your checkout:

```toml
[dependencies]
virtual-mic-core = { path = "../virtual-mic-client/crates/virtual-mic-core" }
```

For strict writes, query the capture format and submit a packet with that exact format. This example submits 10 ms of silence while a recording application is capturing:

```rust,no_run
use std::time::Duration;
use virtual_mic_core::VirtualMic;

let mut mic = VirtualMic::open()?;
let format = mic.status()?.format;
let frames = format.sample_rate as usize / 100;
let pcm = vec![0u8; frames * format.frame_bytes()];
let accepted_frames = mic.write_strict(format, &pcm)?;
mic.drain(Duration::from_secs(3))?;
# Ok::<(), virtual_mic_core::Error>(())
```

Both write APIs return the number of **input frames consumed**. After a partial write, continue at `consumed_frames * input.frame_bytes()`. The converting writer accepts at most 10 ms of input per call, retains converted output, and keeps the driver queue near 20 ms. The driver itself has a 200 ms ring buffer.

For automatic conversion, keep the writer alive across packets and retry the unconsumed input. This example sends 100 ms of silence, including partial-write handling:

```rust,no_run
use std::{thread, time::Duration};
use virtual_mic_core::{AudioFormat, Error, VirtualMic};

let input = AudioFormat::new(44100, 2, 16);
let mut writer = VirtualMic::open()?.into_converting(input)?;
let pcm = vec![0u8; 4410 * input.frame_bytes()];
let mut offset = 0;

while offset < pcm.len() {
    match writer.write(&pcm[offset..]) {
        Ok(0)
        | Err(Error::WouldBlock)
        | Err(Error::NotCapturing)
        | Err(Error::StreamChanged) => thread::sleep(Duration::from_millis(2)),
        Ok(frames) => offset += frames * input.frame_bytes(),
        Err(error) => return Err(error),
    }
}

writer.finish(Duration::from_secs(3))?;
# Ok::<(), virtual_mic_core::Error>(())
```

Run the retry loop on a worker thread and connect it to your application's cancellation mechanism when waiting for a recording application.

### Connection and stream behavior

- `VirtualMic::open()` owns the driver's single writer connection. Dropping it clears queued audio and allows another writer to connect.
- `VirtualMic::inspect()` queries status without acquiring the writer connection. `mic.status()` and `writer.status()` query an existing connection.
- Status includes the capture format, stream generation, queue occupancy, and cumulative written, read, and underrun frame counts.
- `mic.clear()` empties the driver queue. `writer.clear()` also resets the converter and its pending output.
- `mic.drain(timeout)` waits for the driver queue. `writer.finish(timeout)` flushes the converter, drains the queue, and closes the writer.
- Capture restarts change the stream generation and discard old audio. The converting writer adapts to the new format on the next write.

| Error | Caller action |
| --- | --- |
| `NotCapturing` | Wait for a recording application to start capture. |
| `WouldBlock` | Wait briefly, then retry the unconsumed input. |
| `FormatMismatch` | In strict mode, supply the active capture format or use automatic conversion. |
| `StreamChanged` | Recheck the capture format; the converting writer resets on the next write. |
| `Timeout` | Draining did not complete within the supplied timeout. |
| `ProtocolMismatch` | Install the driver version matching this client. |

## CLI and audio checks

To record the injected audio, start capture in one terminal and send audio from another:

```powershell
.\target\release\virtual-mic-demo.exe capture recorded.wav --shared --seconds 15
```

```powershell
.\target\release\virtual-mic-demo.exe play speech.wav
```

`capture` uses exclusive mode by default. `--shared` uses the Windows audio engine, as meeting applications typically do. Exclusive capture accepts `--rate`, `--bits`, and `--channels`; `play` reads the input format from the WAV file. Run `.\target\release\virtual-mic-demo.exe --help` for command syntax.

With the driver installed, run the audio checks:

```powershell
# 10 strict writes, one for each capture format.
.\target\release\virtual-mic-demo.exe selftest

# 10 strict cases plus all 100 input-to-capture conversion pairs.
.\target\release\virtual-mic-demo.exe selftest --matrix

# 10 input formats through shared capture at the default mono 48 kHz/16-bit setting.
.\target\release\virtual-mic-demo.exe selftest --shared
```

Close other Virtual Mic senders and capture applications before running the checks. Each case captures injected test tones through WASAPI and checks the expected frequency in each channel. Strict cases also check format rejection, the single-writer rule, a full buffer, and queue clearing.

## Development

```powershell
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
```

The regular tests run without an installed driver. They check the protocol layout, PCM handling, and all 100 streaming conversion pairs. The two Core examples above are compiled as documentation tests.

On a machine with the driver installed, close other Virtual Mic applications and run the protocol test separately:

```powershell
cargo test -p virtual-mic-core installed_driver -- --ignored
```

The driver repository provides `tools/Build-VirtualMic.ps1 -Configuration Release -TestSign` for driver builds and `tools/Install-VirtualMic.ps1` for installation.

## License

MIT. See the `LICENSE` file in the repository root.
