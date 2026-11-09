<div align="center">

<h1>
  <img src="assets/logo-dark.gif#gh-dark-mode-only" alt="nyan" height="40" align="top"><img src="assets/logo-light.gif#gh-light-mode-only" alt="nyan" height="40" align="top">
  nyan
</h1>

[English](README.md) | 简体中文

![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white)
![License](https://img.shields.io/badge/License-AGPL--3.0-blue)

</div>

一个面向服务商通用的终端 Agent，用 Rust 编写，面向软件工程工作流。nyan
提供交互式编码、工具执行、MCP、权限控制和 Agent 工作流能力，支持多种模型服务商。

> **非官方项目，与 Anthropic 无隶属关系。**
> 本项目是通过研读已发布的 Claude Code CLI 独立实现的。
> "Claude" 与 "Claude Code" 是 Anthropic, PBC 的商标。
> 本项目未获得 Anthropic 的背书或支持。

## 这是什么

nyan 的终端界面用 Rust 基于 iocraft 实现。iocraft 是一个围绕组件、hooks
和声明式元素构建的 React 风格保留模式 TUI 框架。项目保持这些概念清晰可见，
刻意的偏离都就地记录在源码注释里。

项目构建于 [CometixTUI]，那是 iocraft 的一个 fork，携带了 nyan 所需的原语
（行级差分、事件传播、SIGCONT 自愈、输入法光标、括号粘贴、网格布局）。

项目仍在进行中。交互主循环、工具执行、权限、MCP 与斜杠命令的大部分已经实现，
其余部分尚不完整。

## 赞助商

<p align="center">
  <a href="https://www.infistar.cc/register?aff=HALLEY#gh-dark-mode-only"><img src="assets/sponsor-infistar-zh-dark.webp#gh-dark-mode-only" alt="无限星河 Sponsor"></a><a href="https://www.infistar.cc/register?aff=HALLEY#gh-light-mode-only"><img src="assets/sponsor-infistar-zh-light.webp#gh-light-mode-only" alt="无限星河 Sponsor"></a>
</p>

<p align="center">
  <strong>nyan × Infistar.cc 无限星河｜高可用全模型 API</strong><br>
  企业级高并发稳定调度｜官方 1 折起 · 透明倍率 · 多节点冗余<br>
  全系模型支持｜ChatGPT · Claude · Gemini · DeepSeek<br>
  全模态接入｜文本 · 图像 · 视频 · Embedding / Rerank<br>
  nyan 用户通过<a href="https://www.infistar.cc/register?aff=HALLEY">专属链接</a>注册，可享专属特惠。
</p>

## 构建

```sh
cargo build --release
cargo run --release
```

可执行文件名为 `nyan`。

## 安装

支持的发布平台：

- Linux x86_64（musl）
- Linux aarch64（musl）
- Windows x86_64（MSVC）

不支持 macOS：不构建、也不发布 macOS 产物。

### Linux

使用 `install.sh` 安装最新版本：

```sh
curl -fsSL https://raw.githubusercontent.com/blxzer77/nyan/master/install.sh | bash
```

可在第一个参数传入版本或 tag：

```sh
curl -fsSL https://raw.githubusercontent.com/blxzer77/nyan/master/install.sh | bash -s -- nyan-v0.2.0
```

脚本默认安装到 `~/.local/bin`（可用 `NYAN_INSTALL_DIR` 覆盖），安装前会用 Release 的 `SHA256SUMS` 校验归档。

从源码安装：

```sh
cargo install --git https://github.com/blxzer77/nyan.git --locked --bin nyan
```

### Windows

Windows 版需要预先安装 [Git for Windows](https://git-scm.com/downloads/win)，因为 nyan 的通用 shell 执行使用 Git Bash，而不是原生 PowerShell。使用 `install.ps1` 安装时，脚本不会安装 Git for Windows；检测不到 Git Bash 只会打印警告。

```powershell
.\install.ps1
```

如果 Git Bash 安装在常见位置之外，请在启动 nyan 前将 `NYAN_GIT_BASH_PATH` 设置为 `bash.exe` 的完整路径，例如：

```powershell
$env:NYAN_GIT_BASH_PATH = 'C:\Program Files\Git\bin\bash.exe'
```

#### 校验下载文件

每个 Release 在同一页面提供 `nyan-vX.Y.Z-windows-x86_64.zip` 和 `SHA256SUMS`。`SHA256SUMS` 为 GNU coreutils 格式（`<sha256>  <文件名>`）。解压前，在下载目录中用 PowerShell 计算摘要（以 0.2.0 为例）：

```powershell
Get-FileHash -Algorithm SHA256 .\nyan-v0.2.0-windows-x86_64.zip
Select-String -Path .\SHA256SUMS -Pattern 'windows-x86_64\.zip$'
```

将输出的 `Hash` 与 `SHA256SUMS` 中文件名为该 zip 的那一行的摘要比对（大小写不影响结果）。不一致时不要解压或运行，重新下载。`install.ps1` 安装前会做同样的比对。

#### SmartScreen 提示

Windows 版二进制目前未做代码签名；签名证书尚未提供，是否签名待定。从网络下载的文件带有来源标记，因此首次运行 `nyan.exe` 时，Windows SmartScreen 可能显示“Windows 已保护你的电脑”。只有在上述校验通过后才继续：

1. 点击“更多信息”。
2. 点击“仍要运行”。

或者在解压后，对两个可执行文件移除下载标记（若无标记则无影响）：

```powershell
Unblock-File -Path .\nyan.exe, .\vendor\ripgrep\x64-win32\rg.exe
```

按钮文字随 Windows 显示语言而不同（英文系统为 More info / Run anyway）。

### 更新

从 GitHub Release 安装的版本（Linux x86_64/aarch64、Windows x86_64）会在后台检查 `blxzer77/nyan` 的新版本，发现更新时只在页脚提示，不会自行下载或替换二进制。需要更新时运行：

```sh
nyan update
```

该命令会从对应 Release 下载匹配的资产，用该 Release 的 `SHA256SUMS` 校验后原子替换正在运行的二进制；完成后重新启动 nyan。

- `NYAN_REPO=owner/name` 覆盖 Release 仓库。
- `DISABLE_AUTOUPDATER=1`、`NYAN_DISABLE_AUTOUPDATER=1` 或 `NYAN_DISABLE_NONESSENTIAL_TRAFFIC=1` 关闭后台检查；显式运行 `nyan update` 仍然有效。
- 两者都使用已配置的代理与 CA（`HTTPS_PROXY`、`NO_PROXY`、`NODE_EXTRA_CA_CERTS`，包括用户 settings 的 `env`）。

只提供稳定版：忽略 draft 和预发布版本，没有更新通道设置。

安装目录不可写时（例如 root 所有的 `/usr/local/bin`），`nyan update` 不做任何改动，并提示重新运行安装脚本：Linux 用 `install.sh`，Windows 用 `install.ps1`。

源码构建（`cargo build`、`cargo install`）不会被 `nyan update` 替换；命令会打印重新构建的说明。

### 交叉编译 Windows 版

需要 mingw-w64 工具链。

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

## 配置

nyan 从 `~/.nyan/settings.json` 读取 provider 与模型 profile。可从
[docs/nyan/settings.example.json](docs/nyan/settings.example.json) 开始：它定义一个
Anthropic provider 与模型 profile，将其设为 `nyan.selection.main`，并从
`ANTHROPIC_API_KEY` 环境变量读取 API key。

## ripgrep

文件搜索会调用外部的 `rg`。该二进制是外挂资产，**不会**被编译进可执行文件。

查找顺序：

1. 可执行文件同级的 `vendor/ripgrep/<arch>-<os>/rg`（发行包）
2. 源码树中的 `vendor/ripgrep/<arch>-<os>/rg`（开发环境）
3. `PATH` 中宿主自带的 `rg`

所以你既可以在 `vendor/ripgrep/` 下放一份固定版本的 ripgrep，也可以直接装一个
`rg` 了事。`/doctor` 会报告当前用的是哪一个。设置 `USE_BUILTIN_RIPGREP=0` 可强制
使用宿主的二进制。

## 测试

```sh
just test          # cargo-nextest 全量套件 —— 唯一的门禁
just t <pattern>   # 按子串过滤
just check         # 仅做类型/借用检查，不链接
```

`cargo test` **不是**本 crate 的有效门禁。它把整个套件作为线程跑在同一个进程里，
而这份代码库存在进程级状态（环境变量、`OnceLock` 缓存），因此会在测试之间泄漏。
`just test` 走 `cargo-nextest`，为每个测试分配独立进程，并钉住测试本会从宿主继承
的那部分状态。justfile 的头部注释记录了支撑这一判断的实测数据。

## 社区

<a href="https://qm.qq.com/q/nmbS5eUUP8" target="_blank"><img src="https://img.shields.io/badge/QQ%20群-1045122926-EB1923?logo=tencentqq&logoColor=white" alt="QQ Group" /></a>
[![LINUX DO](https://img.shields.io/badge/LINUX%20DO-Community-blue)](https://linux.do/t/topic/2927016)

## 致谢

- **[CometixCode]** —— nyan 是 Haleclipse 的 CometixCode 项目的分支（fork）。
- **[ClaudeCodeRev]** —— 本移植所依据的分析工作。这里的每一处行为，都源自经由它
  对 Claude Code 的研读。
- **[CometixTUI]** —— 本项目所基于的 TUI 框架，一个携带了 Ink 对齐原语的
  iocraft fork。
- **[marked-rs]** —— `marked` 的 Rust 移植，CC 用它解析 Markdown。
- **[anthropic-sdk-rs]** —— `@anthropic-ai/sdk` 的 Rust 移植，即 API 客户端。
- **[nucleo]**（来自 Helix 编辑器项目）—— 文件、命令与 agent 补全背后的模糊匹配器。

## 许可

[AGPL-3.0-only](LICENSE)。

[CometixCode]: https://github.com/Haleclipse/CometixCode
[CometixTUI]: https://github.com/Haleclipse/CometixTUI
[ClaudeCodeRev]: https://github.com/Haleclipse/ClaudeCodeRev
[marked-rs]: https://github.com/Haleclipse/marked-rs
[anthropic-sdk-rs]: https://github.com/Haleclipse/anthropic-sdk-rs
[nucleo]: https://github.com/helix-editor/nucleo
