# Virtual Mic Rust API

Rust 应用直接向 Virtual Mic 驱动写入音频。会议软件选择 **Virtual Mic Driver by MTT** 后，通过标准 Windows 麦克风采集接口接收这些声音。

```text
应用 → virtual-mic-core → DeviceIoControl → Virtual Mic → 会议软件
```

## 项目

- `crates/virtual-mic-core`：连接驱动、查询状态、严格写入、自动格式转换。
- `crates/virtual-mic-demo`：调用 Core 的命令行示例，以及 WASAPI 采集验证工具。
- 驱动源码位于独立的 `Virtual-Audio-Driver` 仓库；构建工具链为 Windows x64、VS 2022、WDK 和 Rust MSVC。

支持单声道和双声道的以下整数 PCM 格式：

| 位深 | 采样率 |
|---|---|
| 16-bit | 44,100 Hz、48,000 Hz |
| 24-bit | 96,000 Hz、192,000 Hz |
| 32-bit | 48,000 Hz |

PCM 数据为交错、小端、有符号整数；24-bit 每个样本占 3 字节。默认采集格式为单声道 16-bit/48,000 Hz。32-bit 使用整数，WAV 文件也需要整数 PCM。

音量和静音由发送应用处理；当前注入路径不应用驱动原有的 Windows 拓扑音量/静音节点。停止写入后驱动补静音，`clear()` 丢弃排队音频。

## 构建和运行

```powershell
cargo build --release --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

在测试 Windows 中安装驱动后，先在会议软件或录音软件中选择 Virtual Mic 并开始采集，再运行：

```powershell
.\target\release\virtual-mic-demo.exe status
.\target\release\virtual-mic-demo.exe tone --seconds 10
.\target\release\virtual-mic-demo.exe play speech.wav
.\target\release\virtual-mic-demo.exe tone --strict --rate 48000 --bits 16 --channels 1
```

自动转换为默认模式。`--strict` 要求输入与驱动当前采集格式完全一致。发送程序可以先启动：它会等待采集开始，按音频帧的消费速度发送；Ctrl+C 停止并释放连接。

## Core 接入

业务应用通过路径依赖使用 Core：

```toml
[dependencies]
virtual-mic-core = { path = "../virtual-mic-client/crates/virtual-mic-core" }
```

严格写入：

```rust,no_run
use virtual_mic_core::{AudioFormat, VirtualMic};

let format = AudioFormat::new(48000, 1, 16);
let mut mic = VirtualMic::open()?;
let status = mic.status()?;
let pcm = vec![0u8; 480 * format.frame_bytes()];
let accepted_frames = mic.write_strict(format, &pcm)?;
# Ok::<(), virtual_mic_core::Error>(())
```

自动转换：

```rust,no_run
use virtual_mic_core::{AudioFormat, VirtualMic};
use std::time::Duration;

let input = AudioFormat::new(44100, 2, 16);
let mut writer = VirtualMic::open()?.into_converting(input)?;
let pcm = vec![0u8; 441 * input.frame_bytes()];
let consumed_input_frames = writer.write(&pcm)?;
writer.finish(Duration::from_secs(3))?;
# Ok::<(), virtual_mic_core::Error>(())
```

两种写入接口均返回**输入帧数**。部分写入后，从 `frames * input.frame_bytes()` 继续提交。`WouldBlock` 时等待后重试剩余数据；`NotCapturing` 时等待录音端启动。自动写入器每次最多接收 10 ms 输入，保留转换状态和待发送输出，按约 20 ms 队列深度发送。

自动转换使用 Windows Media Foundation Resampler，支持重采样、位深转换，以及单声道复制为双声道、双声道取平均转单声道。转换对象应在同一线程使用。采集代次变化时，旧代次待发送数据被丢弃，下一次写入会适配新的采集格式。

`status()` 返回当前格式、采集代次、队列占用、累计写入/读取帧数和欠载补静音帧数。`VirtualMic::inspect()` 可在另一个写入程序运行时查询状态。`clear()` 丢弃排队音频；`finish()` 排空自动转换器和驱动队列。关闭连接或应用退出时，驱动清空队列并释放单写入端占用。

## 在测试机验证

先用一个进程打开麦克风并录制，再用另一个进程发送音频：

```powershell
.\virtual-mic-demo.exe capture recorded.wav --shared --seconds 15
.\virtual-mic-demo.exe play speech.wav
```

自动端到端验证：

```powershell
# 十种采集格式逐项测试严格接口，同时检查单写入端限制。
.\virtual-mic-demo.exe selftest

# 再增加十种输入到十种采集格式的转换，共 110 个采集用例。
.\virtual-mic-demo.exe selftest --matrix

# Windows 音频引擎的共享采集：十种输入格式均送入默认单声道 48 kHz 端点。
.\virtual-mic-demo.exe selftest --shared
```

独占自测逐项选择驱动格式，需要关闭其他正在使用 Virtual Mic 的程序。它录制由 Core 注入的测试音，检查声道对应的频率成分，同时检查严格格式校验、缓冲写满与清空。`selftest --shared` 验证会议软件通常使用的共享采集路径，`capture --shared` 可保存该路径的 WAV 录音。

Core 的常规单元测试在开发机运行，包含全部 100 种格式转换组合；已安装驱动的协议校验测试可单独运行：

```powershell
cargo test -p virtual-mic-core installed_driver -- --ignored
```

运行协议测试前关闭其他 Virtual Mic 采集和发送程序。驱动构建和测试签名使用驱动仓库中的 `tools/Build-VirtualMic.ps1 -Configuration Release -TestSign`，安装使用 `tools/Install-VirtualMic.ps1`，需要管理员权限和 Windows 测试签名模式。
