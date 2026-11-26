#[cfg(unix)]
use std::time::Instant;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{self, BufRead, BufReader, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use ndm_plugin_sdk::{
    DownloadRequest, DownloadSource, ENGINE_ARGUMENT, FailureReason, MEDIA_TOOLS_ENV, TaskUpdate,
    TransferProgress, Worker, serve,
};
use serde_json::{Map, Value};

fn main() -> io::Result<()> {
    if std::env::args().nth(1).as_deref() != Some(ENGINE_ARGUMENT) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "run this engine through OnesDM",
        ));
    }
    serve(download)
}

fn download(request: DownloadRequest, worker: &Worker) -> Result<Vec<String>, FailureReason> {
    let DownloadSource::Url { url, headers } = &request.source else {
        return Err(FailureReason::InvalidSource);
    };
    if !(url.starts_with("https://") || url.starts_with("http://"))
        || url.chars().any(char::is_control)
    {
        return Err(FailureReason::InvalidSource);
    }
    let engine = std::env::current_exe().map_err(|_| FailureReason::Other)?;
    let tools = engine
        .parent()
        .ok_or(FailureReason::Storage)?
        .join("runtime");
    let media = std::env::var_os(MEDIA_TOOLS_ENV)
        .map(PathBuf::from)
        .filter(|path| {
            path.join(executable("ffmpeg")).is_file() && path.join(executable("ffprobe")).is_file()
        });
    let deno = tools.join(executable("deno"));
    let mut command = Command::new(tools.join(executable("yt-dlp")));
    command
        .current_dir(&request.workspace)
        .stdin(Stdio::piped())
        .args([
            "--ignore-config",
            "--no-cache-dir",
            "--no-update",
            "--windows-filenames",
            "--continue",
            "--socket-timeout",
            "25",
            "--retries",
            "3",
            "--fragment-retries",
            "3",
            "--quiet",
            "--progress",
            "--newline",
            "--no-mtime",
            "--no-overwrites",
            "--no-simulate",
            "--encoding",
            "UTF-8",
            "--progress-template",
            concat!(
                "download:NDM_PROGRESS %(progress.filename,info_dict.format_id)j\t",
                "%(progress.{downloaded_bytes,total_bytes,total_bytes_estimate})j\t",
                "%(info_dict.{filesize,filesize_approx,is_live,tbr,duration})j",
            ),
            "--progress-template",
            "postprocess:NDM_FINALIZING %(progress.status)s",
            "--print",
            "after_move:NDM_RESULT %(filepath)j\t%(requested_subtitles.:.filepath)j",
            "--batch-file",
            "-",
        ])
        .arg("--concurrent-fragments")
        .arg(
            request
                .download_threads
                .map_or(1, |n| n.get().min(16))
                .to_string(),
        );
    let playlist = request.options.get("playlist").and_then(Value::as_bool) == Some(true);
    let subtitles = configure_subtitles(&mut command, &request.options)?;
    configure_proxy(&mut command, &request.options)?;
    if !playlist {
        command.args(["--playlist-items", "1"]);
    }
    command
        .arg(if playlist {
            "--yes-playlist"
        } else {
            "--no-playlist"
        })
        .arg("--print")
        .arg(if playlist {
            "before_dl:NDM_TITLE %(playlist_title,title)j\t%(requested_formats.:.format_id)j"
        } else {
            "before_dl:NDM_TITLE %(title)j\t%(requested_formats.:.format_id)j"
        })
        .arg("--output")
        .arg(if playlist || subtitles {
            "%(playlist_title,title).120B [%(playlist_id,id).64B]/%(title).120B [%(id).64B].%(ext)s"
        } else {
            "%(title).150B [%(id).64B].%(ext)s"
        });
    if deno.is_file() {
        command
            .arg("--no-js-runtimes")
            .arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()));
    }
    configure_video(&mut command, &request.options, media.as_deref())?;
    let headers = web_headers(headers, &request.options)?;
    let _header_config = if headers.is_empty() {
        None
    } else {
        let mut file = tempfile::NamedTempFile::new_in(&request.workspace)
            .map_err(|_| FailureReason::Storage)?;
        writeln!(file, "# coding: utf-8").map_err(|_| FailureReason::Storage)?;
        for (key, value) in headers {
            if key.chars().any(char::is_control) || value.chars().any(char::is_control) {
                return Err(FailureReason::InvalidSource);
            }
            let option = format!("{key}:{value}").replace('\'', "'\"'\"'");
            writeln!(file, "--add-headers '{option}'").map_err(|_| FailureReason::Storage)?;
        }
        command.arg("--config-locations").arg(file.path());
        Some(file)
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    // yt-dlp prints postprocessor updates on stderr and file paths on stdout.
    // Read both in order and only forward the protocol's typed messages.
    let (output, writer) = io::pipe().map_err(|_| FailureReason::Other)?;
    command
        .stdout(writer.try_clone().map_err(|_| FailureReason::Other)?)
        .stderr(writer);
    let mut child = command.spawn().map_err(|_| FailureReason::Other)?;
    drop(command);
    let input = child.stdin.take().ok_or(FailureReason::Other)?;
    if writeln!(&input, "{url}").is_err() {
        stop(&mut child);
        return Err(FailureReason::Other);
    }
    drop(input);
    let (send, lines) = mpsc::sync_channel(32);
    std::thread::spawn(move || {
        for line in BufReader::new(output).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut title_reported = false;
    let mut failure = FailureReason::Other;
    let mut progress = MediaProgress::default();
    loop {
        if worker.stopped() {
            stop(&mut child);
            return Err(FailureReason::Other);
        }
        let line = match lines.recv_timeout(Duration::from_millis(50)) {
            Ok(line) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some((title, formats)) = line
            .strip_prefix("NDM_TITLE ")
            .and_then(|line| line.split_once('\t'))
        {
            progress.track_count = serde_json::from_str::<Vec<String>>(formats)
                .map_or(1, |formats| formats.len().max(1));
            if !title_reported && let Ok(title) = serde_json::from_str::<String>(title) {
                title_reported = true;
                let _ = worker.update_task(TaskUpdate::Title { title });
            }
        } else if let Some(line) = line.strip_prefix("NDM_PROGRESS ") {
            if let Some((update, estimate)) = progress.update(line, playlist) {
                let _ = worker.progress_with_estimate(update, estimate);
            }
        } else if line == "NDM_FINALIZING started" {
            let _ = worker.update_task(TaskUpdate::Finalizing);
        } else if let Some(line) = line.strip_prefix("NDM_RESULT ") {
            let Ok(paths) = result_paths(line, &request.workspace) else {
                stop(&mut child);
                return Err(FailureReason::Storage);
            };
            for path in paths {
                if seen.insert(path.clone()) {
                    progress.finished = progress.finished.saturating_add(
                        fs::metadata(request.workspace.join(&path)).map_or(0, |m| m.len()),
                    );
                    files.push(path);
                }
            }
            progress.tracks.clear();
        } else if let Some(reason) = failure_reason(&line) {
            failure = reason;
        }
    }
    let status = child.wait().map_err(|_| FailureReason::Other)?;
    if !status.success() {
        return Err(failure);
    }
    if files.is_empty() {
        return Err(FailureReason::Other);
    }
    Ok(files)
}

fn web_headers(
    observed: &BTreeMap<String, String>,
    options: &Map<String, Value>,
) -> Result<BTreeMap<String, String>, FailureReason> {
    let mut headers: BTreeMap<_, _> = observed
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .collect();
    for (key, max) in [("user-agent", 1024), ("accept-language", 256)] {
        let Some(value) = options.get(key) else {
            continue;
        };
        let value = value.as_str().ok_or(FailureReason::InvalidSource)?;
        if value.is_empty() {
            continue;
        }
        if value.trim().is_empty()
            || value.len() > max
            || !value.bytes().all(|b| matches!(b, b' '..=b'~'))
        {
            return Err(FailureReason::InvalidSource);
        }
        headers
            .entry(key.to_owned())
            .or_insert_with(|| value.to_owned());
    }
    Ok(headers)
}

fn configure_subtitles(
    command: &mut Command,
    options: &Map<String, Value>,
) -> Result<bool, FailureReason> {
    let mode = options
        .get("subtitles")
        .and_then(Value::as_str)
        .unwrap_or("off");
    if mode == "off" {
        return Ok(false);
    }
    let languages = options
        .get("subtitle-languages")
        .and_then(Value::as_str)
        .unwrap_or("en,zh-Hans,zh-Hant");
    if languages.trim().is_empty() {
        return Ok(false);
    }
    let languages: Vec<_> = languages.split(',').map(str::trim).collect();
    if languages.len() > 16
        || languages.iter().any(|language| {
            let mut parts = language.split('-');
            let base = parts.next().unwrap_or_default();
            !(2..=3).contains(&base.len())
                || !base.bytes().all(|c| c.is_ascii_alphabetic())
                || base.eq_ignore_ascii_case("all")
                || parts.any(|part| {
                    !(1..=8).contains(&part.len())
                        || !part.bytes().all(|c| c.is_ascii_alphanumeric())
                })
        })
    {
        return Err(FailureReason::InvalidOptions);
    }
    match mode {
        "manual" => command.arg("--write-subs"),
        "auto" => command.args(["--write-subs", "--write-auto-subs"]),
        _ => return Err(FailureReason::InvalidOptions),
    };
    command
        .arg("--sub-langs")
        .arg(languages.join(","))
        .args(["--sub-format", "vtt/srt/best"]);
    Ok(true)
}

fn configure_proxy(
    command: &mut Command,
    options: &Map<String, Value>,
) -> Result<(), FailureReason> {
    if options.get("proxy-enabled").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }
    let address = options
        .get("proxy-url")
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or(FailureReason::InvalidOptions)?;
    let proxy = ndm_plugin_sdk::normalize_proxy_address(
        address,
        "http",
        &["http", "https", "socks4", "socks4a", "socks5", "socks5h"],
    )
    .map_err(|_| FailureReason::InvalidOptions)?;
    // Set it on this process tree only. Keep proxy credentials out of arguments and temporary files.
    command.env_remove("NO_PROXY").env_remove("no_proxy");
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        command.env(key, &proxy);
    }
    Ok(())
}

fn failure_reason(line: &str) -> Option<FailureReason> {
    let line = line
        .trim_start()
        .strip_prefix("ERROR:")?
        .to_ascii_lowercase();
    if [
        "no space left",
        "not enough space",
        "permission denied",
        "access is denied",
        "cannot write",
    ]
    .iter()
    .any(|message| line.contains(message))
    {
        Some(FailureReason::Storage)
    } else if [
        "postprocessing:",
        "ffmpeg not found",
        "ffmpeg is not installed",
    ]
    .iter()
    .any(|message| line.contains(message))
    {
        Some(FailureReason::PostProcessing)
    } else if [
        "sign in to confirm",
        "login required",
        "authentication required",
        "log in to view",
        "private video",
        "http error 401",
        "http error 407",
    ]
    .iter()
    .any(|message| line.contains(message))
    {
        Some(FailureReason::Authentication)
    } else if [
        "http error",
        "unable to download webpage",
        "unable to download video data",
        "connection refused",
        "connection reset",
        "timed out",
        "proxyerror",
    ]
    .iter()
    .any(|message| line.contains(message))
    {
        Some(FailureReason::Network)
    } else if [
        "unsupported url",
        "no video could be found",
        "no video formats found",
        "requested format is not available",
        "drm protected",
        "drm-protected",
    ]
    .iter()
    .any(|message| line.contains(message))
    {
        Some(FailureReason::Unsupported)
    } else {
        None
    }
}

fn result_paths(line: &str, workspace: &Path) -> io::Result<Vec<String>> {
    let (media, subtitles) = line
        .split_once('\t')
        .ok_or_else(|| io::Error::other("missing output fields"))?;
    let media: String = serde_json::from_str(media)?;
    let subtitles: Vec<String> = if subtitles == "NA" {
        Vec::new()
    } else {
        serde_json::from_str(subtitles)?
    };
    std::iter::once(media)
        .chain(subtitles)
        .map(|file| output_path(workspace, Path::new(&file)))
        .collect()
}

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn configure_video(
    command: &mut Command,
    options: &Map<String, Value>,
    media: Option<&Path>,
) -> Result<(), FailureReason> {
    let quality = options
        .get("quality")
        .and_then(Value::as_str)
        .unwrap_or("best");
    if let Some(media) = media {
        command.arg("--ffmpeg-location").arg(media);
        // Local downloads can play with the MP4 index at the end. Skip the
        // extra pass that relocates the index for web playback.
        command.args(["--postprocessor-args", "ffmpeg:-movflags -faststart"]);
    }
    if quality == "audio" {
        command.args(["--format", "bestaudio"]);
        return Ok(());
    }
    let container = options
        .get("video-container")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let codec = options
        .get("video-codec")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    let codec_sort = match codec {
        "auto" => None,
        "h265" => Some("res,vcodec:h265"),
        "h264" => Some("res,vcodec:h264"),
        _ => return Err(FailureReason::InvalidOptions),
    };
    if let Some(sort) = codec_sort.filter(|_| container != "webm") {
        command.args(["--format-sort-force", "--format-sort", sort]);
    }
    let webm = matches!(container, "auto" | "webm") && media.is_some_and(supports_webm);
    let can_mux = media.is_some() && (container != "webm" || webm);
    command
        .arg("--format")
        .arg(format_expression(quality, container, can_mux)?);
    if can_mux {
        if container == "auto" {
            if !webm {
                // Installed desktop versions with the older muxer set can still copy the original streams.
                command.args(["--merge-output-format", "mp4/mkv"]);
            }
        } else {
            command.args([
                "--merge-output-format",
                container,
                "--remux-video",
                container,
            ]);
        }
    }
    Ok(())
}

fn supports_webm(media: &Path) -> bool {
    let mut command = Command::new(media.join(executable("ffmpeg")));
    command
        .args(["-hide_banner", "-muxers"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.output().is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.split_whitespace().nth(1) == Some("webm"))
    })
}

