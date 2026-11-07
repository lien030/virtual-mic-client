# Virtual Mic Client

[English](README.md) | 简体中文

面向 Windows 的用户态 Rust SDK 和命令行工具，可将 PCM 音频写入 Virtual Mic。应用通过 Core crate 直接向内核驱动发送音频，录音或会议软件选择 **Virtual Mic Driver by MTT** 麦克风端点后即可接收这些声音。

```text
自制应用 -> virtual-mic-core -> Virtual Mic 内核驱动 -> 会议软件
```

## 项目结构

| 组件 | 用途 |
| --- | --- |
| `crates/virtual-mic-core` | 连接驱动、查询状态、写入 PCM，以及自动转换音频格式。 |
| `crates/virtual-mic-demo` | 命令行示例，可发送音频、通过 WASAPI 录音并验证完整音频链路。 |

内核驱动及其安装脚本位于独立的 `Virtual-Audio-Driver` 仓库。本项目提供供应用集成的客户端 API 和 Demo。

## 环境要求

- 支持 Media Foundation 的 Windows x64；客户端已在 Windows 11 x64 上测试。
- Rust stable、MSVC 工具链和 Visual Studio C++ Build Tools。
- 在运行客户端的机器上安装配套 Virtual Mic 驱动，通过 `\\.\VirtualMic` 提供 v1 协议接口。

构建内核驱动需要 Visual Studio 2022 和 WDK。开发驱动包使用测试证书，安装需要管理员权限和 Windows 测试签名模式。客户端应用可以使用普通桌面账户运行。

## 快速开始

在项目根目录构建：

```powershell
cargo build --release --workspace
```

构建要给其他机器使用的程序前，先在同一个 PowerShell 窗口运行 `$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=~"`，这样可执行文件中不会包含你的用户目录路径。

在录音或会议软件中选择 **Virtual Mic Driver by MTT** 并开始采集麦克风，然后运行：

```powershell
.\target\release\virtual-mic-demo.exe status
.\target\release\virtual-mic-demo.exe tone --seconds 10
.\target\release\virtual-mic-demo.exe play speech.wav
```

`tone` 和 `play` 默认将输入自动转换为当前采集格式。发送程序也可以先启动：它会等待采集开始，按录音端的消费速度发送。按 Ctrl+C 停止并释放连接。

输入已与驱动当前采集格式完全一致时，可以使用 `--strict`：

```powershell
.\target\release\virtual-mic-demo.exe tone --strict --rate 48000 --bits 16 --channels 1
```

## 音频格式

以下每组格式均支持单声道和双声道：

| 采样格式 | 采样率 |
| --- | --- |
| 有符号 16-bit PCM | 44,100 Hz、48,000 Hz |
| 有符号 24-bit PCM | 96,000 Hz、192,000 Hz |
| 有符号 32-bit PCM | 48,000 Hz |

样本采用交错、小端布局，24-bit 每个样本占 3 字节。默认采集格式为单声道、16-bit、48,000 Hz。WAV 播放接受上述格式的整数 PCM。

自动转换使用 Windows Media Foundation Resampler，支持采样率和位深转换、单声道复制为双声道，以及双声道取平均转为单声道。转换写入器应在创建它的线程上使用。

Windows 会对共享模式采集（会议应用使用的方式）施加 Virtual Mic 的音量和静音；独占模式采集收到原样的注入 PCM。缺少音频时驱动补静音；`clear()` 丢弃排队音频，可用于立即静音。

## 接入 Core API

添加路径依赖，并按实际项目位置调整路径：

```toml
[dependencies]
virtual-mic-core = { path = "../virtual-mic-client/crates/virtual-mic-core" }
```

严格写入时，先查询采集格式，再按相同格式提交数据包。以下示例在录音应用正在采集时提交 10 ms 静音：

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

