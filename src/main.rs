#[cfg(unix)]
use std::time::Instant;
use std::{
    collections::HashSet,
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
            "run this engine through NDM Next",
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
    command.current_dir(&request.workspace).stdin(Stdio::piped())
        .args(["--ignore-config", "--no-cache-dir", "--no-update", "--windows-filenames", "--continue",
            "--socket-timeout", "25", "--retries", "3", "--fragment-retries", "3", "--quiet",
            "--progress", "--newline", "--no-mtime", "--no-overwrites", "--no-simulate", "--encoding", "UTF-8",
            "--progress-template", "download:NDM_PROGRESS %(progress.downloaded_bytes)j %(progress.total_bytes,progress.total_bytes_estimate)j",
            "--progress-template", "postprocess:NDM_FINALIZING %(progress.status)s",
            "--print", "after_move:NDM_RESULT %(filepath)j\t%(requested_subtitles.:.filepath)j",
            "--batch-file", "-",
        ])
        .arg("--concurrent-fragments").arg(request.download_threads.map_or(1, |n| n.get().min(16)).to_string());
    let playlist = request.options.get("playlist").and_then(Value::as_bool) == Some(true);
    let subtitles = configure_subtitles(&mut command, &request.options)?;
    configure_proxy(&mut command, &request.options)?;
    command
        .arg(if playlist {
            "--yes-playlist"
        } else {
            "--no-playlist"
        })
        .arg("--print")
        .arg(if playlist {
            "before_dl:NDM_TITLE %(playlist_title,title)j"
        } else {
            "before_dl:NDM_TITLE %(title)j"
        })
        .arg("--output")
        .arg(if playlist || subtitles {
            "%(playlist_title,title).120B [%(playlist_id,id)s]/%(title).120B [%(id)s].%(ext)s"
        } else {
            "%(title).150B [%(id)s].%(ext)s"
        });
    if deno.is_file() {
        command
            .arg("--no-js-runtimes")
            .arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()));
    }
    if let Some(media) = &media {
        // The desktop's local-only FFmpeg includes Matroska, including on Windows and macOS.
        command
            .arg("--ffmpeg-location")
            .arg(media)
            .args(["--merge-output-format", "mkv"]);
    }
    let quality = request
        .options
        .get("quality")
        .and_then(Value::as_str)
        .unwrap_or("best");
    command
        .arg("--format")
        .arg(format_expression(quality, media.is_some())?);
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
    let mut highwater = 0u64;
    let mut transferred = 0u64;
    let mut finished_bytes = 0u64;
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
        if let Some(title) = line.strip_prefix("NDM_TITLE ") {
            if !title_reported && let Ok(title) = serde_json::from_str::<String>(title) {
                title_reported = true;
                let _ = worker.update_task(TaskUpdate::Title { title });
            }
        } else if let Some(line) = line.strip_prefix("NDM_PROGRESS ") {
            let (downloaded, total) = parse_progress(line);
            if let Some(downloaded) = downloaded {
                transferred = transferred.saturating_add(downloaded.saturating_sub(highwater));
                highwater = downloaded;
                let _ = worker.progress(TransferProgress {
                    downloaded: finished_bytes.saturating_add(downloaded),
                    total: total.map(|total| finished_bytes.saturating_add(total)),
                    transferred,
                });
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
                    finished_bytes = finished_bytes.saturating_add(
                        fs::metadata(request.workspace.join(&path)).map_or(0, |m| m.len()),
                    );
                    files.push(path);
                }
            }
            highwater = 0;
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

fn configure_subtitles(
    command: &mut Command,
    options: &Map<String, Value>,
) -> Result<bool, FailureReason> {
    match options
        .get("subtitles")
        .and_then(Value::as_str)
        .unwrap_or("off")
    {
        "off" => return Ok(false),
        "manual" => command.arg("--write-subs"),
        "auto" => command.args(["--write-subs", "--write-auto-subs"]),
        _ => return Err(FailureReason::InvalidOptions),
    };
    let languages = options
        .get("subtitle-languages")
        .and_then(Value::as_str)
        .unwrap_or("en,zh-Hans,zh-Hant");
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
    if !address.contains("://") || address.chars().any(char::is_control) {
        return Err(FailureReason::InvalidOptions);
    }
    let proxy = url::Url::parse(address).map_err(|_| FailureReason::InvalidOptions)?;
    if !matches!(
        proxy.scheme(),
        "http" | "https" | "socks4" | "socks4a" | "socks5" | "socks5h"
    ) || proxy.host_str().is_none()
        || !matches!(proxy.path(), "" | "/")
        || proxy.query().is_some()
        || proxy.fragment().is_some()
    {
        return Err(FailureReason::InvalidOptions);
    }
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
        command.env(key, proxy.as_str());
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

fn format_expression(quality: &str, has_media_tools: bool) -> Result<String, FailureReason> {
    match quality {
        "audio" => Ok("bestaudio/best".into()),
        "best" => Ok(if has_media_tools { "bv*+ba/b" } else { "b" }.into()),
        "1080" | "720" | "480" => Ok(if has_media_tools {
            format!("bv*[height<={quality}]+ba/b[height<={quality}]")
        } else {
            format!("b[height<={quality}]")
        }),
        _ => Err(FailureReason::InvalidOptions),
    }
}

fn parse_progress(line: &str) -> (Option<u64>, Option<u64>) {
    let mut values = line.split_whitespace();
    let downloaded = values.next().and_then(|value| value.parse().ok());
    let total = values.next().and_then(|value| value.parse().ok());
    (downloaded, total)
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
    }

    #[test]
    fn permits_manifest_qualities_and_reports_unknown_length() {
        assert_eq!(
            format_expression("720", true).unwrap(),
            "bv*[height<=720]+ba/b[height<=720]"
        );
        assert_eq!(parse_progress("64 null"), (Some(64), None));
        assert_eq!(parse_progress("1024 2048"), (Some(1024), Some(2048)));
        assert!(format_expression("--exec", true).is_err());
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
