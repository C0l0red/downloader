use axum::{
    Router,
    body::Body,
    extract::{Json, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::array::IntoIter;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use std::{
    collections::HashMap,
    io::{self, Write},
    process::Command,
    sync::{Arc, RwLock},
};
use std::fmt::Display;
use url::Url;

const MAX_FORMAT_OPTIONS: usize = 10;
const DEFAULT_ADDRESS: &str = "0.0.0.0:3424";
type MediaItems = Vec<MediaInfo>;
type MediaCache = HashMap<String, MediaItems>;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let result = run().await;
    if let Err(error) = &result {
        error.log();
    }
    result
}

async fn run() -> Result<(), Error> {
    let state = AppState::from_environment()?;
    if std::env::args().nth(1).as_deref() == Some("server") {
        let address = std::env::args().nth(2);
        return run_server(state, address).await;
    }

    run_cli(&state).await
}

// Error and data types

#[derive(Debug)]
enum Error {
    YtDlpExecutionFailed(String),
    FormatDeserializationFailed(String),
    InvalidInput(String),
    IO(io::Error),
    UnsupportedPlatform(String),
    // TODO: Change this to take a Path and auto print the path in the error message, but be careful not to return the path to the client
    NotAValidPath(String),
    MissingArgument(String),
    Internal(String),
}

impl Error {
    fn log(&self) {
        eprintln!("Application error: {self:?}");
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response<Body> {
        self.log();
        let (status, message) = match self {
            Self::InvalidInput(message) | Self::UnsupportedPlatform(message) => {
                (StatusCode::BAD_REQUEST, message)
            }
            Self::YtDlpExecutionFailed(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "An error occurred while downloading or getting metadata for the media".to_owned(),
            ),
            Self::FormatDeserializationFailed(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "An error occurred while processing the media formats".to_owned(),
            ),
            Self::IO(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "An error occurred while accessing the filesystem".to_owned(),
            ),
            Self::NotAValidPath(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "An error occurred while loading the application configuration".to_owned(),
            ),
            Self::MissingArgument(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "An error occurred while loading the application configuration".to_owned(),
            ),
            Self::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
        };

        (status, message).into_response()
    }
}

struct TempDownloadedFile(PathBuf);

impl TempDownloadedFile {
    fn try_new(path: PathBuf) -> Result<Self, Error> {
        let metadata = std::fs::metadata(&path.canonicalize().unwrap()).map_err(|error| Error::IO(error))?;
        if !metadata.is_file() {
            return Err(Error::NotAValidPath(path.to_string_lossy().to_string()));
        }
        Ok(Self(path))
    }

    fn file_name(&self) -> Result<String, Error> {
        Ok(self
            .0
            .file_name()
            .ok_or(Error::Internal(
                "Downloaded file has no file name".to_string(),
            ))?
            .to_str()
            .ok_or(Error::Internal(
                "Downloaded file name is not valid".to_string(),
            ))?
            .to_owned())
    }
    fn get_size(&self) -> Result<usize, Error> {
        Ok(std::fs::metadata(&self.0)
            .map_err(|error| Error::IO(error))?
            .len() as usize)
    }

    fn content_type(&self) -> Result<ContentType, Error> {
        let extension = self
            .0
            .extension()
            .ok_or(Error::Internal(
                "Downloaded file has no extension".to_string(),
            ))?
            .to_str()
            .ok_or(Error::Internal(
                "Downloaded file's extension is invalid".to_string(),
            ))?
            .to_owned();

        Ok(ContentType::from_extension(&extension))
    }

    fn read_and_remove(self) -> Result<Vec<u8>, Error> {
        let bytes = std::fs::read(&self.0).map_err(|error| Error::IO(error))?;
        std::fs::remove_file(&self.0).map_err(|error| Error::IO(error))?;
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    // Video
    Mp4,
    WebM,
    QuickTime,
    M4v,
    Matroska,

    // Images
    Jpeg,
    Png,
    WebP,
    Gif,
    Avif,
    Heic,

    // Fallback
    OctetStream,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "video/mp4",
            Self::WebM => "video/webm",
            Self::QuickTime => "video/quicktime",
            Self::M4v => "video/x-m4v",
            Self::Matroska => "video/x-matroska",

            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
            Self::Avif => "image/avif",
            Self::Heic => "image/heic",

            Self::OctetStream => "application/octet-stream",
        }
    }

    pub fn from_extension(extension: &str) -> Self {
        match extension.to_ascii_lowercase().as_str() {
            "mp4" => Self::Mp4,
            "webm" => Self::WebM,
            "mov" => Self::QuickTime,
            "m4v" => Self::M4v,
            "mkv" => Self::Matroska,

            "jpg" | "jpeg" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::WebP,
            "gif" => Self::Gif,
            "avif" => Self::Avif,
            "heic" => Self::Heic,

            _ => Self::OctetStream,
        }
    }
}

impl Display for TempDownloadedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct YtDlpMediaMetadata {
    title: String,
    thumbnail: Option<String>,
    formats: Vec<YtDlpFormat>,
    entries: Vec<YtDlpMediaEntryMetadata>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct YtDlpMediaEntryMetadata {
    title: String,
    thumbnail: String,
    formats: Vec<YtDlpFormat>,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
struct YtDlpFormat {
    format_id: String,
    height: Option<u16>,
    ext: String,
    vcodec: Option<String>,
    acodec: Option<String>,
    filesize: Option<u64>,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
struct MediaInfo {
    title: String,
    thumbnail: String,
    formats: Vec<MediaFormat>,
}

#[derive(Clone, Deserialize, Serialize, Debug)]
struct MediaFormat {
    format_id: String,
    height: Option<u16>,
    filesize: Option<u64>,
}

impl YtDlpMediaMetadata {
    fn is_playlist(&self) -> bool {
        self.entries.is_empty()
    }

    fn entry_count(&self) -> usize {
        self.entries.len()
    }

    fn get_format_by_id(
        &self,
        format_id: &str,
        playlist_index: Option<u8>,
    ) -> Option<&YtDlpFormat> {
        if let Some(index) = playlist_index {
            if let Some(entry) = self.entries.get(index as usize) {
                return entry.formats.iter().find(|f| f.format_id == format_id);
            }
        }
        self.formats.iter().find(|f| f.format_id == format_id)
    }
}

// Into because it doesn't seem very readable as a From implementation
impl TryInto<MediaItems> for YtDlpMediaMetadata {
    type Error = Error;

    fn try_into(self) -> Result<MediaItems, Error> {
        if self.is_playlist() {
            let mut media_items = Vec::with_capacity(self.entry_count());

            media_items.extend(self.entries.into_iter().map(|entry| {
                let formats: Vec<MediaFormat> = entry
                    .formats
                    .into_iter()
                    .map(|format| MediaFormat {
                        format_id: format.format_id,
                        height: format.height,
                        filesize: format.filesize,
                    })
                    .collect();

                MediaInfo {
                    title: entry.title,
                    thumbnail: entry.thumbnail,
                    formats,
                }
            }));

            Ok(media_items)
        } else {
            let mut media_items: MediaItems = Vec::with_capacity(1);

            let formats: Vec<MediaFormat> = self
                .formats
                .into_iter()
                .map(|format| MediaFormat {
                    format_id: format.format_id,
                    height: format.height,
                    filesize: format.filesize,
                })
                .collect();

            media_items.push(MediaInfo {
                title: self.title,
                thumbnail: self.thumbnail.ok_or(Error::Internal(
                    "Media metadata has no thumbnail".to_string(),
                ))?,
                formats,
            });

            Ok(media_items)
        }
    }
}

#[derive(Clone, Debug)]
struct Config {
    instagram_cookies_path: PathBuf,
    youtube_cookies_path: PathBuf,
    downloads_directory: PathBuf,
}

struct AppState {
    config: Config,
    media_cache: RwLock<MediaCache>,
}

impl AppState {
    fn from_environment() -> Result<Self, Error> {
        dotenvy::dotenv().ok();

        let downloads_directory = environment_path("DOWNLOADS_DIRECTORY").or_else(|error| {
            match error {
                // Create the downloads directory only if it was not set in the environment variables
                Error::MissingArgument(_) => {
                    let current_directory = std::env::current_dir().map_err(Error::IO)?;
                    let path = PathBuf::from(current_directory.join("downloads"));

                    println!("DOWNLOADS_DIRECTORY environment variable not set, using {} as the downloads directory", path.display());
                    std::fs::create_dir_all(&path).map_err(Error::IO)?;
                    Ok(path)
                }
                // Otherwise, return the error
                _ => return Err(error),
            }
        })?;

        Ok(Self {
            config: Config {
                instagram_cookies_path: environment_path("INSTAGRAM_COOKIES_FILE")?,
                youtube_cookies_path: environment_path("YOUTUBE_COOKIES_FILE")?,
                downloads_directory,
            },
            media_cache: RwLock::new(HashMap::new()),
        })
    }
}

/// Returns the path to the environment variable, or an error if it is not set or is not a valid path.
fn environment_path(name: &str) -> Result<PathBuf, Error> {
    std::env::var(name)
        .map_err(|_| Error::MissingArgument(format!("{name} must be set in .env")))
        .map(PathBuf::from)
        .and_then(|path| {
            if path.exists() {
                // Ensure the path set in the environment variables exists
                Ok(path)
            } else {
                Err(Error::NotAValidPath(format!(
                    "{name} from the environment is not a valid path"
                )))
            }
        })
}

/// Gets the download file name template. yt-dlp will substitute id and ext with the corresponding values for each file.
fn get_download_file_path(config: &Config) -> PathBuf {
    config.downloads_directory.join("%(id)s.%(ext)s")
}

impl YtDlpFormat {
    fn best() -> Self {
        Self {
            format_id: "best".to_owned(),
            height: None,
            ext: String::new(),
            vcodec: None,
            acodec: None,
            filesize: None,
        }
    }
}

#[derive(Default, Debug)]
struct DeduplicatedFormatOptions {
    closest_to_144p: Option<YtDlpFormat>,
    closest_to_240p: Option<YtDlpFormat>,
    closest_to_360p: Option<YtDlpFormat>,
    closest_to_480p: Option<YtDlpFormat>,
    closest_to_720p: Option<YtDlpFormat>,
    closest_to_1080p: Option<YtDlpFormat>,
    closest_to_1440p: Option<YtDlpFormat>,
    closest_to_2160p: Option<YtDlpFormat>,
    closest_to_4320p: Option<YtDlpFormat>,
    larger_than_4320p: Option<YtDlpFormat>,
}

impl IntoIterator for DeduplicatedFormatOptions {
    type Item = Option<YtDlpFormat>;
    type IntoIter = IntoIter<Option<YtDlpFormat>, MAX_FORMAT_OPTIONS>;

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

fn filter_media_metadata_formats(
    media_metadata: &mut YtDlpMediaMetadata,
    predicates: &[&dyn FormatPredicate],
) -> Result<(), Error> {
    // If the media metadata is a playlist, filter the formats of each entry
    if media_metadata.is_playlist() {
        // Retain formats that match all predicates and then group remaining formats using select_formats
        for entry in media_metadata.entries.iter_mut() {
            entry
                .formats
                .retain(|format| predicates.iter().all(|predicate| predicate.matches(format)));
            let formats = std::mem::take(&mut entry.formats);
            entry.formats = deduplicate_by_resolution(formats)
                .into_iter()
                .flatten()
                .collect();
        }
        Ok(())
    } else {
        // Retain formats that match all predicates and then group remaining formats using select_formats
        media_metadata
            .formats
            .retain(|format| predicates.iter().all(|predicate| predicate.matches(format)));
        let formats = std::mem::take(&mut media_metadata.formats);
        media_metadata.formats = deduplicate_by_resolution(formats)
            .into_iter()
            .flatten()
            .collect();
        Ok(())
    }
}

fn deduplicate_by_resolution(mut formats: Vec<YtDlpFormat>) -> DeduplicatedFormatOptions {
    let mut deduplicated_formats = DeduplicatedFormatOptions::default();

    // Sort formats by height and then by filesize presence
    formats.sort_by(|a, b| {
        a.height
            .cmp(&b.height)
            .then_with(|| a.filesize.is_some().cmp(&b.filesize.is_some()))
    });

    for format in formats {
        match format.height {
            Some(res) if res <= 144 => deduplicated_formats.closest_to_144p = Some(format),
            Some(res) if res <= 144 + 240 / 2 => {
                deduplicated_formats.closest_to_240p = Some(format)
            }
            Some(res) if res <= 240 + 360 / 2 => {
                deduplicated_formats.closest_to_360p = Some(format)
            }
            Some(res) if res <= 360 + 480 / 2 => {
                deduplicated_formats.closest_to_480p = Some(format)
            }
            Some(res) if res <= 480 + 720 / 2 => {
                deduplicated_formats.closest_to_720p = Some(format)
            }
            Some(res) if res <= 720 + 1080 / 2 => {
                deduplicated_formats.closest_to_1080p = Some(format)
            }
            Some(res) if res <= 1080 + 1440 / 2 => {
                deduplicated_formats.closest_to_1440p = Some(format)
            }
            Some(res) if res <= 1440 + 2160 / 2 => {
                deduplicated_formats.closest_to_2160p = Some(format)
            }
            Some(res) if res <= 2160 + 4320 / 2 => {
                deduplicated_formats.closest_to_4320p = Some(format)
            }
            Some(_) => deduplicated_formats.larger_than_4320p = Some(format),
            None => {}
        }
    }

    deduplicated_formats
}

trait FormatPredicate {
    fn matches(&self, format: &YtDlpFormat) -> bool;
}

/// Predicate for finding formats compatible with iPhone by checking for MP4 extension and AVC1 video codec
struct IsIphoneCompatible;

impl FormatPredicate for IsIphoneCompatible {
    fn matches(&self, format: &YtDlpFormat) -> bool {
        format.ext == "mp4"
            && format
                .vcodec
                .as_ref()
                .is_some_and(|vcodec| vcodec.starts_with("avc1"))
    }
}

/// Predicate for finding video formats by ensuring height and vcodec metadata exist
struct IsVideoFormat;

impl FormatPredicate for IsVideoFormat {
    fn matches(&self, format: &YtDlpFormat) -> bool {
        format.height.is_some()
            && format
                .vcodec
                .as_ref()
                .is_some_and(|vcodec| vcodec != "none")
    }
}

#[derive(Deserialize, Debug)]
struct GetMetadataRequest {
    url: String,
}

#[derive(Deserialize, Debug)]
struct DownloadParams {
    url: String,
    download_option_index: usize,
    #[serde(default)]
    keep_on_server: bool,
    item_index: usize,
}

// yt-dlp wrapper

struct YtDlp {
    url: String,
    format_id: Option<String>,
    cookies_path: Option<PathBuf>,
    output_path: PathBuf,
    merge_mp4: bool,
    default_to_best_audio: bool,
}

#[derive(Default)]
struct YtDlpBuilder {
    url: Option<String>,
    format_id: Option<String>,
    cookies_path: Option<PathBuf>,
    output_path: Option<PathBuf>,
    merge_mp4: bool,
    default_to_best_audio: bool,
}

impl YtDlpBuilder {
    fn new() -> Self {
        Self::default()
    }

    fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    fn format_id(mut self, format_id: impl Into<String>) -> Self {
        self.format_id = Some(format_id.into());
        self
    }

    fn cookies_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.cookies_path = Some(path.into());
        self
    }

    fn output_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.output_path = Some(path.into());
        self
    }

    fn merge_mp4(mut self) -> Self {
        self.merge_mp4 = true;
        self
    }

    fn default_to_best_audio(mut self) -> Self {
        self.default_to_best_audio = true;
        self
    }

    fn build(self) -> Result<YtDlp, Error> {
        Ok(YtDlp {
            url: self.url.ok_or_else(|| {
                Error::Internal("Building the yt-dlp request failed: URL is missing".to_owned())
            })?,
            format_id: self.format_id,
            cookies_path: self.cookies_path,
            output_path: self.output_path.ok_or_else(|| {
                Error::Internal(
                    "Building the yt-dlp request failed: output path is missing".to_owned(),
                )
            })?,
            merge_mp4: self.merge_mp4,
            default_to_best_audio: self.default_to_best_audio,
        })
    }
}

impl YtDlp {
    fn get_metadata(&self) -> Result<serde_json::Value, Error> {
        let mut command = Command::new("yt-dlp");
        command.args(["--dump-single-json", "--skip-download"]);
        self.add_cookies(&mut command);
        command.arg(&self.url);

        let output = Self::execute(&mut command)?;
        serde_json::from_slice(&output.stdout)
            .map_err(|error| Error::FormatDeserializationFailed(error.to_string()))
    }

    fn download_media(&self) -> Result<TempDownloadedFile, Error> {
        let format = self
            .format_id
            .as_deref()
            .map(|format_id| {
                format_id.to_owned()
                    + if self.default_to_best_audio {
                        "+ba"
                    } else {
                        ""
                    }
            })
            .unwrap_or_else(|| "b".to_owned());

        let mut command = Command::new("yt-dlp");
        command.args(["-f", &format]);
        if self.merge_mp4 {
            command.args(["--merge-output-format", "mp4"]);
        }
        command.args(["--print", "after_move:filename"]);
        self.add_cookies(&mut command);
        command.args(["-o", self.output_path.to_string_lossy().as_ref(), &self.url]);
        let output = Self::execute(&mut command)?;

        if output.status.success() {
            let file_path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Ok(TempDownloadedFile::try_new(PathBuf::from(file_path))?)
        } else {
            let err = String::from_utf8_lossy(&output.stderr).to_string();
            Err(Error::YtDlpExecutionFailed(err))
        }
    }

    fn add_cookies(&self, command: &mut Command) {
        if let Some(path) = &self.cookies_path {
            command.args(["--cookies", path.to_string_lossy().as_ref()]);
        }
    }

    fn execute(command: &mut Command) -> Result<std::process::Output, Error> {
        command
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
            })
    }
}

// TODO: Run this to confirm that a file is actually downloaded and file name is returned
#[test]
fn test_download_function_returns_filename() {
    let download_path = PathBuf::from("downloads").join("%(title)s.%(ext)s");

    let yt_dlp = YtDlpBuilder::new()
        .output_path(download_path)
        .cookies_path("cookies/instagram-cookies.txt")
        .url("https://www.instagram.com/reel/Ddorb7ryu5t/?stkn=aThteW96MmI2Z3Vx")
        .build()
        .unwrap();

    let file_name = yt_dlp.download_media().unwrap();
    println!("{}", file_name);
}

// Content providers

trait ContentProvider {
    fn get_media_items(&self, url: &str) -> Result<MediaItems, Error> {
        let metadata_json = YtDlpBuilder::new().url(url).build()?.get_metadata()?;
        let media_metadata: YtDlpMediaMetadata =
            serde_json::from_value(metadata_json).map_err(|error| {
                Error::FormatDeserializationFailed(format!(
                    "Failed to deserialize metadata: {}",
                    error
                ))
            })?;

        Ok(media_metadata.try_into()?)
    }

    fn download(&self, url: String, format_id: String, config: &Config) -> Result<TempDownloadedFile, Error> {
        YtDlpBuilder::new()
            .url(&url)
            .output_path(get_download_file_path(config))
            .format_id(format_id)
            .build()?
            .download_media()
    }

    fn get_format_for_download(
        &self,
        media_cache: &RwLock<MediaCache>,
        download_params: &DownloadParams,
    ) -> Result<MediaFormat, Error> {
        // TODO: try to achieve this without cloning
        let media_items = media_cache
            .read()
            .map_err(|_| Error::Internal("Accessing the format cache failed".to_owned()))?
            .get(&download_params.url)
            .cloned()
            .map_or_else(|| self.get_media_items(&download_params.url), Ok)?;

        let media_info =
            media_items
                .get(download_params.item_index as usize)
                .ok_or(Error::InvalidInput(format!(
                    "Item index {} not found",
                    download_params.item_index
                )))?;
        let format = media_info
            .formats
            .get(download_params.download_option_index as usize)
            .ok_or(Error::InvalidInput(format!(
                "Download option index {} not found",
                download_params.download_option_index
            )))?;

        Ok(format.clone())
    }
}

struct YouTubeProvider {
    config: Config,
}
struct FacebookProvider;
struct InstagramProvider;
struct XProvider;
struct TikTokProvider;

impl ContentProvider for YouTubeProvider {
    fn get_media_items(&self, _url: &str) -> Result<MediaItems, Error> {
        let metadata_json = YtDlpBuilder::new()
            .url(_url)
            .cookies_path(&self.config.youtube_cookies_path)
            .build()?
            .get_metadata()?;
        let mut media_metadata: YtDlpMediaMetadata = serde_json::from_value(metadata_json)
            .map_err(|error| {
                Error::FormatDeserializationFailed(format!(
                    "Failed to deserialize metadata: {}",
                    error
                ))
            })?;

        // Filter out non-video formats and formats incompatible with iPhone
        let predicates: &[&dyn FormatPredicate] = &[&IsVideoFormat, &IsIphoneCompatible];
        filter_media_metadata_formats(&mut media_metadata, predicates)?;

        Ok(media_metadata.try_into()?)
    }

    fn download(&self, url: String, format_id: String, config: &Config) -> Result<TempDownloadedFile, Error> {
        YtDlpBuilder::new()
            .url(url)
            .format_id(format_id)
            .default_to_best_audio()
            .cookies_path(&config.youtube_cookies_path)
            .output_path(get_download_file_path(&config))
            .merge_mp4()
            .build()?
            .download_media()
    }
}

impl ContentProvider for FacebookProvider {}
impl ContentProvider for InstagramProvider {
    fn download(&self, url: String, format_id: String, config: &Config) -> Result<TempDownloadedFile, Error> {
        YtDlpBuilder::new()
            .url(url)
            .format_id(format_id)
            .cookies_path(&config.instagram_cookies_path)
            .output_path(get_download_file_path(&config))
            .build()?
            .download_media()
    }
}
impl ContentProvider for XProvider {}
impl ContentProvider for TikTokProvider {}

// Interactive CLI

async fn run_cli(state: &AppState) -> Result<(), Error> {
    let url = prompt("URL: ")?;

    let media_items = get_media_items(&url, &state.config).await?;

    let (item_index, media_info) = match media_items.len() {
        0 => return Err(Error::Internal("URL has no media items".to_owned())),
        1 => {
            let media_info = media_items.get(0).ok_or(Error::Internal(
                "Failed to get first media item for URL".to_string(),
            ))?;
            (0, media_info)
        }
        _ => {
            for (index, media_info) in media_items.iter().enumerate() {
                println!("{} -> Thumbnail ({})", index + 1, media_info.thumbnail);
            }
            let choice = prompt("Select an item index from the list of items in the URL: ")?;
            let index = parse_choice_to_usize(&choice)?;
            let media_info = select_index(&media_items, index)?;

            (index, media_info)
        }
    };

    for (index, format) in media_info.formats.iter().enumerate() {
        println!(
            "{}. {}p - {}",
            index + 1,
            if let Some(height) = format.height {
                if height == 0 {
                    "best".to_owned()
                } else {
                    height.to_string()
                }
            } else {
                "best".to_string()
            },
            format
                .filesize
                .map(|size| format!("{} MB", size / 1_000_000))
                .unwrap_or_else(|| "unknown size".into())
        );
    }

    let choice = prompt("Select format: ")?;
    let download_option_index = parse_choice_to_usize(&choice)?;

    let download_params = DownloadParams {
        url: url.clone(),
        download_option_index,
        item_index,
        keep_on_server: false,
    };
    download_video(download_params, state).await?;

    println!("Download complete!");

    Ok(())
}

fn select_index<T>(slice: &[T], index: usize) -> Result<&T, Error> {
    slice
        .get(
            index
                .checked_sub(1)
                .ok_or(Error::InvalidInput(format!("Invalid index {}", index)))?,
        )
        .ok_or(Error::InvalidInput(format!(
            "Invalid input: {index} is not a valid format index"
        )))
}

fn parse_choice_to_usize(choice: &str) -> Result<usize, Error> {
    choice
        .parse()
        .map_err(|_| Error::InvalidInput(format!("Invalid input: {choice} is not a number")))
}

// Axum API

async fn run_server(state: AppState, address: Option<String>) -> Result<(), Error> {
    let address = address.unwrap_or_else(|| DEFAULT_ADDRESS.to_owned());
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .map_err(Error::IO)?;
    let app = api_router(Arc::new(state));

    println!("Axum server listening on {address}");
    axum::serve(listener, app)
        .await
        .map_err(|error| Error::IO(io::Error::other(error)))
}

fn api_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/ui", get(ui_handler))
        .route("/formats", get(get_metadata_handler))
        .route("/download", get(download_handler))
        .with_state(state)
}

