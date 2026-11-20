# NDM Next YouTube plugin

This engine downloads YouTube videos, audio and playlists through NDM Next.
The package includes pinned yt-dlp and Deno runtimes. NDM Next supplies
FFmpeg when a video needs separate audio and video tracks. The engine
supports transfer progress and reuse of yt-dlp's partial files.

Install from the NDM Next plugin market or import the matching release ZIP.
Paste a YouTube URL and select the quality in the native download form.
When AI integration is enabled, Codex CLI also sees the YouTube tools.

Build with Rust 1.100.0. Use `cargo +1.100.0 test --locked` for local
verification. GitHub Actions publishes the platform archives and SHA-256
sidecars for each version tag. The upstream runtime versions and release
digests are recorded in [runtime-lock.json](runtime-lock.json).
