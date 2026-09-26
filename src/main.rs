use std::collections::{ HashSet};
use serde::Deserialize;
use std::process::{Command, Stdio};
use std::io::{self, Write};
use url::Url;

const MAX_FORMAT_OPTIONS: usize = 10;

#[derive(Debug)]
enum Error {
    YtDlpExecutionFailed(String),
    JqExecutionFailed(String),
    FormatDeserializationFailed(String),
    InvalidInput(String),
    IO(io::Error),
    UnsupportedPlatform(String),
}

enum VideoResolution {
    P144,
    P240,
    P360,
    P480,
    P720,
    P1080,
    P1440,
    P2160,
    P4320,
}

#[derive(Deserialize, Debug)]
struct Format {
    format_id: String,
    height: u16,
    #[serde(skip_serializing)]
    ext: String,
    #[serde(skip_serializing)]
    vcodec: String,
    filesize: Option<u64>,
}

#[derive(Default, Debug)]
struct SelectedFormatOptions {
    closest_to_144p: Option<Format>,
    closest_to_240p: Option<Format>,
    closest_to_360p: Option<Format>,
    closest_to_480p: Option<Format>,
    closest_to_720p: Option<Format>,
    closest_to_1080p: Option<Format>,
    closest_to_1440p: Option<Format>,
    closest_to_2160p: Option<Format>,
    closest_to_4320p: Option<Format>,
    larger_than_4320p: Option<Format>,
}

fn select_formats(mut formats: Vec<Format>) -> SelectedFormatOptions {
    let mut selected_formats = SelectedFormatOptions::default();
    // Sort formats by height and then by filesize presence
    formats.sort_by(|a, b| {
        a.height
            .cmp(&b.height)
            .then_with(|| a.filesize.is_some().cmp(&b.filesize.is_some()))
    });

    for format in formats {
        match format.height {
            res if res <= 144 => selected_formats.closest_to_144p = Some(format),
            res if res <= 144 + 240 / 2 => selected_formats.closest_to_240p = Some(format),
            res if res <= 240 + 360 / 2 => selected_formats.closest_to_360p = Some(format),
            res if res <= 360 + 480 / 2 => selected_formats.closest_to_480p = Some(format),
            res if res <= 480 + 720 / 2 => selected_formats.closest_to_720p = Some(format),
            res if res <= 720 + 1080 / 2 => selected_formats.closest_to_1080p = Some(format),
            res if res <= 1080 + 1440 / 2 => selected_formats.closest_to_1440p = Some(format),
            res if res <= 1440 + 2160 / 2 => selected_formats.closest_to_2160p = Some(format),
            res if res <= 2160 + 4320 / 2 => selected_formats.closest_to_4320p = Some(format),
            _ => selected_formats.larger_than_4320p = Some(format),
        }
    }

    selected_formats
}

fn filter_non_null_formats(selected_format_options: SelectedFormatOptions) -> Vec<Format> {
    let selected_format_values: [Option<Format>; MAX_FORMAT_OPTIONS] = [
        selected_format_options.closest_to_144p,
        selected_format_options.closest_to_240p,
        selected_format_options.closest_to_360p,
        selected_format_options.closest_to_480p,
        selected_format_options.closest_to_720p,
        selected_format_options.closest_to_1080p,
        selected_format_options.closest_to_1440p,
        selected_format_options.closest_to_2160p,
        selected_format_options.closest_to_4320p,
        selected_format_options.larger_than_4320p,
    ];

    selected_format_values
        .into_iter()
        .filter(|format| format.is_some())
        .map(|format| format.unwrap())
        .collect()
}

struct DownloadFormat {
    resolution: VideoResolution,
}

struct RequestBody {
    url: String,
}

trait DownloaderClient {
    fn download();

    fn list_formats();
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let url = prompt("URL: ")?;

    match get_platform_from_url(&url) {
        Ok(platform) => match platform {
            Platform::Unsupported(host) => return Err(Error::UnsupportedPlatform(host)),
            _ => {}
        }
        Err(err) => return Err(err)
    }

    let formats = list_formats(&url).await?;

    for (index, format) in formats.iter().enumerate() {
        println!(
            "{}. {}p - {}",
            index + 1,
            format.height,
            format.filesize
                .map(|size| format!("{} MB", size / 1_000_000))
                .unwrap_or_else(|| "unknown size".into())
        );
    }

    let choice = prompt("Select format: ")?;

    let index: usize = choice.parse()
        .map_err(|_| Error::InvalidInput(format!("Invalid input: {choice} is not a number")))?;

    let format = formats
        .get(index - 1)
        .ok_or(Error::InvalidInput(format!("Invalid input: {choice} is not a valid format index")))?;

    download_video(&url, &format.format_id).await?;

    println!("Download complete!");

    Ok(())
}

fn prompt(message: &str) -> Result<String, Error> {
    print!("{message}");
    io::stdout().flush().map_err(|error| Error::IO(error))?;

    let mut input = String::new();
    io::stdin().read_line(&mut input).map_err(|error| Error::IO(error))?;

    Ok(input.trim().to_owned())
}

