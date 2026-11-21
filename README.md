# NDM Next YouTube plugin

This engine downloads YouTube videos, audio and playlists through NDM Next.
The package includes pinned yt-dlp and Deno runtimes. NDM Next supplies
FFmpeg when a video needs separate audio and video tracks. These tracks are
merged into an MKV file. A single downloaded stream keeps its original
container. The engine reuses yt-dlp's partial files when resuming.

Install from the NDM Next plugin market or import the matching release ZIP.
Paste a YouTube URL in New download and select quality, playlist and subtitle
options. Choose uploaded subtitles or include automatic subtitles. Enter
comma-separated language codes such as `en,zh-Hans,zh-Hant`. The available
languages depend on the video. Subtitle files are saved alongside the media.

Open **Settings** on the installed YouTube card to save defaults and set a
separate proxy. With the switch on, enter an HTTP(S) or SOCKS proxy URL.
With the switch off, yt-dlp uses the system network settings. Codex CLI
downloads use the saved defaults when AI integration is enabled.

Build with Rust 1.100.0. Use `cargo +1.100.0 test --locked` for local
verification. GitHub Actions publishes the platform archives and SHA-256
sidecars for each version tag. The upstream runtime versions and release
digests are recorded in [runtime-lock.json](runtime-lock.json).

## 中文

在 NDM Next 的插件市场安装 YouTube 插件，然后粘贴链接新建下载。可选择
画质、仅音频、播放列表和字幕。视频与音频分开下载时合并为 MKV，单一音视频流
保留原封装。字幕可以选择仅作者上传，或同时下载自动字幕。语言用逗号分隔，
例如 `en,zh-Hans,zh-Hant`，以视频提供的语言为准。字幕文件随视频一起保存。

在已安装的 YouTube 卡片上打开 **设置** 可保存默认选项。开启 **使用独立代理**
后填入 HTTP(S) 或 SOCKS 代理地址。关闭时沿用系统网络设置。开启 AI 集成后，
Codex CLI 发起的新任务也会使用这些默认选项。