两种写入 API 都返回**已消费的输入帧数**。部分写入后，从 `consumed_frames * input.frame_bytes()` 处继续提交。自动转换写入器每次最多接受 10 ms 输入，转换结果立即发送，驱动队列最多保留约 30 ms。驱动本身使用 200 ms 环形缓冲区。

自动转换时，应在连续数据包之间保持同一个写入器，并重试尚未消费的输入。以下示例发送 100 ms 静音，包含部分写入处理：

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

在工作线程中运行重试循环；等待录音应用启动时，接入业务应用的取消机制。

### 连接与流行为

- `VirtualMic::open()` 占用驱动唯一的写入连接。释放连接后，驱动清空排队音频，其他写入程序可以连接。
- `VirtualMic::inspect()` 查询状态而不占用写入连接。`mic.status()` 和 `writer.status()` 通过现有连接查询状态。
- 状态包含采集格式、流代次、队列占用，以及累计写入、读取和欠载补静音帧数。
- `mic.clear()` 清空驱动队列。`writer.clear()` 同时重置转换器和待发送输出。
- `mic.drain(timeout)` 等待驱动队列排空。`writer.finish(timeout)` 先排空转换器，再排空队列并关闭写入连接。
- 采集重启会改变流代次并丢弃旧音频。自动转换写入器在下一次写入时适配新的采集格式。

| 错误 | 调用方处理方式 |
| --- | --- |
| `NotCapturing` | 等待录音应用开始采集。 |
| `WouldBlock` | 短暂等待后，重试尚未消费的输入。 |
| `FormatMismatch` | 严格模式下，提供当前采集格式，或改用自动转换。 |
| `StreamChanged` | 重新检查采集格式；自动转换写入器会在下一次写入时重置。 |
| `Timeout` | 在指定超时时间内未完成排空。 |
| `ProtocolMismatch` | 安装与客户端匹配的驱动版本。 |

## 命令行与音频检查

要录制注入的音频，先在一个终端启动采集，再从另一个终端发送：

```powershell
.\target\release\virtual-mic-demo.exe capture recorded.wav --shared --seconds 15
```

```powershell
.\target\release\virtual-mic-demo.exe play speech.wav
```

`capture` 默认使用独占模式。`--shared` 使用 Windows 音频引擎，这是会议应用通常采用的采集方式。独占采集接受 `--rate`、`--bits` 和 `--channels`；`play` 从 WAV 文件读取输入格式。运行 `.\target\release\virtual-mic-demo.exe --help` 查看命令语法。

安装驱动后，运行音频检查：

```powershell
# 10 项严格写入，覆盖每种采集格式。
.\target\release\virtual-mic-demo.exe selftest

# 10 项严格写入，加上全部 100 种输入到采集格式的转换组合。
.\target\release\virtual-mic-demo.exe selftest --matrix

# 10 种输入格式，通过默认单声道 48 kHz/16-bit 设置进行共享采集。
.\target\release\virtual-mic-demo.exe selftest --shared
```

运行检查前，关闭其他 Virtual Mic 发送和采集应用。运行 `--shared` 前，在 Windows 声音设置中取消 Virtual Mic 静音，并将音量设为 100。每个用例通过 WASAPI 采集注入的测试音，检查各声道的预期频率。严格用例还检查格式拒绝、单写入端限制、缓冲写满和队列清空。转换用例还检查写入器不会在两次写入之间扣住转换后的音频。

## 开发检查

```powershell
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo doc --workspace --no-deps
```

常规测试可以在未安装驱动时运行，覆盖协议布局、PCM 处理和全部 100 种流式转换组合。上方两个 Core 示例会作为文档测试进行编译。

在已安装驱动的机器上，关闭其他 Virtual Mic 应用，再单独运行协议测试：

```powershell
cargo test -p virtual-mic-core installed_driver -- --ignored
```

驱动仓库提供 `tools/Build-VirtualMic.ps1 -Configuration Release -TestSign` 用于构建驱动，`tools/Install-VirtualMic.ps1` 用于安装。

## 许可证

MIT，详见仓库根目录的 `LICENSE` 文件。
