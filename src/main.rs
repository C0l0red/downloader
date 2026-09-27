use axum::{
    Router,
    body::Body,
    extract::{Json, Query},
    http::StatusCode,
    response::{Html, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::array::IntoIter;
use std::path::Path;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use std::{
    collections::HashMap,
    io::{self, Write},
    process::{Command, Stdio},
    sync::{LazyLock, RwLock},
};
use url::Url;

const MAX_FORMAT_OPTIONS: usize = 10;
const DEFAULT_DOWNLOADS_DIRECTORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/downloads");
static DOWNLOADS_DIRECTORY: LazyLock<String> = LazyLock::new(|| {
    dotenvy::dotenv().ok();
    std::env::var("DOWNLOADS_DIRECTORY")
        .ok()
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| DEFAULT_DOWNLOADS_DIRECTORY.to_owned())
});
const DEFAULT_ADDRESS: &str = "0.0.0.0:3424";

static FORMATS_CACHE: LazyLock<RwLock<HashMap<String, Vec<Format>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[tokio::main]
async fn main() -> Result<(), Error> {
    ensure_directory_exists(&DOWNLOADS_DIRECTORY)?;
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

#[derive(Clone, Deserialize, Serialize, Debug)]
struct Format {
    format_id: String,
    height: u16,
    ext: String,
    vcodec: String,
    acodec: String,
    filesize: Option<u64>,
}

#[derive(Debug, Serialize)]
struct FormatResponse {
    format_id: String,
    height: u16,
    filesize: Option<u64>,
}

impl From<Format> for FormatResponse {
    fn from(format: Format) -> Self {
        Self {
            format_id: format.format_id,
            height: format.height,
            filesize: format.filesize,
        }
    }
}

fn ensure_directory_exists(path: &str) -> Result<(), Error> {
    if !Path::new(path).exists() {
        std::fs::create_dir_all(path).map_err(Error::IO)?;
    }
    Ok(())
}

/// Gets the download file name template. yt-dlp will substitute id and ext with the corresponding values for each file.
fn get_download_file_path() -> String {
    format!(
        "{}/%(id)s.%(ext)s",
        DOWNLOADS_DIRECTORY.trim_end_matches('/')
    )
}

impl Format {
    fn best() -> Self {
        Self {
            format_id: "best".to_owned(),
            height: 0,
            ext: String::new(),
            vcodec: String::new(),
            acodec: String::new(),
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

impl IntoIterator for SelectedFormatOptions {
    type Item = Option<Format>;
    type IntoIter = IntoIter<Option<Format>, MAX_FORMAT_OPTIONS>;

    fn into_iter(self) -> Self::IntoIter {
        [
            self.closest_to_144p,
            self.closest_to_240p,
            self.closest_to_360p,
            self.closest_to_480p,
            self.closest_to_720p,
            self.closest_to_1080p,
            self.closest_to_1440p,
            self.closest_to_2160p,
            self.closest_to_4320p,
            self.larger_than_4320p,
        ]
        .into_iter()
    }
}

fn select_formats(
    mut formats: Vec<Format>,
    predicates: &[&dyn FormatPredicate],
) -> SelectedFormatOptions {
    let mut selected_formats = SelectedFormatOptions::default();

    // Filter out unwanted formats
    for predicate in predicates {
        formats.retain(|format| predicate.matches(format));
    }

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

trait FormatPredicate {
    fn matches(&self, format: &Format) -> bool;
}

struct IsIphoneCompatible;

impl FormatPredicate for IsIphoneCompatible {
    fn matches(&self, format: &Format) -> bool {
        format.ext == "mp4" && format.vcodec.starts_with("avc1")
    }
}

#[derive(Deserialize, Debug)]
struct FormatsRequest {
    url: String,
}

#[derive(Deserialize, Debug)]
struct DownloadRequest {
    url: String,
    format_id: String,
    #[serde(default)]
    keep_on_server: bool,
}

// Platform downloaders

trait PlatformDownloader {
    fn list_formats(&self, _url: &str) -> Result<Vec<Format>, Error> {
        Ok(vec![Format::best()])
    }

    fn download(&self, url: &str, _format_id: &str) -> Result<(), Error> {
        Command::new("yt-dlp")
            .args(["-f", "best", "-o", get_download_file_path().as_str(), url])
            .output()
            .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))?;

        Ok(())
    }
}

struct YouTubeDownloader;

impl YouTubeDownloader {
    const COOKIES_FILE_PATH_ENV_KEY: &'static str = "YOUTUBE_COOKIES_FILE";
}

struct FacebookDownloader;
struct InstagramDownloader;

impl InstagramDownloader {
    const COOKIES_FILE_PATH_ENV_KEY: &'static str = "INSTAGRAM_COOKIES_FILE";
}

struct XDownloader;
struct TikTokDownloader;

fn get_cookies_file_path(name: &str) -> Result<String, Error> {
    dotenvy::dotenv().ok();
    std::env::var(name).map_err(|_| Error::InvalidInput(format!("{name} must be set in .env")))
}

impl PlatformDownloader for YouTubeDownloader {
    fn list_formats(&self, url: &str) -> Result<Vec<Format>, Error> {
        let cookies_file_path = get_cookies_file_path(Self::COOKIES_FILE_PATH_ENV_KEY)?;
        let video_details = Command::new("yt-dlp")
            .arg("--dump-single-json")
            .arg("--skip-download")
            .arg("--cookies")
            .arg(cookies_file_path)
            .arg(url)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))?;

        let formats_bytes = Command::new("jq")
            .arg(r#"[.formats[] | select(.vcodec != "none" and .height != null) | {format_id, height, ext, vcodec, acodec, filesize}]"#)
            .stdin(video_details.stdout.unwrap())
            .output()
            .map_err(|error| Error::JqExecutionFailed(error.to_string()))?;

        let formats: Vec<Format> = serde_json::from_slice(&formats_bytes.stdout)
            .map_err(|error| Error::FormatDeserializationFailed(error.to_string()))?;

        // Filter out formats incompatible with iPhone
        let predicates: &[&dyn FormatPredicate] = &[&IsIphoneCompatible];
        Ok(select_formats(formats, predicates)
            .into_iter()
            .flatten()
            .collect())
    }

    fn download(&self, url: &str, format_id: &str) -> Result<(), Error> {
        let formats = FORMATS_CACHE
            .read()
            .map_err(|error| Error::InvalidInput(format!("Format cache unavailable: {error}")))?
            .get(url)
            .cloned()
            .map_or_else(|| self.list_formats(url), Ok)?;

        if !formats.iter().any(|format| format.format_id == format_id) {
            return Err(Error::InvalidInput(format!(
                "Format ID not found: {format_id}"
            )));
        }

        let cookies_file_path = get_cookies_file_path(Self::COOKIES_FILE_PATH_ENV_KEY)?;

        Command::new("yt-dlp")
            .args([
                "-f",
                &format!("{format_id}+ba"),
                "--merge-output-format",
                "mp4",
                "--cookies",
                &cookies_file_path,
                "-o",
                get_download_file_path().as_str(),
                url,
            ])
            .output()
            .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))
            .and_then(|output| {
                if output.status.success() {
                    Ok(output)
                } else {
                    Err(Error::YtDlpExecutionFailed(
                        String::from_utf8_lossy(&output.stderr).into_owned(),
                    ))
                }
            })?;

        Ok(())
    }
}

impl PlatformDownloader for FacebookDownloader {}
impl PlatformDownloader for InstagramDownloader {
    fn download(&self, url: &str, _format_id: &str) -> Result<(), Error> {
        let cookies_file_path = get_cookies_file_path(Self::COOKIES_FILE_PATH_ENV_KEY)?;

        Command::new("yt-dlp")
            .args([
                "-f",
                "best",
                "--cookies",
                &cookies_file_path,
                "-o",
                get_download_file_path().as_str(),
                url,
            ])
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
    let address = address.unwrap_or_else(|| DEFAULT_ADDRESS.to_owned());
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
        .route("/ui", get(ui_handler))
        .route("/formats", get(list_formats_handler))
        .route("/download", get(download_handler))
}

async fn ui_handler() -> Html<&'static str> {
    Html(include_str!("resources/index.html"))
}

async fn list_formats_handler(
    Query(request): Query<FormatsRequest>,
) -> Result<Json<Vec<FormatResponse>>, (StatusCode, String)> {
    println!("Received formats request for URL: {}", request.url);
    cached_list_formats(&request.url)
        .await
        .map(|formats| Json(formats.into_iter().map(FormatResponse::from).collect()))
        .map_err(api_error)
}

async fn download_handler(
    Query(request): Query<DownloadRequest>,
) -> Result<Response<Body>, (StatusCode, String)> {
    println!(
        "Received download request for URL: {} with format: {} (keep_on_server: {})",
        request.url, request.format_id, request.keep_on_server
    );
    if request.keep_on_server {
        download_video(&request.url, &request.format_id)
            .await
            .map(|()| {
                Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty())
                    .unwrap()
            })
            .map_err(api_error)
    } else {
        download_for_client(&request.url, &request.format_id)
            .await
            .map(|(body, filename)| {
                Response::builder()
                    .header("content-type", "video/mp4")
                    .header(
                        "content-disposition",
                        format!("attachment; filename=\"{filename}\""),
                    )
                    .body(body)
                    .unwrap()
            })
            .map_err(api_error)
    }
}

