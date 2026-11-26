# OnesDM Media Downloader (yt-dlp)

Media Downloader uses yt-dlp's site extractors and generic media page parser.
It matches common platforms such as YouTube, Bilibili, Douyin, TikTok, Vimeo,
X, Instagram, Reddit, Twitch, SoundCloud and Bandcamp. The link must expose
downloadable media to the pinned yt-dlp version. OnesDM reports protected streams
and unsupported pages as unsupported sources.

The package includes pinned yt-dlp and Deno runtimes. OnesDM supplies
FFmpeg when a video needs separate audio and video tracks. **Prefer native**
keeps the original container where possible. You can also choose MP4, WebM,
or MKV. The engine copies compatible source streams and reuses yt-dlp's
partial files when resuming. Older desktop builds with limited WebM output
support can fall back to MKV for split streams.

Install from the OnesDM plugin market or import the matching release ZIP.
Paste a link in **New download**. For an unlisted site, choose **Media
Downloader** under **Download engine**. Manual selection also accepts direct
HLS and DASH URLs. This uses the desktop SDK's `manual_inputs` support.
Choose quality, playlist and subtitle options. With playlist download off, a
playlist-only link queues its first item. **Audio only** selects an available
standalone audio track. Prefer H.265 or H.264 among the source tracks at the chosen
resolution. H.265 playback depends on the device. Choose uploaded or
automatic subtitles, then select one or more languages from the list. Command
line calls use comma-separated codes such as `en,zh-Hans,zh-Hant`. Subtitle
files are saved alongside the media when the video provides those tracks.

For mobile sources, use **User-Agent** and **Request language** in the download
options or save defaults in the plugin settings. Leave either blank to use
yt-dlp's default. OnesDM 0.9.0 and newer also accept the per-task
`user_agent_profile` and `accept_language` fields through MCP. Request headers
provided by the task, including those captured by the browser extension, take
precedence over the plugin defaults.

OnesDM 0.9.1 and newer display approximate totals for streams when yt-dlp can
estimate the size. Split video and audio downloads report both tracks together.
The local MP4 remux keeps the index at the end, saving an additional rewrite
of the file.

Open **Settings** on the installed Media Downloader card to save defaults and set a
separate proxy. With the switch on, select the protocol and enter a host and
port. You can paste a complete HTTP(S) or SOCKS proxy URL as well.
With the switch off, yt-dlp uses the system network settings. Codex CLI
downloads use the saved defaults when AI integration is enabled. Existing
installations upgrade under the `yt-dlp` ID, and Codex can use
`ndm_yt_dlp__download` and `ndm_yt_dlp__audio` for both existing and new sites.

With a desktop and companion extension that support browser page capabilities,
the extension can submit the current page URL to Media Downloader.

Build with Rust 1.100.0. Use `cargo +1.100.0 test --locked` for local
verification. GitHub Actions publishes the platform archives and SHA-256
sidecars for each version tag. The upstream runtime versions and release
digests are recorded in [runtime-lock.json](runtime-lock.json).

## 中文

流媒体下载使用 yt-dlp 的站点解析器和通用媒体网页解析。YouTube、Bilibili、抖音、
TikTok、Vimeo、X、Instagram、Reddit、Twitch、SoundCloud、Bandcamp 等常见平台
可自动匹配。可下载的内容以随包 yt-dlp 版本对该链接的解析为准，受保护的流和无法解析
的页面会显示为来源不支持。

安装后在 **新建下载** 中粘贴链接。其他站点和 HLS/DASH 直链可在 **下载引擎**
中手动选 **流媒体下载**，需桌面端支持 `manual_inputs`。可选择画质、仅音频、
播放列表和字幕。关闭播放列表时，纯播放列表链接会下载第一项。仅音频会选择来源提供的
独立音轨。默认尽量保留原生封装，也可以指定 MP4、WebM 或
MKV。插件直接复制兼容的源轨。同分辨率可优先选用源站提供的 H.265 或 H.264，
H.265 的播放取决于设备。旧版媒体工具的 WebM 合并可回退为 MKV。

字幕可选择仅作者上传或包含自动字幕，并从列表勾选多种语言。命令行调用可传入
逗号分隔的代码，例如 `en,zh-Hans,zh-Hant`。视频提供的相应字幕会随视频保存。

遇到需要手机请求的来源，可以在下载选项或插件设置中设置 **User-Agent** 和
**请求语言**。留空时沿用 yt-dlp 的默认值。OnesDM 0.9.0 起也可以通过 MCP 为任务
指定 `user_agent_profile` 和 `accept_language`。任务显式提供或浏览器扩展
捕获的请求头优先于插件默认值。

OnesDM 0.9.1 起会显示 yt-dlp 提供的流大小估算。分开下载音轨和画面时会合计
进度。本地 MP4 封装保留尾部索引，省去一次整文件重写。

在已安装的流媒体下载卡片上打开 **设置** 可保存默认选项。开启 **使用独立代理**
后请选择 HTTP(S) 或 SOCKS 协议，再填主机和端口，也可以粘贴完整地址。关闭时沿用系统网络设置。开启 AI 集成后，
Codex CLI 发起的新任务也会使用这些默认选项。原 YouTube 插件通过 `yt-dlp`
这个 ID 升级，Codex 工具 `ndm_yt_dlp__download` 和 `ndm_yt_dlp__audio`
可调用新增的站点。

配套桌面端和浏览器扩展支持网页解析能力时，可从扩展把当前网页交给流媒体下载插件。
