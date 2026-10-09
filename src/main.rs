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
    #[serde(default)]
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
        !self.entries.is_empty()
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
    config.downloads_directory.join("%(title)s.%(ext)s")
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
struct FormatResolutionBuckets {
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

impl IntoIterator for FormatResolutionBuckets {
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

fn deduplicate_by_resolution(mut formats: Vec<YtDlpFormat>) -> FormatResolutionBuckets {
    let mut resolution_buckets = FormatResolutionBuckets::default();

    // Sort formats by height and then by filesize presence
    formats.sort_by(|a, b| {
        a.height
            .cmp(&b.height)
            .then_with(|| a.filesize.is_some().cmp(&b.filesize.is_some()))
    });

    for format in formats {
        match format.height {
            Some(res) if res <= 144 => resolution_buckets.closest_to_144p = Some(format),
            Some(res) if res <= 144 + 240 / 2 => {
                resolution_buckets.closest_to_240p = Some(format)
            }
            Some(res) if res <= 240 + 360 / 2 => {
                resolution_buckets.closest_to_360p = Some(format)
            }
            Some(res) if res <= 360 + 480 / 2 => {
                resolution_buckets.closest_to_480p = Some(format)
            }
            Some(res) if res <= 480 + 720 / 2 => {
                resolution_buckets.closest_to_720p = Some(format)
            }
            Some(res) if res <= 720 + 1080 / 2 => {
                resolution_buckets.closest_to_1080p = Some(format)
            }
            Some(res) if res <= 1080 + 1440 / 2 => {
                resolution_buckets.closest_to_1440p = Some(format)
            }
            Some(res) if res <= 1440 + 2160 / 2 => {
                resolution_buckets.closest_to_2160p = Some(format)
            }
            Some(res) if res <= 2160 + 4320 / 2 => {
                resolution_buckets.closest_to_4320p = Some(format)
            }
            Some(_) => resolution_buckets.larger_than_4320p = Some(format),
            None => {}
        }
    }

    resolution_buckets
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
struct GetMediaInfoRequest {
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
            .output_path(get_download_file_path(&self.config))
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
        .route("/media-info", get(get_media_info_handler))
        .route("/download", get(download_handler))
        .with_state(state)
}

async fn ui_handler() -> Html<&'static str> {
    Html(include_str!("resources/index.html"))
}

async fn get_media_info_handler(
    State(state): State<Arc<AppState>>,
    Query(request): Query<GetMediaInfoRequest>,
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
mod unit_tests {
    use super::*;
    use axum::body::to_bytes;
    use std::fs;

    fn format(id: &str, height: Option<u16>) -> YtDlpFormat {
        YtDlpFormat {
            format_id: id.to_owned(),
            height,
            ext: "mp4".to_owned(),
            vcodec: Some("avc1.640028".to_owned()),
            acodec: Some("mp4a.40.2".to_owned()),
            filesize: Some(100),
        }
    }

    fn config() -> Config {
        Config {
            instagram_cookies_path: "instagram.cookies".into(),
            youtube_cookies_path: "youtube.cookies".into(),
            downloads_directory: "downloads".into(),
        }
    }

    #[test]
    fn content_types_cover_known_extensions_and_fallback() {
        let cases = [
            ("mp4", ContentType::Mp4, "video/mp4"),
            ("WEBM", ContentType::WebM, "video/webm"),
            ("mov", ContentType::QuickTime, "video/quicktime"),
            ("m4v", ContentType::M4v, "video/x-m4v"),
            ("mkv", ContentType::Matroska, "video/x-matroska"),
            ("jpg", ContentType::Jpeg, "image/jpeg"),
            ("jpeg", ContentType::Jpeg, "image/jpeg"),
            ("png", ContentType::Png, "image/png"),
            ("webp", ContentType::WebP, "image/webp"),
            ("gif", ContentType::Gif, "image/gif"),
            ("avif", ContentType::Avif, "image/avif"),
            ("heic", ContentType::Heic, "image/heic"),
            ("bin", ContentType::OctetStream, "application/octet-stream"),
        ];
        for (extension, expected, mime) in cases {
            assert_eq!(ContentType::from_extension(extension), expected);
            assert_eq!(expected.as_str(), mime);
        }
    }

    #[test]
    fn temporary_file_reports_metadata_and_removes_it_after_read() {
        let path = std::env::temp_dir().join(format!("downloader-{}.mp4", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        let file = TempDownloadedFile::try_new(path.clone()).unwrap();
        assert_eq!(file.file_name().unwrap(), path.file_name().unwrap().to_str().unwrap());
        assert_eq!(file.get_size().unwrap(), 3);
        assert_eq!(file.content_type().unwrap(), ContentType::Mp4);
        assert_eq!(file.read_and_remove().unwrap(), b"abc");
        assert!(!path.exists());
    }

    #[test]
    fn temporary_file_rejects_directories_and_missing_paths() {
        let directory = std::env::temp_dir().join(format!("downloader-dir-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        assert!(matches!(TempDownloadedFile::try_new(directory.clone()), Err(Error::NotAValidPath(_))));
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn metadata_converts_single_items_and_playlists() {
        let single = YtDlpMediaMetadata {
            title: "title".into(),
            thumbnail: Some("thumb".into()),
            formats: vec![format("f1", Some(360))],
            entries: vec![],
        };
        assert!(!single.is_playlist());
        let converted: MediaItems = single.clone().try_into().unwrap();
        assert_eq!(converted[0].title, "title");
        assert_eq!(converted[0].thumbnail, "thumb");
        assert_eq!(converted[0].formats[0].format_id, "f1");
        assert_eq!(single.get_format_by_id("f1", None).unwrap().format_id, "f1");

        let playlist = YtDlpMediaMetadata {
            title: "playlist".into(),
            thumbnail: None,
            formats: vec![],
            entries: vec![format_entry("entry")],
        };
        assert!(playlist.is_playlist());
        assert_eq!(playlist.entry_count(), 1);
        assert_eq!(playlist.get_format_by_id("entry-format", Some(0)).unwrap().format_id, "entry-format");
        assert!(playlist.get_format_by_id("missing", Some(5)).is_none());
    }

    fn format_entry(title: &str) -> YtDlpMediaEntryMetadata {
        YtDlpMediaEntryMetadata { title: title.into(), thumbnail: "thumb".into(), formats: vec![format("entry-format", Some(240))] }
    }

    #[test]
    fn metadata_without_thumbnail_is_rejected_for_single_item() {
        let metadata = YtDlpMediaMetadata { title: "title".into(), thumbnail: None, formats: vec![], entries: vec![] };
        assert!(matches!(TryInto::<MediaItems>::try_into(metadata), Err(Error::Internal(message)) if message.contains("thumbnail")));
    }

    #[test]
    fn resolution_deduplication_groups_all_boundaries_and_ignores_audio_only_formats() {
        let mut formats = vec![format("none", None)];
        for (id, height) in [("144", 144), ("240", 240), ("360", 360), ("480", 480), ("720", 720), ("1080", 1080), ("1440", 1440), ("2160", 2160), ("4320", 4320), ("4321", 4321)] {
            formats.push(format(id, Some(height)));
        }
        let selected: Vec<_> = deduplicate_by_resolution(formats).into_iter().flatten().collect();
        assert_eq!(selected.iter().map(|item| item.format_id.as_str()).collect::<Vec<_>>(), vec!["144", "240", "360", "480", "720", "1080", "1440", "2160", "4320", "4321"]);
    }

    #[test]
    fn predicates_distinguish_video_and_iphone_formats() {
        let compatible = format("ok", Some(360));
        let mut wrong_extension = compatible.clone();
        wrong_extension.ext = "webm".into();
        let mut wrong_codec = compatible.clone();
        wrong_codec.vcodec = Some("vp9".into());
        let mut audio = compatible.clone();
        audio.height = None;
        audio.vcodec = Some("none".into());
        assert!(IsIphoneCompatible.matches(&compatible));
        assert!(!IsIphoneCompatible.matches(&wrong_extension));
        assert!(!IsIphoneCompatible.matches(&wrong_codec));
        assert!(IsVideoFormat.matches(&compatible));
        assert!(!IsVideoFormat.matches(&audio));
    }

    #[test]
    fn filtering_handles_single_media_and_playlist_entries() {
        let mut metadata = YtDlpMediaMetadata { title: "t".into(), thumbnail: Some("u".into()), formats: vec![format("ok", Some(360)), format("audio", None)], entries: vec![] };
        filter_media_metadata_formats(&mut metadata, &[&IsVideoFormat]).unwrap();
        assert_eq!(metadata.formats.len(), 1);

        let mut playlist = YtDlpMediaMetadata { title: "p".into(), thumbnail: None, formats: vec![], entries: vec![format_entry("entry")] };
        filter_media_metadata_formats(&mut playlist, &[&IsIphoneCompatible]).unwrap();
        assert_eq!(playlist.entries[0].formats.len(), 1);
    }

    #[test]
    fn builders_require_url_and_output_and_preserve_options() {
        assert!(YtDlpBuilder::new().build().is_err());
        assert!(YtDlpBuilder::new().url("u").build().is_err());
        let client = YtDlpBuilder::new().url("u").format_id("f").cookies_path("c").output_path("o").merge_mp4().default_to_best_audio().build().unwrap();
        assert_eq!(client.url, "u");
        assert_eq!(client.format_id.as_deref(), Some("f"));
        assert_eq!(client.cookies_path.as_deref(), Some(PathBuf::from("c").as_path()));
        assert!(client.merge_mp4 && client.default_to_best_audio);
        assert_eq!(YtDlpFormat::best().format_id, "best");
    }

    #[test]
    fn platform_selection_accepts_supported_hosts_and_rejects_bad_urls() {
        assert!(matches!(get_platform_from_url("https://youtu.be/id"), Ok(Platform::YouTube)));
        assert!(matches!(get_platform_from_url("https://www.facebook.com/id"), Ok(Platform::Facebook)));
        assert!(matches!(get_platform_from_url("https://instagram.com/id"), Ok(Platform::Instagram)));
        assert!(matches!(get_platform_from_url("https://x.com/id"), Ok(Platform::X)));
        assert!(matches!(get_platform_from_url("https://tiktok.com/id"), Ok(Platform::TikTok)));
        assert!(matches!(get_platform_from_url("https://example.com/id"), Ok(Platform::Unsupported(host)) if host == "example.com"));
        assert!(matches!(get_platform_from_url("not a url"), Err(Error::InvalidInput(_))));
    }

    #[test]
    fn selection_and_choice_parsing_report_invalid_input() {
        assert_eq!(*select_index(&[10, 20], 2).unwrap(), 20);
        assert!(select_index(&[10], 0).is_err());
        assert!(select_index(&[10], 2).is_err());
        assert_eq!(parse_choice_to_usize("12").unwrap(), 12);
        assert!(parse_choice_to_usize("x").is_err());
    }

    #[test]
    fn provider_cache_selects_formats_and_reports_missing_indices() {
        let state = AppState { config: config(), media_cache: RwLock::new(HashMap::new()) };
        state.media_cache.write().unwrap().insert("url".into(), vec![MediaInfo { title: "t".into(), thumbnail: "thumb".into(), formats: vec![MediaFormat { format_id: "f".into(), height: Some(1), filesize: None }] }]);
        let params = DownloadParams { url: "url".into(), download_option_index: 0, item_index: 0, keep_on_server: false };
        assert_eq!(FacebookProvider.get_format_for_download(&state.media_cache, &params).unwrap().format_id, "f");
        assert!(FacebookProvider.get_format_for_download(&state.media_cache, &DownloadParams { url: "url".into(), download_option_index: 0, item_index: 1, keep_on_server: false }).is_err());
        assert!(FacebookProvider.get_format_for_download(&state.media_cache, &DownloadParams { url: "url".into(), download_option_index: 1, item_index: 0, keep_on_server: false }).is_err());
    }

    #[tokio::test]
    async fn errors_map_to_expected_http_responses() {
        let response = Error::InvalidInput("bad".into()).into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let response = Error::YtDlpExecutionFailed("details".into()).into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let response = Error::Internal("details".into()).into_response();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), b"details");
    }

    #[test]
    fn request_structs_deserialize_defaults_and_config_paths_are_used() {
        let request: GetMediaInfoRequest = serde_json::from_str(r#"{"url":"u"}"#).unwrap();
        let params: DownloadParams = serde_json::from_str(r#"{"url":"u","download_option_index":1,"item_index":2}"#).unwrap();
        assert_eq!(request.url, "u");
        assert!(!params.keep_on_server);
        assert_eq!(get_download_file_path(&config()), PathBuf::from("downloads/%(id)s.%(ext)s"));
    }

    #[test]
    fn provider_and_application_state_structs_hold_current_configuration() {
        let configuration = config();
        let state = AppState {
            config: configuration.clone(),
            media_cache: RwLock::new(HashMap::new()),
        };
        let youtube = YouTubeProvider { config: configuration.clone() };
        let instagram = InstagramProvider;
        let facebook = FacebookProvider;
        let x = XProvider;
        let tiktok = TikTokProvider;
        assert_eq!(state.config.downloads_directory, PathBuf::from("downloads"));
        assert_eq!(youtube.config.youtube_cookies_path, PathBuf::from("youtube.cookies"));
        let _providers: [&dyn ContentProvider; 4] = [&facebook, &instagram, &x, &tiktok];
    }
}