async fn cached_list_formats(url: &str) -> Result<Vec<Format>, Error> {
    if let Some(formats) = FORMATS_CACHE
        .read()
        .map_err(|error| Error::InvalidInput(format!("Format cache unavailable: {error}")))?
        .get(url)
        .cloned()
    {
        return Ok(formats);
    }

    let formats = list_formats(url).await?;
    FORMATS_CACHE
        .write()
        .map_err(|error| Error::InvalidInput(format!("Format cache unavailable: {error}")))?
        .insert(url.to_owned(), formats.clone());
    Ok(formats)
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

async fn download_for_client(url: &str, format_id: &str) -> Result<(Body, String), Error> {
    download_video(url, format_id).await?;

    let path = std::fs::read_dir(DOWNLOADS_DIRECTORY.as_str())
        .map_err(Error::IO)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .max_by_key(|entry| {
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .map(|entry| entry.path())
        .ok_or_else(|| {
            Error::YtDlpExecutionFailed("yt-dlp did not produce a downloaded file".to_owned())
        })?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::YtDlpExecutionFailed("Invalid output filename".to_owned()))?
        .to_owned();
    let bytes = tokio::fs::read(&path).await.map_err(Error::IO)?;
    tokio::fs::remove_file(&path).await.map_err(Error::IO)?;

    Ok((Body::from(bytes), filename))
}

#[cfg(test)]
static LIVE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
fn get_env_variable(name: &str) -> String {
    dotenvy::dotenv().ok();
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set in .env"))
}

#[cfg(test)]
mod api_tests {
    use super::*;
    use std::path::Path;

    fn cleanup_download() {
        let path = Path::new(DOWNLOADS_DIRECTORY.as_str()).join("foO1QakhjZE.mp4");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn formats_endpoint_returns_default_format_for_supported_fallback_platform() {
        let response = list_formats_handler(Query(FormatsRequest {
            url: get_env_variable("FACEBOOK_TEST_URL"),
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

    #[tokio::test]
    async fn youtube_api_lists_formats_downloads_lowest_format_and_creates_file() {
        let _guard = LIVE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        cleanup_download();

        let youtube_url = get_env_variable("YOUTUBE_TEST_URL");
        let formats = list_formats_handler(Query(FormatsRequest {
            url: youtube_url.clone(),
        }))
        .await
        .unwrap()
        .0;
        let lowest_format = formats
            .first()
            .expect("YouTube formats should not be empty");

        let response = download_handler(Query(DownloadRequest {
            url: youtube_url,
            format_id: lowest_format.format_id.clone(),
            keep_on_server: true,
        }))
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(
            Path::new(DOWNLOADS_DIRECTORY.as_str())
                .join("foO1QakhjZE.mp4")
                .exists()
        );
        cleanup_download();
    }

    #[tokio::test]
    async fn youtube_api_downloads_to_client_and_removes_server_file_by_default() {
        let _guard = LIVE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        cleanup_download();

        let youtube_url = get_env_variable("YOUTUBE_TEST_URL");
        let formats = list_formats(&youtube_url).await.unwrap();
        let lowest_format = formats
            .first()
            .expect("YouTube formats should not be empty");

        let response = download_handler(Query(DownloadRequest {
            url: youtube_url,
            format_id: lowest_format.format_id.clone(),
            keep_on_server: false,
        }))
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("content-type").unwrap(), "video/mp4");
        assert!(response.headers().contains_key("content-disposition"));
        assert!(
            !Path::new(DOWNLOADS_DIRECTORY.as_str())
                .join("foO1QakhjZE.mp4")
                .exists()
        );
    }

    #[test]
    fn router_exposes_api_routes() {
        let _router = api_router();
    }

    #[tokio::test]
    async fn ui_page_loads_formats_and_starts_downloads() {
        let page = ui_handler().await.0;

        assert!(page.contains("/formats?url="));
        assert!(page.contains("format.format_id"));
        assert!(page.contains("/download?url="));
        assert!(page.contains("format_id="));
        assert!(page.contains("Download complete"));
        assert!(!page.contains("form.reset()"));
        assert!(page.contains("formats.replaceChildren()"));
        assert!(page.contains("@media (prefers-color-scheme: dark)"));
        assert!(page.contains("download.blob()"));
        assert!(page.contains("Content-Disposition"));
    }

    #[test]
    fn download_request_defaults_to_not_keep_on_server() {
        let request: DownloadRequest = serde_json::from_value(serde_json::json!({
            "url": "https://example.com/video",
            "format_id": "best"
        }))
        .unwrap();

        assert!(!request.keep_on_server);
    }
}

#[cfg(test)]
mod cli_tests {
    use super::*;
    use std::path::Path;

    fn cleanup_download() {
        let path = Path::new(DOWNLOADS_DIRECTORY.as_str()).join("foO1QakhjZE.mp4");
        let _ = std::fs::remove_file(path);
    }

    fn format(format_id: &str) -> Format {
        Format {
            format_id: format_id.to_owned(),
            height: 360,
            ext: "mp4".to_owned(),
            vcodec: "h264".to_owned(),
            acodec: "aac".to_owned(),
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

    #[tokio::test]
    async fn youtube_cli_lists_formats_downloads_lowest_format_and_creates_file() {
        let _guard = LIVE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap();
        cleanup_download();

        let youtube_url = get_env_variable("YOUTUBE_TEST_URL");
        let formats = list_formats(&youtube_url).await.unwrap();
        let lowest_format = formats
            .first()
            .expect("YouTube formats should not be empty");

        download_video(&youtube_url, &lowest_format.format_id)
            .await
            .unwrap();

        assert!(
            Path::new(DOWNLOADS_DIRECTORY.as_str())
                .join("foO1QakhjZE.mp4")
                .exists()
        );
        cleanup_download();
    }
}

#[cfg(test)]
mod downloader_unit_tests {
    use super::*;

    #[test]
    fn default_downloader_returns_only_best_format_without_metadata() {
        let formats = FacebookDownloader
            .list_formats(&get_env_variable("FACEBOOK_TEST_URL"))
            .unwrap();

        assert_eq!(formats.len(), 1);
        assert_eq!(
            serde_json::to_value(FormatResponse::from(formats[0].clone())).unwrap(),
            serde_json::json!({ "format_id": "best", "height": 0, "filesize": null })
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
                    .list_formats(&get_env_variable("EXAMPLE_TEST_URL"))
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
                ext: "mp4".to_owned(),
                vcodec: "avc1".to_owned(),
                acodec: "aac".to_owned(),
                filesize: None,
            },
            Format {
                format_id: "360".to_owned(),
                height: 360,
                ext: "mp4".to_owned(),
                vcodec: "avc1".to_owned(),
                acodec: "aac".to_owned(),
                filesize: None,
            },
            Format {
                format_id: "1080".to_owned(),
                height: 1080,
                ext: "mp4".to_owned(),
                vcodec: "avc1".to_owned(),
                acodec: "aac".to_owned(),
                filesize: None,
            },
        ];

        let predicates = &[];
        let selected: Vec<Format> = select_formats(formats, predicates)
            .into_iter()
            .flatten()
            .collect();

        assert_eq!(
            selected
                .iter()
                .map(|format| format.format_id.as_str())
                .collect::<Vec<_>>(),
            ["144", "360", "1080"]
        );
    }
}