fn format_expression(
    quality: &str,
    container: &str,
    can_mux: bool,
) -> Result<String, FailureReason> {
    let height = match quality {
        "best" => String::new(),
        "2160" | "1440" | "1080" | "720" | "480" => format!("[height<={quality}]"),
        _ => return Err(FailureReason::InvalidOptions),
    };
    let (video, audio) = match container {
        "mp4" => (
            "[vcodec~='^(avc|h264|hevc|h265|hvc1|hev1|dvhe|dvh1|av0?1|vp0?9|mp4v)']",
            "[acodec~='^(mp4a|aac|opus|mp3|ac-?3|ec-?3|eac3|alac)']",
        ),
        "webm" => (
            "[vcodec~='^(vp0?8|vp0?9|av0?1)']",
            "[acodec~='^(opus|vorbis|vrbs)']",
        ),
        "auto" | "mkv" => ("", ""),
        _ => return Err(FailureReason::InvalidOptions),
    };
    let direct = if container == "auto" {
        format!("b{height}")
    } else {
        format!("b[ext={container}]{height}")
    };
    if !can_mux {
        return Ok(direct);
    }
    if video.is_empty() {
        // yt-dlp's best format also covers sources with just one track type.
        return Ok(format!("bv*{height}+ba/b{height}"));
    }
    Ok(format!(
        "bv{height}{video}+ba{audio}/{direct}/b{height}{video}{audio}"
    ))
}

