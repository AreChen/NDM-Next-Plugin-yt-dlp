# Media Downloader

Use the `yt-dlp` engine for a supported media page, an available audio track,
or a playlist. Use `ndm_list_engines` with the source in OnesDM 0.6.0 or later
to distinguish an automatic match from an engine you must select explicitly.
Site compatibility depends on the bundled yt-dlp version and the tracks the
page exposes.

- Call `ndm_yt_dlp__download` for ordinary video and `ndm_yt_dlp__audio` to select
  an available audio-only track. Use `ndm_download` with per-item
  `"plugin":"yt-dlp"` for a batch.
- To cap resolution at 1080p and prefer MP4, pass
  `"options":{"quality":"1080","video-container":"mp4"}`. Omit these fields to
  use the app's saved choices. `video-codec` can prefer H.264 or H.265 when the
  chosen resolution and container support that track.
- Set `"playlist":true` in `options` for a requested playlist. With playlist mode
  off, a playlist-only link queues its first item.
- With OnesDM 0.9.0, use the task's `user_agent_profile` (`iphone_safari` or
  `android_chrome`), `accept_language`, and `referer` for relevant mobile requests.
  The plugin also accepts `options` fields `user-agent` and `accept-language`
  for saved defaults. Explicit and browser-captured request headers take
  precedence. Use `ndm_probe_streams` or the browser extension when a page
  needs another way to discover a native HLS or DASH stream.
- To request author's subtitles in Simplified Chinese and English, pass
  `"options":{"subtitles":"manual","subtitle-languages":"zh-Hans,en"}`.
  Use `"subtitles":"auto"` to include automatic subtitles. Tracks depend on
  availability at the source.
- Leave proxy fields out to use the user's settings. A protected stream or a
  source that yt-dlp cannot resolve can fail after the task is queued. Confirm
  the result using the returned task ID.
