#[cfg(unix)]
use std::time::Instant;
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};

use ndm_plugin_sdk::{
    DownloadRequest, DownloadSource, ENGINE_ARGUMENT, FailureReason, MEDIA_TOOLS_ENV,
    TransferProgress, Worker, serve,
};
use serde_json::Value;

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
    command.current_dir(&request.workspace).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .args(["--ignore-config", "--no-cache-dir", "--no-update", "--windows-filenames", "--continue",
            "--socket-timeout", "25", "--retries", "3", "--fragment-retries", "3", "--quiet",
            "--progress", "--newline", "--no-mtime", "--no-overwrites",
            "--output", "%(title).150B [%(id)s].%(ext)s",
            "--progress-template", "download:NDM_PROGRESS %(progress.downloaded_bytes)j %(progress.total_bytes,progress.total_bytes_estimate)j",
            "--print", "after_move:NDM_RESULT %(filepath)j",
            "--batch-file", "-",
        ]).arg("--paths").arg(&request.workspace)
        .arg("--concurrent-fragments").arg(request.download_threads.map_or(1, |n| n.get().min(16)).to_string());
    command.arg(
        if request
            .options
            .get("playlist")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "--yes-playlist"
        } else {
            "--no-playlist"
        },
    );
    if deno.is_file() {
        command
            .arg("--no-js-runtimes")
            .arg("--js-runtimes")
            .arg(format!("deno:{}", deno.display()));
    }
    if let Some(media) = &media {
        command.arg("--ffmpeg-location").arg(media);
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
    let mut child = command.spawn().map_err(|_| FailureReason::Other)?;
    let input = child.stdin.take().ok_or(FailureReason::Other)?;
    if writeln!(&input, "{url}").is_err() {
        stop(&mut child);
        return Err(FailureReason::Other);
    }
    drop(input);
    let stdout = child.stdout.take().ok_or(FailureReason::Other)?;
    let (send, lines) = mpsc::sync_channel(32);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                break;
            }
        }
    });
    let mut files = Vec::new();
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
        if let Some(line) = line.strip_prefix("NDM_PROGRESS ") {
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
        } else if let Some(path) = line.strip_prefix("NDM_RESULT ")
            && let Ok(path) = serde_json::from_str::<String>(path)
            && let Ok(relative) = output_path(&request.workspace, Path::new(&path))
            && !files.contains(&relative)
        {
            finished_bytes = finished_bytes.saturating_add(
                fs::metadata(request.workspace.join(&relative)).map_or(0, |m| m.len()),
            );
            highwater = 0;
            files.push(relative);
        }
    }
    let status = child.wait().map_err(|_| FailureReason::Other)?;
    if !status.success() {
        return Err(FailureReason::Network);
    }
    if files.is_empty() {
        return Err(FailureReason::Other);
    }
    Ok(files)
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
        _ => Err(FailureReason::InvalidSource),
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
    fn permits_manifest_qualities_and_reports_unknown_length() {
        assert_eq!(
            format_expression("720", true).unwrap(),
            "bv*[height<=720]+ba/b[height<=720]"
        );
        assert_eq!(parse_progress("64 null"), (Some(64), None));
        assert_eq!(parse_progress("1024 2048"), (Some(1024), Some(2048)));
        assert!(format_expression("--exec", true).is_err());
    }
}