#[derive(Default)]
struct MediaProgress {
    tracks: BTreeMap<String, TrackProgress>,
    track_count: usize,
    transferred: u64,
    finished: u64,
}

struct TrackProgress {
    downloaded: u64,
    exact: Option<u64>,
    estimate: Option<u64>,
}

impl MediaProgress {
    fn update(&mut self, line: &str, playlist: bool) -> Option<(TransferProgress, Option<u64>)> {
        let (name, values) = line.split_once('\t')?;
        let (progress, information) = values.split_once('\t')?;
        let name = serde_json::from_str::<String>(name).unwrap_or_default();
        let progress = serde_json::from_str::<Value>(progress).ok()?;
        let information = serde_json::from_str::<Value>(information).unwrap_or_default();
        let downloaded = progress["downloaded_bytes"].as_u64()?;
        let exact = progress["total_bytes"]
            .as_u64()
            .or_else(|| information["filesize"].as_u64())
            .filter(|&size| size >= downloaded);
        let estimate = (exact.is_none() && information["is_live"].as_bool() != Some(true))
            .then(|| {
                progress["total_bytes_estimate"]
                    .as_f64()
                    .or_else(|| information["filesize_approx"].as_f64())
                    .or_else(|| {
                        // yt-dlp reports stream bitrate in kilobits per second.
                        Some(
                            information["tbr"].as_f64()? * information["duration"].as_f64()? * 125.,
                        )
                    })
                    .filter(|size| (1. ..u64::MAX as f64).contains(size))
                    .map(|size| size.round() as u64)
                    .filter(|&size| size >= downloaded)
            })
            .flatten();
        let previous = self.tracks.insert(
            name,
            TrackProgress {
                downloaded,
                exact,
                estimate,
            },
        );
        self.transferred = self.transferred.saturating_add(
            downloaded.saturating_sub(previous.map_or(0, |track| track.downloaded)),
        );
        let downloaded = self.tracks.values().fold(self.finished, |bytes, track| {
            bytes.saturating_add(track.downloaded)
        });
        let has_all_tracks = !playlist && self.tracks.len() == self.track_count.max(1);
        let mut total = None;
        let mut estimated_total = None;
        if has_all_tracks {
            total = self
                .tracks
                .values()
                .try_fold(self.finished, |bytes, track| {
                    Some(bytes.saturating_add(track.exact?))
                });
            if total.is_none() {
                estimated_total = self
                    .tracks
                    .values()
                    .try_fold(self.finished, |bytes, track| {
                        Some(bytes.saturating_add(track.exact.or(track.estimate)?))
                    });
            }
        }
        Some((
            TransferProgress {
                downloaded,
                total,
                transferred: self.transferred,
            },
            estimated_total,
        ))
    }
}