async fn ui_handler() -> Html<&'static str> {
    Html(include_str!("resources/index.html"))
}

async fn get_metadata_handler(
    State(state): State<Arc<AppState>>,
    Query(request): Query<GetMetadataRequest>,
) -> Result<Json<MediaItems>, Error> {
    println!("Received media info request for URL: {}", request.url);
    get_cached_media_items(&request.url, &state)
        .await
        .map(|media_items| Json(media_items))
}

async fn download_handler(
    State(state): State<Arc<AppState>>,
    Query(download_params): Query<DownloadParams>,
) -> Result<Response<Body>, Error> {
    println!(
        "Received download request for URL: {} for item index: {} and download option index: {} (keep_on_server: {})",
        download_params.url,
        download_params.item_index,
        download_params.download_option_index,
        download_params.keep_on_server
    );
    let keep_on_server = download_params.keep_on_server;
    let temp_downloaded_file = download_video(download_params, &state).await?;

    if keep_on_server {
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Body::empty())
            .map_err(|_| Error::Internal("Building the response failed".to_owned()))
    } else {
        // Use inline to allow the browser to display the video or image inline when the format is known
        // Use attachment to force a download when the format is unknown (octet stream)
        let content_disposition = if temp_downloaded_file.content_type()? == ContentType::OctetStream {
            format!("attachment; filename=\"{}\"", temp_downloaded_file.file_name()?)
        } else {
            format!("inline; filename=\"{}\"", temp_downloaded_file.file_name()?)
        };

        Response::builder()
            .header("content-type", temp_downloaded_file.content_type()?.as_str())
            .header(
                "content-disposition",
                content_disposition,
            )
            .body(temp_downloaded_file.read_and_remove()?.into())
            .map_err(|_| Error::Internal("Building the response failed".to_owned()))

    }
}

