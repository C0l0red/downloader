use axum::{
    Router,
    extract::{Json, Query},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::{self, Write},
    process::{Command, Stdio},
};
use url::Url;

const MAX_FORMAT_OPTIONS: usize = 10;

#[tokio::main]
async fn main() -> Result<(), Error> {
    if std::env::args().nth(1).as_deref() == Some("server") {
        return run_server(std::env::args().nth(2)).await;
    }

    run_cli().await
}

// Error and data types

#[derive(Debug)]
enum Error {
    YtDlpExecutionFailed(String),
    JqExecutionFailed(String),
    FormatDeserializationFailed(String),
    InvalidInput(String),
    IO(io::Error),
    UnsupportedPlatform(String),
}

#[derive(Deserialize, Serialize, Debug)]
struct Format {
    format_id: String,
    #[serde(skip_serializing_if = "is_zero")]
    height: u16,
    #[serde(skip_serializing)]
    ext: String,
    #[serde(skip_serializing)]
    vcodec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    filesize: Option<u64>,
}

fn is_zero(value: &u16) -> bool {
    *value == 0
}

impl Format {
    fn best() -> Self {
        Self {
            format_id: "best".to_owned(),
            height: 0,
            ext: String::new(),
            vcodec: String::new(),
            filesize: None,
        }
    }
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

#[derive(Deserialize, Debug)]
struct FormatsRequest {
    url: String,
}

#[derive(Deserialize, Debug)]
struct DownloadRequest {
    url: String,
    format_id: String,
}

// Platform downloaders

trait PlatformDownloader {
    fn list_formats(&self, _url: &str) -> Result<Vec<Format>, Error> {
        Ok(vec![Format::best()])
    }

    fn download(&self, url: &str, _format_id: &str) -> Result<(), Error> {
        Command::new("yt-dlp")
            .args(["-f", "best", url])
            .output()
            .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))?;

        Ok(())
    }
}

struct YouTubeDownloader;

impl YouTubeDownloader {
    const COOKIES_FILE: &'static str = "youtube-cookies.txt";
}

struct FacebookDownloader;
struct InstagramDownloader;

impl InstagramDownloader {
    const COOKIES_FILE: &'static str = "instagram-cookies.txt";
}

struct XDownloader;
struct TikTokDownloader;

impl PlatformDownloader for YouTubeDownloader {
    fn list_formats(&self, url: &str) -> Result<Vec<Format>, Error> {
        let video_details = Command::new("yt-dlp")
            .arg("--dump-single-json")
            .arg("--skip-download")
            .arg("--cookies")
            .arg(Self::COOKIES_FILE)
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

    fn download(&self, url: &str, format_id: &str) -> Result<(), Error> {
        let formats = self.list_formats(url)?;
        let format_ids_set: HashSet<String> =
            formats.into_iter().map(|format| format.format_id).collect();

        if !format_ids_set.contains(format_id) {
            return Err(Error::InvalidInput(format!(
                "Format ID not found: {format_id}"
            )));
        }

        Command::new("yt-dlp")
            .args([
                "-f",
                &format!("{format_id}+ba"),
                "--merge-output-format",
                "mp4",
                "--cookies",
                Self::COOKIES_FILE,
                url,
            ])
            .output()
            .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))?;

        Ok(())
    }
}

impl PlatformDownloader for FacebookDownloader {}
impl PlatformDownloader for InstagramDownloader {
    fn download(&self, url: &str, _format_id: &str) -> Result<(), Error> {
        Command::new("yt-dlp")
            .args(["-f", "best", "--cookies", Self::COOKIES_FILE, url])
            .output()
            .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))?;

        Ok(())
    }
}
impl PlatformDownloader for XDownloader {}
impl PlatformDownloader for TikTokDownloader {}

// Interactive CLI

async fn run_cli() -> Result<(), Error> {
    let url = prompt("URL: ")?;

    let formats = list_formats(&url).await?;

    for (index, format) in formats.iter().enumerate() {
        println!(
            "{}. {}p - {}",
            index + 1,
            if format.height == 0 {
                "best".to_owned()
            } else {
                format.height.to_string()
            },
            format
                .filesize
                .map(|size| format!("{} MB", size / 1_000_000))
                .unwrap_or_else(|| "unknown size".into())
        );
    }

    let choice = prompt("Select format: ")?;

    let format = select_format(&formats, &choice)?;

    download_video(&url, &format.format_id).await?;

    println!("Download complete!");

    Ok(())
}

fn select_format<'a>(formats: &'a [Format], choice: &str) -> Result<&'a Format, Error> {
    let index: usize = choice
        .parse()
        .map_err(|_| Error::InvalidInput(format!("Invalid input: {choice} is not a number")))?;

    formats
        .get(index.checked_sub(1).unwrap_or(usize::MAX))
        .ok_or(Error::InvalidInput(format!(
            "Invalid input: {choice} is not a valid format index"
        )))
}

// Axum API

async fn run_server(address: Option<String>) -> Result<(), Error> {
    let address = address.unwrap_or_else(|| "0.0.0.0:3000".to_owned());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .map_err(Error::IO)?;
    let app = api_router();

    println!("Axum server listening on http://{address}");
    axum::serve(listener, app)
        .await
        .map_err(|error| Error::IO(io::Error::other(error)))
}

fn api_router() -> Router {
    Router::new()
        .route("/formats", get(list_formats_handler))
        .route("/download", get(download_handler))
}

async fn list_formats_handler(
    Query(request): Query<FormatsRequest>,
) -> Result<Json<Vec<Format>>, (StatusCode, String)> {
    list_formats(&request.url)
        .await
        .map(Json)
        .map_err(api_error)
}