fn output_path(workspace: &Path, file: &Path) -> io::Result<String> {
    let workspace = workspace.canonicalize()?;
    let file = if file.is_absolute() {
        file.to_owned()
    } else {
        workspace.join(file)
    }
    .canonicalize()?;
    let relative = file.strip_prefix(workspace).map_err(io::Error::other)?;
    relative
        .components()
        .map(|part| match part {
            Component::Normal(part) => part
                .to_str()
                .ok_or_else(|| io::Error::other("output filename must be UTF-8")),
            _ => Err(io::Error::other("invalid output filename")),
        })
        .collect::<io::Result<Vec<_>>>()
        .map(|parts| parts.join("/"))
}

fn stop(child: &mut Child) {
    #[cfg(unix)]
    {
        // SAFETY: This PID belongs to our live child until it has been waited on.
        unsafe {
            libc::kill(child.id() as i32, libc::SIGINT);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_manifest_is_accepted_by_the_host() {
        let manifest: ndm_plugin_sdk::PluginManifest =
            serde_json::from_str(include_str!("../plugin.json")).unwrap();
        manifest.validate().unwrap();
        assert!(manifest.capabilities.browser_page);
        for host in [
            "www.youtube.com",
            "www.bilibili.com",
            "vimeo.com",
            "soundcloud.com",
        ] {
            assert!(
                manifest.inputs[0]
                    .score("https", Some(host), "/watch")
                    .is_some()
            );
        }
        assert!(
            manifest.inputs[0]
                .score("https", Some("vimeo.com"), "/live.m3u8")
                .is_none()
        );
        assert!(
            manifest.manual_inputs[0]
                .score("https", Some("example.test"), "/page")
                .is_some()
        );
        assert!(
            manifest.manual_inputs[0]
                .score("https", Some("example.test"), "/file.torrent")
                .is_none()
        );
    }

    #[test]
    fn permits_manifest_qualities_and_reports_unknown_length() {
        assert_eq!(
            format_expression("720", "auto", true).unwrap(),
            "bv*[height<=720]+ba/b[height<=720]"
        );
        assert_eq!(
            format_expression("2160", "auto", false).unwrap(),
            "b[height<=2160]"
        );
        assert_eq!(
            format_expression("720", "webm", false).unwrap(),
            "b[ext=webm][height<=720]"
        );
        assert!(
            format_expression("1080", "mp4", true)
                .unwrap()
                .contains("opus")
        );
        assert!(format_expression("--exec", "auto", true).is_err());
        assert!(format_expression("best", "--exec", true).is_err());
    }

    #[test]
    fn reports_fragment_estimates_and_keeps_exact_size_separate() {
        let mut progress = MediaProgress::default();
        let (update, estimated) = progress
            .update(
                r#""clip.mp4"	{"downloaded_bytes":64,"total_bytes_estimate":193.6}	{}"#,
                false,
            )
            .unwrap();
        assert_eq!(update.downloaded, 64);
        assert_eq!(update.total, None);
        assert_eq!(estimated, Some(194));
        let (update, estimated) = progress
            .update(
                r#""clip.mp4"	{"downloaded_bytes":162,"total_bytes":162}	{}"#,
                false,
            )
            .unwrap();
        assert_eq!((update.total, estimated), (Some(162), None));
        assert_eq!(update.transferred, 162);
        let (_, estimated) = progress
            .update(r#""live.mp4"	{"downloaded_bytes":64,"total_bytes_estimate":194.0}	{"is_live":true}"#, false)
            .unwrap();
        assert_eq!(estimated, None);
    }

    #[test]
    fn reports_the_size_of_all_selected_streams() {
        let mut progress = MediaProgress {
            track_count: 2,
            ..Default::default()
        };
        let (video, estimated) = progress
            .update(
                r#""video"	{"downloaded_bytes":600,"total_bytes":600}	{}"#,
                false,
            )
            .unwrap();
        assert_eq!((video.total, estimated), (None, None));
        let audio = r#""audio"	{"downloaded_bytes":20}	{"tbr":12.5,"duration":0.1}"#;
        let (update, estimated) = progress.update(audio, false).unwrap();
        assert_eq!(update.downloaded, 620);
        assert_eq!(estimated, Some(756));
        let (playlist, estimated) = progress.update(audio, true).unwrap();
        assert_eq!((playlist.total, estimated), (None, None));
    }

    #[test]
    fn distinguishes_muxer_failures_from_proxy_and_disk_errors() {
        assert_eq!(
            failure_reason("ERROR: Postprocessing: Muxer not found"),
            Some(FailureReason::PostProcessing),
        );
        assert_eq!(
            failure_reason("ERROR: Postprocessing: No space left on device"),
            Some(FailureReason::Storage),
        );
        assert_eq!(
            failure_reason("ERROR: HTTP Error 407: Proxy authentication required"),
            Some(FailureReason::Authentication),
        );
        assert_eq!(
            failure_reason("ERROR: Unsupported URL: https://example.test/library"),
            Some(FailureReason::Unsupported),
        );
        assert_eq!(
            failure_reason("ERROR: The video is DRM protected"),
            Some(FailureReason::Unsupported),
        );
    }

    #[test]
    fn mobile_settings_supply_missing_headers_and_keep_captured_values() {
        let mut options = Map::from_iter([
            ("user-agent".into(), "Mobile Safari preset".into()),
            ("accept-language".into(), "zh-CN,zh;q=0.9".into()),
        ]);
        let defaults = web_headers(&BTreeMap::new(), &options).unwrap();
        assert_eq!(defaults["user-agent"], "Mobile Safari preset");
        assert_eq!(defaults["accept-language"], "zh-CN,zh;q=0.9");
        let observed = BTreeMap::from([
            ("User-Agent".into(), "Browser device".into()),
            ("Referer".into(), "https://example.org/watch".into()),
        ]);
        let headers = web_headers(&observed, &options).unwrap();
        assert_eq!(headers["user-agent"], "Browser device");
        assert_eq!(headers["referer"], "https://example.org/watch");
        assert_eq!(headers["accept-language"], "zh-CN,zh;q=0.9");
        options.insert("accept-language".into(), "en\r\nCookie: rejected".into());
        assert_eq!(
            web_headers(&observed, &options),
            Err(FailureReason::InvalidSource)
        );
    }

    #[test]
    fn validates_enabled_proxy_and_subtitle_language_options() {
        let mut command = Command::new("yt-dlp");
        let mut options = Map::new();
        assert!(configure_proxy(&mut command, &options).is_ok());
        options.insert("proxy-enabled".into(), true.into());
        assert_eq!(
            configure_proxy(&mut command, &options),
            Err(FailureReason::InvalidOptions)
        );
        options.insert("proxy-url".into(), "socks5h://127.0.0.1:1080".into());
        assert!(configure_proxy(&mut command, &options).is_ok());
        options.insert("proxy-url".into(), "127.0.0.1:8080".into());
        assert!(configure_proxy(&mut command, &options).is_ok());
        options.insert("subtitles".into(), "auto".into());
        options.insert("subtitle-languages".into(), "en, zh-Hans".into());
        assert_eq!(configure_subtitles(&mut command, &options), Ok(true));
        options.insert("subtitle-languages".into(), "en,--exec".into());
        assert_eq!(
            configure_subtitles(&mut command, &options),
            Err(FailureReason::InvalidOptions)
        );
    }
}