async fn get_cached_media_items(url: &str, state: &AppState) -> Result<MediaItems, Error> {
    if let Some(media_items) = state
        .media_cache
        .read()
        .map_err(|_| Error::Internal("Accessing the media cache failed".to_owned()))?
        .get(url)
        .cloned()
    {
        return Ok(media_items);
    }

    let formats = get_media_items(url, &state.config).await?;
    state
        .media_cache
        .write()
        .map_err(|_| Error::Internal("Accessing the media cache failed".to_owned()))?
        .insert(url.to_owned(), formats.clone());
    Ok(formats)
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

fn downloader_for_url(url: &str, config: &Config) -> Result<Box<dyn ContentProvider>, Error> {
    match get_platform_from_url(url)? {
        Platform::YouTube => Ok(Box::new(YouTubeProvider {
            config: config.clone(),
        })),
        Platform::Facebook => Ok(Box::new(FacebookProvider)),
        Platform::Instagram => Ok(Box::new(InstagramProvider)),
        Platform::X => Ok(Box::new(XProvider)),
        Platform::TikTok => Ok(Box::new(TikTokProvider)),
        Platform::Unsupported(host) => Err(Error::UnsupportedPlatform(host)),
    }
}

async fn get_media_items(url: &str, config: &Config) -> Result<MediaItems, Error> {
    downloader_for_url(url, config)?.get_media_items(url)
}

async fn download_video(
    download_params: DownloadParams,
    state: &AppState,
) -> Result<TempDownloadedFile, Error> {
    let provider = downloader_for_url(&download_params.url, &state.config)?;
    let format = provider.get_format_for_download(&state.media_cache, &download_params)?;

    provider.download(download_params.url, format.format_id, &state.config)
}

#[cfg(test)]
static LIVE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[cfg(test)]
fn get_env_variable(name: &str) -> String {
    dotenvy::dotenv().ok();
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set in .env"))
}

#[cfg(test)]
fn test_state() -> AppState {
    AppState::from_environment().expect("test configuration should be available")
}

// TODO: Junie: Uncomment these tests and fix them based on the code changes I've made
// #[cfg(test)]
// mod api_tests {
//     use super::*;
//     use std::path::Path;
//
//     fn cleanup_download() {
//         let path = test_state()
//             .config
//             .downloads_directory
//             .join("foO1QakhjZE.mp4");
//         let _ = std::fs::remove_file(path);
//     }
//
//     #[tokio::test]
//     async fn formats_endpoint_returns_default_format_for_supported_fallback_platform() {
//         let state = Arc::new(test_state());
//         let response = get_metadata_handler(
//             State(state),
//             Query(GetMetadataRequest {
//                 url: get_env_variable("FACEBOOK_TEST_URL"),
//             }),
//         )
//         .await
//         .unwrap()
//         .0;
//
//         assert_eq!(response.len(), 1);
//         assert_eq!(response[0].format_id, "best");
//     }
//
//     #[tokio::test]
//     async fn formats_endpoint_rejects_invalid_url() {
//         let state = Arc::new(test_state());
//         let error = get_metadata_handler(
//             State(state),
//             Query(GetMetadataRequest {
//                 url: "not-a-url".to_owned(),
//             }),
//         )
//         .await
//         .unwrap_err();
//
//         assert_eq!(error.into_response().status(), StatusCode::BAD_REQUEST);
//     }
//
//     #[tokio::test]
//     async fn internal_errors_return_their_descriptive_message() {
//         let message = "Building the response failed";
//         let response = Error::Internal(message.to_owned()).into_response();
//
//         assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
//         let body = axum::body::to_bytes(response.into_body(), usize::MAX)
//             .await
//             .unwrap();
//         assert_eq!(body.as_ref(), message.as_bytes());
//     }
//
//     #[test]
//     fn client_input_errors_return_their_message() {
//         let response = Error::InvalidInput("Invalid URL".to_owned()).into_response();
//
//         assert_eq!(response.status(), StatusCode::BAD_REQUEST);
//     }
//
//     // TODO: JUNIE: Fix this test
//     // #[tokio::test]
//     // async fn youtube_api_lists_formats_downloads_lowest_format_and_creates_file() {
//     //     let _guard = LIVE_TEST_LOCK
//     //         .get_or_init(|| Mutex::new(()))
//     //         .lock()
//     //         .unwrap();
//     //     cleanup_download();
//     //
//     //     let state = Arc::new(test_state());
//     //     let youtube_url = get_env_variable("YOUTUBE_TEST_URL");
//     //     let formats = list_formats_handler(
//     //         State(Arc::clone(&state)),
//     //         Query(GetMetadataRequest {
//     //             url: youtube_url.clone(),
//     //         }),
//     //     )
//     //     .await
//     //     .unwrap()
//     //     .0;
//     //     let lowest_format = formats
//     //         .first()
//     //         .expect("YouTube formats should not be empty");
//     //
//     //     let response = download_handler(
//     //         State(state),
//     //         Query(DownloadParams {
//     //             url: youtube_url,
//     //             download_option_id: lowest_format.format_id.clone(),
//     //             keep_on_server: true,
//     //         }),
//     //     )
//     //     .await
//     //     .unwrap();
//     //
//     //     assert_eq!(response.status(), StatusCode::NO_CONTENT);
//     //     assert!(
//     //         Path::new(&test_state().config.downloads_directory)
//     //             .join("foO1QakhjZE.mp4")
//     //             .exists()
//     //     );
//     //     cleanup_download();
//     // }
//
//     #[tokio::test]
//     async fn youtube_api_downloads_to_client_and_removes_server_file_by_default() {
//         let _guard = LIVE_TEST_LOCK
//             .get_or_init(|| Mutex::new(()))
//             .lock()
//             .unwrap();
//         cleanup_download();
//
//         let state = Arc::new(test_state());
//         let youtube_url = get_env_variable("YOUTUBE_TEST_URL");
//         let formats = get_media_items(&youtube_url, &state.config).await.unwrap();
//         let lowest_format = formats
//             .first()
//             .expect("YouTube formats should not be empty");
//
//         let response = download_handler(
//             State(state),
//             Query(DownloadParams {
//                 url: youtube_url,
//                 download_option_id: lowest_format.format_id.clone(),
//                 keep_on_server: false,
//             }),
//         )
//         .await
//         .unwrap();
//
//         assert_eq!(response.status(), StatusCode::OK);
//         assert_eq!(response.headers().get("content-type").unwrap(), "video/mp4");
//         assert!(response.headers().contains_key("content-disposition"));
//         assert!(
//             !Path::new(&test_state().config.downloads_directory)
//                 .join("foO1QakhjZE.mp4")
//                 .exists()
//         );
//     }
//
//     #[test]
//     fn router_exposes_api_routes() {
//         let _router = api_router(Arc::new(test_state()));
//     }
//
//     #[tokio::test]
//     async fn ui_page_loads_formats_and_starts_downloads() {
//         let page = ui_handler().await.0;
//
//         assert!(page.contains("/formats?url="));
//         assert!(page.contains("format.format_id"));
//         assert!(page.contains("/download?url="));
//         assert!(page.contains("format_id="));
//         assert!(page.contains("Download complete"));
//         assert!(!page.contains("form.reset()"));
//         assert!(page.contains("formats.replaceChildren()"));
//         assert!(page.contains("@media (prefers-color-scheme: dark)"));
//         assert!(page.contains("download.blob()"));
//         assert!(page.contains("Content-Disposition"));
//     }
//
//     #[test]
//     fn download_params_defaults_to_not_keep_on_server() {
//         let download_params: DownloadParams = serde_json::from_value(serde_json::json!({
//             "url": "https://example.com/video",
//             "format_id": "best"
//         }))
//         .unwrap();
//
//         assert!(!download_params.keep_on_server);
//     }
// }
//
// #[cfg(test)]
// mod cli_tests {
//     use super::*;
//     use std::path::Path;
//
//     fn cleanup_download() {
//         let path = test_state()
//             .config
//             .downloads_directory
//             .join("foO1QakhjZE.mp4");
//         let _ = std::fs::remove_file(path);
//     }
//
//     fn format(format_id: &str) -> YtDlpFormat {
//         YtDlpFormat {
//             format_id: format_id.to_owned(),
//             height: Some(360),
//             ext: "mp4".to_owned(),
//             vcodec: Some("h264".to_owned()),
//             acodec: Some("aac".to_owned()),
//             filesize: None,
//         }
//     }
//
//     #[tokio::test]
//     async fn youtube_cli_lists_formats_downloads_lowest_format_and_creates_file() {
//         let _guard = LIVE_TEST_LOCK
//             .get_or_init(|| Mutex::new(()))
//             .lock()
//             .unwrap();
//         cleanup_download();
//
//         let state = test_state();
//         let youtube_url = get_env_variable("YOUTUBE_TEST_URL");
//         let formats = get_media_items(&youtube_url, &state.config).await.unwrap();
//         let lowest_format = formats
//             .first()
//             .expect("YouTube formats should not be empty");
//
//         download_video(&youtube_url, &lowest_format.format_id, &state)
//             .await
//             .unwrap();
//
//         assert!(
//             Path::new(&state.config.downloads_directory)
//                 .join("foO1QakhjZE.mp4")
//                 .exists()
//         );
//         cleanup_download();
//     }
// }
//
// #[cfg(test)]
// mod downloader_unit_tests {
//     use super::*;
//
//     #[test]
//     fn default_downloader_returns_only_best_format_without_metadata() {
//         let formats = FacebookProvider
//             .get_media_items(&get_env_variable("FACEBOOK_TEST_URL"))
//             .unwrap();
//
//         assert_eq!(formats.len(), 1);
//         assert_eq!(
//             serde_json::to_value(FormatResponse::from(formats[0].clone())).unwrap(),
//             serde_json::json!({ "format_id": "best", "height": 0, "filesize": null })
//         );
//     }
//
//     #[test]
//     fn default_downloader_is_used_by_each_non_youtube_platform() {
//         for downloader in [
//             Box::new(FacebookProvider) as Box<dyn ContentProvider>,
//             Box::new(XProvider),
//             Box::new(TikTokProvider),
//         ] {
//             assert_eq!(
//                 downloader
//                     .get_media_items(&get_env_variable("EXAMPLE_TEST_URL"))
//                     .unwrap()[0]
//                     .format_id,
//                 "best"
//             );
//         }
//     }
//
//     #[test]
//     fn format_selection_keeps_the_closest_format_for_each_resolution() {
//         let formats = vec![
//             YtDlpFormat {
//                 format_id: "144".to_owned(),
//                 height: Some(144),
//                 ext: "mp4".to_owned(),
//                 vcodec: Some("avc1".to_owned()),
//                 acodec: Some("aac".to_owned()),
//                 filesize: None,
//             },
//             YtDlpFormat {
//                 format_id: "360".to_owned(),
//                 height: Some(360),
//                 ext: "mp4".to_owned(),
//                 vcodec: Some("avc1".to_owned()),
//                 acodec: Some("aac".to_owned()),
//                 filesize: None,
//             },
//             YtDlpFormat {
//                 format_id: "1080".to_owned(),
//                 height: Some(1080),
//                 ext: "mp4".to_owned(),
//                 vcodec: Some("avc1".to_owned()),
//                 acodec: Some("aac".to_owned()),
//                 filesize: None,
//             },
//         ];
//
//         let selected: Vec<YtDlpFormat> = deduplicate_by_resolution(formats)
//             .into_iter()
//             .flatten()
//             .collect();
//
//         assert_eq!(
//             selected
//                 .iter()
//                 .map(|format| format.format_id.as_str())
//                 .collect::<Vec<_>>(),
//             ["144", "360", "1080"]
//         );
//     }
// }

// #[test]
// fn test_command() {
//     let video_details = Command::new("yt-dlp")
//         .arg("--dump-single-json")
//         .arg("--skip-download")
//         .arg("--cookies")
//         // .arg("cookies/instagram-cookies.txt")
//         .arg("cookies/youtube-cookies.txt")
//         // .arg("https://www.instagram.com/stories/fhavur8/3995189932905526257?utm_source=ig_story_item_share&stkn=MW9xaTMydDlvaHVzMQ==")
//         .arg("https://youtu.be/foO1QakhjZE?si=tRel8JwJ_kmZJoul")
//         .stdout(Stdio::piped())
//         .spawn()
//         .map_err(|error| Error::YtDlpExecutionFailed(error.to_string()))
//         .unwrap();
//
//     let formats_bytes = Command::new("jq")
//         // .arg(r#"[.formats[] | select(.vcodec != "none" and .height != null) | {format_id, height, ext, vcodec, acodec, filesize}]"#)
//         .arg(
//             r#"
//         {
//           title,
//           thumbnail,
//           is_playlist: (.entries != null),
//           item_count: (if .entries then (.entries | length) else 1 end),
//           formats: [
//             .formats[]?
//             | select(.vcodec != "none" and .height != null)
//             | {format_id, height, ext, vcodec, acodec, filesize}
//           ],
//           entries: [
//             .entries[]? | {
//                 title,
//                 thumbnail,
//                 formats: [
//                     .formats[]
//                     | select(.vcodec != "none" and .height != null)
//                     | {format_id, height, ext, vcodec, acodec, filesize}
//                 ]
//             }
//           ]
//         }
//         "#,
//         )
//         .stdin(video_details.stdout.unwrap())
//         .output()
//         .map_err(|error| Error::JqExecutionFailed(error.to_string()))
//         .unwrap();
//
//     if formats_bytes.status.success() {
//         let formats_string = String::from_utf8(formats_bytes.stdout).unwrap();
//         // println!("As a string: {formats_string}");
//         let media_metadata: MediaMetadata = serde_json::from_str(&formats_string).unwrap();
//         println!("{}", serde_json::to_string_pretty(&media_metadata).unwrap());
//     } else {
//         println!("JQ execution failed");
//     }
// }
