<div align="center">

<h1>
  <img src="assets/logo-dark.gif#gh-dark-mode-only" alt="nyan" height="40" align="top"><img src="assets/logo-light.gif#gh-light-mode-only" alt="nyan" height="40" align="top">
  nyan
</h1>

English | [简体中文](README_CN.md)

![Rust](https://img.shields.io/badge/Rust-2024-orange?logo=rust&logoColor=white)
![License](https://img.shields.io/badge/License-AGPL--3.0-blue)

</div>

A terminal-based, provider-agnostic agent for software engineering, written in
Rust. nyan provides an interactive workspace for coding, tool execution, MCP,
permissions, and agent workflows across supported model providers.

> **Unofficial project, not affiliated with Anthropic.**
> This is an independent reimplementation built by reading the published
> Claude Code CLI. "Claude" and "Claude Code" are trademarks of Anthropic, PBC.
> Nothing here is endorsed by or supported by Anthropic.

## What this is

nyan's terminal UI is implemented in Rust on iocraft, a React-style retained-mode
TUI framework built around components, hooks, and declarative elements. The
architecture keeps those concepts explicit, and deliberate deviations are
recorded in the source where they happen.

It builds against [CometixTUI], a fork of iocraft carrying the primitives nyan
needs (row-level diffing, event propagation, SIGCONT self-healing, IME cursor,
bracketed paste, grid layout).

It is a work in progress. Large parts of the interactive loop, tool execution,
permissions, MCP and slash commands are implemented; other areas are partial.

## Sponsors

<p align="center">
  <a href="https://www.infistar.cc/register?aff=HALLEY#gh-dark-mode-only"><img src="assets/sponsor-infistar-en-dark.webp#gh-dark-mode-only" alt="Infistar.cc Sponsor"></a><a href="https://www.infistar.cc/register?aff=HALLEY#gh-light-mode-only"><img src="assets/sponsor-infistar-en-light.webp#gh-light-mode-only" alt="Infistar.cc Sponsor"></a>
</p>

<p align="center">
  <strong>nyan × Infistar.cc | High-Availability AI API</strong><br>
  Stable routing | 10% rates · Transparent · Failover<br>
  Top models | ChatGPT · Claude · Gemini · Kimi · GLM · DeepSeek<br>
  Multimodal | Text · Image · Video · Embedding / Rerank<br>
  nyan users: bonus credits via the <a href="https://www.infistar.cc/register?aff=HALLEY">exclusive link</a>.
</p>

## Building

### Prerequisites

**Rust 1.99+** (edition 2024). Nothing else — no system libraries, no
`pkg-config`.

### Build and run

```sh
cargo build --release
cargo run --release
```

The binary is `nyan`.

## Installation

Supported release platforms:

- Linux x86_64 (musl)
- Linux aarch64 (musl)
- Windows x86_64 (MSVC)

macOS is not supported: no macOS artifacts are built or published.

### Linux

Install the latest release with `install.sh`:

```sh
curl -fsSL https://raw.githubusercontent.com/blxzer77/nyan/master/install.sh | bash
```

Pass a version or tag as the first argument:

```sh
curl -fsSL https://raw.githubusercontent.com/blxzer77/nyan/master/install.sh | bash -s -- nyan-v0.2.0
```

The script installs to `~/.local/bin` by default (override with `NYAN_INSTALL_DIR`) and verifies the archive against the release's `SHA256SUMS` before installing.

To install from source instead:

```sh
cargo install --git https://github.com/blxzer77/nyan.git --locked --bin nyan
```

### Windows

Windows releases require [Git for Windows](https://git-scm.com/downloads/win), because nyan's general shell execution uses Git Bash rather than native PowerShell. Install with `install.ps1`; it does not install Git for Windows and only prints a warning if Git Bash is not detected.

```powershell
.\install.ps1
```

If Git Bash is installed outside the usual locations, set `NYAN_GIT_BASH_PATH` to the full path of `bash.exe` before starting nyan, for example:

```powershell
$env:NYAN_GIT_BASH_PATH = 'C:\Program Files\Git\bin\bash.exe'
```

#### Verifying the download

Each release publishes `nyan-vX.Y.Z-windows-x86_64.zip` and `SHA256SUMS` on the same GitHub Release page. `SHA256SUMS` is in GNU coreutils format (`<sha256>  <filename>`). Before extracting the zip, compute its digest in PowerShell from the download directory (example for 0.2.0):

```powershell
Get-FileHash -Algorithm SHA256 .\nyan-v0.2.0-windows-x86_64.zip
Select-String -Path .\SHA256SUMS -Pattern 'windows-x86_64\.zip$'
```

Compare the `Hash` value with the digest on the `SHA256SUMS` line whose filename is that zip (case does not matter). If they differ, do not extract or run the file; download it again. `install.ps1` performs the same comparison before installing.

#### SmartScreen warning

The Windows binary is not code-signed yet. A signing certificate has not been provided, and whether to sign releases is undecided. Files downloaded from the internet carry a download mark, so Windows SmartScreen may show "Windows protected your PC" the first time you run `nyan.exe`. Continue only after the checksum above matches:

1. Click **More info**.
2. Click **Run anyway**.

Alternatively, after extracting, remove the download mark from the two executables (this has no effect if the mark is absent):

```powershell
Unblock-File -Path .\nyan.exe, .\vendor\ripgrep\x64-win32\rg.exe
```

Button labels depend on the Windows display language.

### Updating

Release builds installed from a GitHub Release (Linux x86_64/aarch64 and
Windows x86_64) check `blxzer77/nyan` releases in the background and show a
footer notice when a newer version exists. They never download or replace the
binary on their own. To update:

```sh
nyan update
```

The command downloads the matching asset from the release, verifies it against
that release's `SHA256SUMS`, and atomically replaces the running binary.
Restart nyan afterwards.

- `NYAN_REPO=owner/name` overrides the release repository.
- `DISABLE_AUTOUPDATER=1`, `NYAN_DISABLE_AUTOUPDATER=1` or
  `NYAN_DISABLE_NONESSENTIAL_TRAFFIC=1` disables the background check;
  `nyan update` still runs when invoked explicitly.
- Both use the configured proxy and CA settings (`HTTPS_PROXY`, `NO_PROXY`,
  `NODE_EXTRA_CA_CERTS`, including the `env` block of user settings).

Only stable releases are offered: drafts and prereleases are ignored, and
there is no channel setting.

If the install directory is not writable (for example a root-owned
`/usr/local/bin`), `nyan update` changes nothing and asks you to re-run the
install script: `install.sh` on Linux, `install.ps1` on Windows.

Source builds (`cargo build`, `cargo install`) are not replaced by
`nyan update`; it prints the rebuild instruction instead.

### Cross-compiling for Windows

```sh
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

A mingw-w64 toolchain has to be on `PATH`: four dependencies compile C or
assembly (`onig_sys`, `ring`, `tree-sitter`, `tree-sitter-bash`). They link
statically, so the resulting `.exe` needs no DLLs alongside it.

## Configuration

nyan reads provider and model profiles from `~/.nyan/settings.json`. Start from
[docs/nyan/settings.example.json](docs/nyan/settings.example.json): it defines
one Anthropic provider and model profile, selects it as `nyan.selection.main`,
and reads the API key from `ANTHROPIC_API_KEY`.

## ripgrep

File search shells out to `rg`. The binary is an external asset, never compiled
into the executable.

Resolution order:

1. `vendor/ripgrep/<arch>-<os>/rg` next to the executable (release archives)
2. `vendor/ripgrep/<arch>-<os>/rg` in the source tree (development)
3. the host's `rg` on `PATH`

So either drop a pinned ripgrep under `vendor/ripgrep/`, or just have `rg`
installed. `/doctor` reports which one is in use. Setting
`USE_BUILTIN_RIPGREP=0` forces the host binary.

## Testing

```sh
just test          # full suite through cargo-nextest — the gate
just t <pattern>   # substring filter
just check         # type/borrow check only, links nothing
```

`cargo test` is **not** a valid gate for this crate. It runs the whole suite as
threads in one process, and this codebase has process-wide state (env vars,
`OnceLock` caches) that leaks between tests as a result. `just test` runs under
`cargo-nextest`, which gives each test its own process, and pins the host state
a test would otherwise inherit. The justfile header explains the measurements
behind that.

## Community

<a href="https://qm.qq.com/q/nmbS5eUUP8" target="_blank"><img src="https://img.shields.io/badge/QQ%20群-1045122926-EB1923?logo=tencentqq&logoColor=white" alt="QQ Group" /></a>
[![LINUX DO](https://img.shields.io/badge/LINUX%20DO-Community-blue)](https://linux.do/t/topic/2927016)

## Acknowledgements

- **[CometixCode]** — nyan is a fork of CometixCode by Haleclipse.
- **[ClaudeCodeRev]** — the analysis work this port reads from. Every behaviour
  here was derived from studying Claude Code through it.
- **[CometixTUI]** — the TUI framework this is built on, an iocraft fork
  carrying the Ink parity primitives the port needs.
- **[marked-rs]** — Rust port of `marked`, which CC uses to parse Markdown.
- **[anthropic-sdk-rs]** — Rust port of `@anthropic-ai/sdk`, the API client.
- **[nucleo]** by the Helix editor project — the fuzzy matcher behind file,
  command and agent completion.

## License

[AGPL-3.0-only](LICENSE).

[CometixCode]: https://github.com/Haleclipse/CometixCode
[CometixTUI]: https://github.com/Haleclipse/CometixTUI
[ClaudeCodeRev]: https://github.com/Haleclipse/ClaudeCodeRev
[marked-rs]: https://github.com/Haleclipse/marked-rs
[anthropic-sdk-rs]: https://github.com/Haleclipse/anthropic-sdk-rs
[nucleo]: https://github.com/helix-editor/nucleo