enum Platform {
    YouTube,
    Facebook,
    Instagram,
    X,
    TikTok,
    Unsupported(String),
}

fn get_platform_from_url(url: &str) -> Result<Platform, Error> {
    let url = Url::parse(url).map_err(|error| Error::InvalidInput(format!("Invalid URL '{url}': {error}")))?;
    let host = url.host_str().ok_or_else(|| Error::InvalidInput(format!("Invalid URL '{url}': No host")))?;

    match host {
        "youtube.com" | "www.youtube.com" | "youtu.be" => Ok(Platform::YouTube),
        "facebook.com" | "www.facebook.com" | "fb.watch" => Ok(Platform::Facebook),
        "instagram.com" | "www.instagram.com" => Ok(Platform::Instagram),
        "twitter.com" | "www.twitter.com" | "x.com" | "www.x.com" => Ok(Platform::X),
        "tiktok.com" | "www.tiktok.com" => Ok(Platform::TikTok),
        _ => Ok(Platform::Unsupported(host.to_string())),
    }
}

async fn list_formats(url: &str) -> Result<Vec<Format>, Error> {
    let video_details = Command::new("yt-dlp")
        .arg("--dump-single-json")
        .arg("--skip-download")
        .arg("--cookies")
        .arg("youtube-cookies.txt")
        .arg(url)
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))?;

    let formats_bytes = Command::new("jq")
        .arg(r#"[.formats[] | select(.vcodec != "none" and .height != null) | {format_id, height, ext, vcodec, filesize}]"#)
        .stdin(video_details.stdout.unwrap())
        .output()
        .map_err(|error| Error::JqExecutionFailed(error.to_string()))?;

    let formats: Vec<Format> = serde_json::from_slice(&formats_bytes.stdout)
        .map_err(|error| Error::FormatDeserializationFailed(error.to_string()))?;

    let selected_format_options = select_formats(formats);

    Ok(filter_non_null_formats(selected_format_options))
}

async fn download_video(url: &str, format_id: &str) -> Result<(), Error> {
    let formats = list_formats(&url).await?;
    let format_ids_set: HashSet<String> = formats.into_iter().map(|format| format.format_id).collect();

    if !format_ids_set.contains(format_id) {
        return Err(Error::InvalidInput(format!("Format ID not found: {}", format_id)));
    }

    Command::new("yt-dlp")
        .args([
            "-f",
            &format!("{}+ba", format_id),
            "--merge-output-format",
            "mp4",
            "--cookies",
            "youtube-cookies.txt",
            url,
        ])
        .output()
        .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))?;

    Ok(())
}

#[tokio::test]
async fn list_formats_returns_filtered_formats() {
    let url = "https://youtu.be/foO1QakhjZE?si=tRel8JwJ_kmZJoul";
    let formats = list_formats(url).await.unwrap();

    assert!(!formats.is_empty());
    assert_eq!(formats.get(0).unwrap().height, 144);
    assert_eq!(formats.len(), 8);
}

#[tokio::test]
async fn list_download_video_saves_file() {
    let url = "https://youtu.be/foO1QakhjZE?si=tRel8JwJ_kmZJoul";
    download_video(url, "401").await.unwrap();
}

#[test]
fn print_formats() {
    let url = "https://youtu.be/foO1QakhjZE?si=tRel8JwJ_kmZJoul";

    let video_details = Command::new("yt-dlp")
        .arg("--dump-single-json")
        .arg("--skip-download")
        .arg("--cookies")
        .arg("youtube-cookies.txt")
        .arg(url)
        .stdout(Stdio::piped())
        .spawn()
        .expect("Failed to execute yt-dlp");

    let formats = Command::new("jq")
        .arg(r#"[.formats[] | select(.vcodec != "none" and .height != null) | {format_id, height, ext, vcodec, filesize}]"#)
        // .stderr(Stdio::null()) // Suppresses unnecessary download messages
        .stdin(video_details.stdout.unwrap())
        .output()
        .expect("Failed to execute yt-dlp");

    if formats.status.success() {
        println!("Command successful");
    } else {
        println!("Command failed");
    }

    let formats: Vec<Format> = serde_json::from_slice(&formats.stdout).unwrap();
    println!("Formats: {:?}", formats);

    let selected_format_options = select_formats(formats);
    println!("Selected formats options: {:?}", selected_format_options);

    let filtered_formats = filter_non_null_formats(selected_format_options);
    println!("Filtered formats: {:?}", filtered_formats);
}

#[test]
fn download() {
    let url = "https://youtu.be/foO1QakhjZE?si=tRel8JwJ_kmZJoul";
    // let max_height = 1080;
    let video_format_id = 401;

    let output = Command::new("yt-dlp")
        .args([
            "-f",
            // &format!("bv*[height<={max_height}]+ba"),
            &format!("{}+ba", video_format_id),
            "--merge-output-format",
            "mp4",
            "--cookies",
            "youtube-cookies.txt",
            url,
        ])
        .output()
        .expect("Failed to download video with yt-dlp");

    if output.status.success() {
        println!("Video download successful");
    } else {
        println!("Video download failed");
    }
}
