# GitHub Releases 发布约定

状态：设计与首版 workflow 一起落地。当前只发布 Linux 和 Windows；macOS 只保留 workflow matrix 占位。

## 版本与 tag

- `Cargo.toml` 的 `[package] version` 是唯一版本来源，格式为 `X.Y.Z`。
- 正式版本使用 `nyan-vX.Y.Z`，例如 `Cargo.toml` 为 `0.2.0` 时使用 `nyan-v0.2.0`。
- 预发布版本使用 `nyan-vX.Y.Z-alpha.N`、`nyan-vX.Y.Z-beta.N` 或 `nyan-vX.Y.Z-rc.N`，例如 `nyan-v0.2.1-rc.1`。
- tag 的基础版本 `X.Y.Z` 必须等于 `Cargo.toml`；预发布后缀不写入 Cargo 版本。
- 首个正式版本从哪个 `X.Y.Z` 开始需要 maintainer 拍板；当前 Cargo 版本是 `0.2.0`，因此 `nyan-v0.2.0` 可以作为首个 tag，也可以先 bump 再发布。

版本校验在构建前执行。tag 格式错误、tag 与 Cargo 版本不一致时，workflow 不构建、不上传、不创建 Release。

## 触发方式

- `push` 到 `nyan-v*` tag：构建三个目标、生成校验文件并创建或更新对应 GitHub Release。
- `workflow_dispatch`：用于 dry run。可选输入 `version`，默认读取当前 Cargo 版本；只上传 Actions artifact，不创建 GitHub Release，也不需要正式 tag。
- 其它分支、普通 push、pull request 都不会触发 release workflow，因此不影响现有 `Merge gate`。
- 发布 job 只有 tag push 拥有 `contents: write`；构建 job 只有读权限。

正式发布前仍应先让对应 commit 的 Merge gate 通过，再创建 tag。发布 workflow 本身不替代 PR 门禁。

## 构建目标与资产

当前 matrix 中的有效目标：

| 目标 | runner | Rust target | 归档 |
|---|---|---|---|
| Linux x86_64 | `ubuntu-24.04` | `x86_64-unknown-linux-musl` | `nyan-vX.Y.Z-linux-x86_64.tar.gz` |
| Linux aarch64 | `ubuntu-24.04-arm` | `aarch64-unknown-linux-musl` | `nyan-vX.Y.Z-linux-aarch64.tar.gz` |
| Windows x86_64 | `windows-2025` | `x86_64-pc-windows-msvc` | `nyan-vX.Y.Z-windows-x86_64.zip` |

Linux 使用 `cargo-zigbuild` + Zig 0.14.1，而不是 `cross`：两个 musl 目标都能用同一套 Zig C 链接器处理，省去 Docker/目标镜像和特权容器依赖；Linux hosted runner 也能原生构建 x86_64，arm64 使用原生 `ubuntu-24.04-arm`。Rust target 是 musl，产物按目标链接为静态 Linux 二进制。

Windows 使用原生 MSVC runner 和 `x86_64-pc-windows-msvc`，归档内文件名固定为 `nyan.exe`。这不是功能适配：本阶段只要求编译、打包和 `--version` 冒烟成功。seccomp、Unix 专属功能若使 Windows 构建失败，只能在本次构建中用已有 cfg/feature 关闭；每一处都必须在 PR 描述和后续 Windows 审计中列出。本首版不修改 `src/`，因此不会静默改变 Windows 功能。

macOS 目标（x86_64、aarch64）保留在 matrix 作为禁用占位，不构建、不上传，待单独设计签名和 runner 策略后启用。

归档包含二进制和 `LICENSE`。每次发布另有 `SHA256SUMS`，格式严格为 GNU coreutils 兼容的：

```text
<64 位小写 sha256>  <资产文件名>
```

文件名使用两个 ASCII 空格分隔，不包含绝对路径；一行对应一个 `.tar.gz` 或 `.zip` 资产。安装脚本下载同一 Release 的 `SHA256SUMS` 后再安装，禁止只依赖 TLS 下载成功作为完整性校验。

## changelog

不维护独立的 `CHANGELOG.md`。正式 tag 创建 Release 时使用 GitHub CLI 的 `--generate-notes`，由 GitHub 根据上一个 Release、commit 和 PR 生成变更说明。预发布也使用同一来源；需要人工补充的 breaking change、迁移说明和已知限制写入 GitHub Release body。

## 安装脚本

- Linux：`install.sh [版本或 tag]`，默认安装到 `~/.local/bin`；通过 `NYAN_INSTALL_DIR` 指定目录。未传版本时解析 GitHub latest stable Release。
- Windows：`install.ps1 [-Version 版本或 tag] [-InstallDir 目录]`，默认安装到 `$HOME\.local\bin`。脚本不自动改 PATH，会打印把该目录加入用户 PATH 的 PowerShell 命令。
- 两个脚本都只支持当前平台的 x86_64/aarch64（Windows 仅 x86_64），都会下载 `SHA256SUMS`、校验对应资产，再解包并安装。

## 失败与回滚

1. 构建、冒烟或汇总失败：workflow 失败，不创建 Release；修复后重新 dispatch 或重新推送一个新 tag。
2. 已创建 Release 但资产有误：先把 Release 标记为 draft 或删除错误资产，撤销对应 tag；不要复用已经公开给用户的版本号。
3. 修复后递增 `X.Y.Z` 或预发布序号，重新走 PR、Merge gate 和新 tag。正式版本不覆盖、不 force-push、不把旧版本的 tag 指向新 commit。
4. 如果只是同一 tag 的上传网络失败，允许重新运行 workflow；publish job 会对同名资产使用 `--clobber`，但不会在没有 Release 时跳过创建步骤。

## Terminal-Bench 约定

`nyan-ci` 上的 Terminal-Bench 工作目录固定为 `/srv/tbench`，但本 PR 不修改 tbench。需要使用 nyan 时遵守以下约定：

1. job 配置显式给出 `NYAN_VERSION=nyan-vX.Y.Z`，不依赖 `latest`，以保证评测可复现；预发布只能由明确配置的 job 使用。
2. 按 `uname -m` 选择 `nyan-vX.Y.Z-linux-x86_64.tar.gz` 或 `nyan-vX.Y.Z-linux-aarch64.tar.gz`，从同一 Release 下载 `SHA256SUMS` 和归档，先用 `sha256sum -c` 校验。
3. 解压到 `/srv/tbench/nyan/<tag>/`，约定可执行文件为 `/srv/tbench/nyan/<tag>/nyan`；job 可在自己的环境中把该目录加入 `PATH`，不要覆盖系统 `/usr/bin` 或共享的旧版本。
4. `/srv/tbench/nyan/current` 由调用方按需指向已校验的版本目录；切换版本应是原子替换 symlink，失败时保留旧版本目录以便回退。
5. 记录 tag、资产文件名和 `SHA256SUMS` 中的摘要到评测日志。不要在 tbench 中使用未校验的 URL、工作树构建结果或 GitHub Actions artifact。