async fn download_handler(
    Query(request): Query<DownloadRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    download_video(&request.url, &request.format_id)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(api_error)
}

fn api_error(error: Error) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, format!("{error:?}"))
}

fn prompt(message: &str) -> Result<String, Error> {
    print!("{message}");
    io::stdout().flush().map_err(|error| Error::IO(error))?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|error| Error::IO(error))?;

    Ok(input.trim().to_owned())
}

// Platform selection

enum Platform {
    YouTube,
    Facebook,
    Instagram,
    X,
    TikTok,
    Unsupported(String),
}

fn get_platform_from_url(url: &str) -> Result<Platform, Error> {
    let url = Url::parse(url)
        .map_err(|error| Error::InvalidInput(format!("Invalid URL '{url}': {error}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| Error::InvalidInput(format!("Invalid URL '{url}': No host")))?;

    match host {
        "youtube.com" | "www.youtube.com" | "youtu.be" => Ok(Platform::YouTube),
        "facebook.com" | "www.facebook.com" | "fb.watch" => Ok(Platform::Facebook),
        "instagram.com" | "www.instagram.com" => Ok(Platform::Instagram),
        "twitter.com" | "www.twitter.com" | "x.com" | "www.x.com" => Ok(Platform::X),
        "tiktok.com" | "www.tiktok.com" => Ok(Platform::TikTok),
        _ => Ok(Platform::Unsupported(host.to_string())),
    }
}

fn downloader_for_url(url: &str) -> Result<Box<dyn PlatformDownloader>, Error> {
    match get_platform_from_url(url)? {
        Platform::YouTube => Ok(Box::new(YouTubeDownloader)),
        Platform::Facebook => Ok(Box::new(FacebookDownloader)),
        Platform::Instagram => Ok(Box::new(InstagramDownloader)),
        Platform::X => Ok(Box::new(XDownloader)),
        Platform::TikTok => Ok(Box::new(TikTokDownloader)),
        Platform::Unsupported(host) => Err(Error::UnsupportedPlatform(host)),
    }
}

async fn list_formats(url: &str) -> Result<Vec<Format>, Error> {
    downloader_for_url(url)?.list_formats(url)
}

async fn download_video(url: &str, format_id: &str) -> Result<(), Error> {
    downloader_for_url(url)?.download(url, format_id)
}

#[cfg(test)]
mod api_tests {
    use super::*;

    #[tokio::test]
    async fn formats_endpoint_returns_default_format_for_supported_fallback_platform() {
        let response = list_formats_handler(Query(FormatsRequest {
            url: "https://www.facebook.com/video".to_owned(),
        }))
        .await
        .unwrap()
        .0;

        assert_eq!(response.len(), 1);
        assert_eq!(response[0].format_id, "best");
    }

    #[tokio::test]
    async fn formats_endpoint_rejects_invalid_url() {
        let error = list_formats_handler(Query(FormatsRequest {
            url: "not-a-url".to_owned(),
        }))
        .await
        .unwrap_err();

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn router_exposes_api_routes() {
        let _router = api_router();
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn format(format_id: &str) -> Format {
        Format {
            format_id: format_id.to_owned(),
            height: 360,
            ext: "mp4".to_owned(),
            vcodec: "h264".to_owned(),
            filesize: None,
        }
    }

    #[test]
    fn selects_format_by_one_based_cli_index() {
        let formats = vec![format("360"), format("best")];

        assert_eq!(select_format(&formats, "2").unwrap().format_id, "best");
    }

    #[test]
    fn rejects_non_numeric_cli_selection() {
        let error = select_format(&[format("360")], "best").unwrap_err();

        assert!(matches!(error, Error::InvalidInput(_)));
    }

    #[test]
    fn rejects_cli_selection_outside_available_formats() {
        let error = select_format(&[format("360")], "2").unwrap_err();

        assert!(matches!(error, Error::InvalidInput(_)));
    }
}

#[cfg(test)]
mod downloader_unit_tests {
    use super::*;

    #[test]
    fn default_downloader_returns_only_best_format_without_metadata() {
        let formats = FacebookDownloader
            .list_formats("https://www.facebook.com/video")
            .unwrap();

        assert_eq!(formats.len(), 1);
        assert_eq!(
            serde_json::to_value(&formats[0]).unwrap(),
            serde_json::json!({ "format_id": "best" })
        );
    }

    #[test]
    fn default_downloader_is_used_by_each_non_youtube_platform() {
        for downloader in [
            Box::new(FacebookDownloader) as Box<dyn PlatformDownloader>,
            Box::new(XDownloader),
            Box::new(TikTokDownloader),
        ] {
            assert_eq!(
                downloader
                    .list_formats("https://example.com/video")
                    .unwrap()[0]
                    .format_id,
                "best"
            );
        }
    }

    #[test]
    fn format_selection_keeps_the_closest_format_for_each_resolution() {
        let formats = vec![
            Format {
                format_id: "144".to_owned(),
                height: 144,
                ext: String::new(),
                vcodec: String::new(),
                filesize: None,
            },
            Format {
                format_id: "360".to_owned(),
                height: 360,
                ext: String::new(),
                vcodec: String::new(),
                filesize: None,
            },
            Format {
                format_id: "1080".to_owned(),
                height: 1080,
                ext: String::new(),
                vcodec: String::new(),
                filesize: None,
            },
        ];

        let selected = filter_non_null_formats(select_formats(formats));

        assert_eq!(
            selected
                .iter()
                .map(|format| format.format_id.as_str())
                .collect::<Vec<_>>(),
            ["144", "360", "1080"]
        );
    }
}
