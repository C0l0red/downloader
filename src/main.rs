use crate::FileSizeUnit::{Bytes, Gigabytes, Kilobytes, Megabytes};
use regex::Regex;
use serde::de::Error;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Display;
use std::process::{Command, Stdio};

enum AppError {
    InvalidResolution(u16, u16),
    MissingField(&'static str),
}

fn round_down_to_2_decimal_places(value: f32) -> f32 {
    (value * 100.0).ceil() / 100.0
}

#[derive(Deserialize, Debug)]
struct RawFileFormat {
    format_id: String,
    ext: String,
    // If not provided, can be approximated with (tbr x duration in seconds x 125)
    filesize: Option<f64>,
    #[serde(default = "default_codec")]
    acodec: String,
    #[serde(default = "default_codec")]
    vcodec: String,
    height: Option<u16>,
    width: Option<u16>,
    tbr: Option<f64>,
}

#[derive(Debug, PartialOrd, PartialEq, Clone)]
enum FileSizeUnit {
    Bytes,
    Kilobytes,
    Megabytes,
    Gigabytes,
}

#[derive(Debug, PartialOrd, PartialEq, Eq, Hash, Clone)]
enum Resolution {
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

#[derive(Debug, PartialEq, Clone)]
struct FileSize {
    size: f32,
    unit: FileSizeUnit,
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
enum FileEncoding {
    VideoAndAudio,
    VideoOnly,
    AudioOnly,
    Image,
    Unknown,
}

#[derive(Debug, Clone)]
struct FileFormat {
    id: String,
    extension: String,
    resolution: Option<Resolution>,
    file_size: FileSize,
    file_encoding: FileEncoding,
}

#[derive(Debug)]
struct FileDetails {
    title: String,
    duration: f64,
    ext: String,
    extractor: String,
    extractor_key: String,
    formats: Vec<FileFormat>,
}

impl FileSize {
    fn new(size_in_bytes: f64) -> FileSize {
        let size_in_kilobytes = (size_in_bytes / 1024f64) as f32;
        if size_in_kilobytes < 1f32 {
            return FileSize {
                size: round_down_to_2_decimal_places(size_in_bytes as f32),
                unit: Bytes,
            };
        }

        let size_in_megabytes = size_in_kilobytes / 1024f32;
        if size_in_megabytes < 1f32 {
            return FileSize {
                size: round_down_to_2_decimal_places(size_in_kilobytes),
                unit: Kilobytes,
            };
        }

        let size_in_gigabytes = size_in_megabytes / 1024f32;
        if size_in_gigabytes < 1f32 {
            return FileSize {
                size: round_down_to_2_decimal_places(size_in_megabytes),
                unit: Megabytes,
            };
        }

        FileSize {
            size: round_down_to_2_decimal_places(size_in_gigabytes),
            unit: Gigabytes,
        }
    }
}

impl Display for FileSize {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self.unit {
            Bytes => write!(f, "{}B", self.size),
            Kilobytes => write!(f, "{}KB", self.size),
            Megabytes => write!(f, "{}MB", self.size),
            Gigabytes => write!(f, "{}GB", self.size),
        }
    }
}

impl PartialOrd for FileSize {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.unit.eq(&other.unit) {
            return Some(self.size.partial_cmp(&other.size).unwrap());
        }
        Some(self.unit.partial_cmp(&other.unit).unwrap())
    }
}

impl Resolution {
    fn try_new(width: u16, height: u16) -> Result<Resolution, AppError> {
        match (width, height) {
            (4320, _) | (_, 4320) => Ok(Resolution::P4320),
            (2160, _) | (_, 2160) => Ok(Resolution::P2160),
            (1440, _) | (_, 1440) => Ok(Resolution::P1440),
            (1080, _) | (_, 1080) => Ok(Resolution::P1080),
            (720, _) | (_, 720) => Ok(Resolution::P720),
            (480, _) | (_, 480) => Ok(Resolution::P480),
            (360, _) | (_, 360) => Ok(Resolution::P360),
            (240, _) | (_, 240) => Ok(Resolution::P240),
            (144, _) | (_, 144) => Ok(Resolution::P144),
            _ => Err(AppError::InvalidResolution(width, height)),
        }
    }
}

impl Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Resolution::P144 => write!(f, "144p"),
            Resolution::P240 => write!(f, "240p"),
            Resolution::P360 => write!(f, "360p"),
            Resolution::P480 => write!(f, "480p"),
            Resolution::P720 => write!(f, "720p"),
            Resolution::P1080 => write!(f, "1080p"),
            Resolution::P1440 => write!(f, "1440p"),
            Resolution::P2160 => write!(f, "2160p"),
            Resolution::P4320 => write!(f, "4320p"),
        }
    }
}

fn default_codec() -> String {
    "unknown".to_string()
}

impl<'de> Deserialize<'de> for FileDetails {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(d)?;
        let title = value
            .get("title")
            .and_then(|v| v.as_str())
            .ok_or_else(|| D::Error::custom("missing title field"))?
            .to_string();
        let duration = value
            .get("duration")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| D::Error::custom("missing duration field"))?;
        let ext = value
            .get("ext")
            .and_then(|v| v.as_str())
            .ok_or_else(|| D::Error::custom("missing ext field"))?
            .to_string();
        let extractor = value
            .get("extractor")
            .and_then(|v| v.as_str())
            .ok_or_else(|| D::Error::custom("missing extractor field"))?
            .to_string();
        let extractor_key = value
            .get("extractor_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| D::Error::custom("missing extractor_key field"))?
            .to_string();
        let json_formats = value
            .get("formats")
            .and_then(|v| v.as_array())
            .ok_or_else(|| D::Error::custom("missing formats field"))?;
        let mut formats = vec![];

        for format in json_formats {
            let raw_file_format: RawFileFormat = serde_json::from_value(format.clone())
                .map_err(|e| D::Error::custom(e.to_string()))?;
            let file_format = match FileFormat::try_new(raw_file_format, duration) {
                Ok(file_format) => file_format,
                _ => continue,
            };
            formats.push(file_format);
        }

        Ok(Self {
            title,
            duration,
            ext,
            extractor,
            extractor_key,
            formats,
        })
    }
}

impl Display for FileDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let formats = self
            .formats
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
            .join("\n\t");
        write!(
            f,
            "FileDetails (\ntitle: {},\nduration: {},\n\
            ext: {},\nextractor: {},\nextractor_key: {},\nformats: {}\n)",
            self.title, self.duration, self.ext, self.extractor, self.extractor_key, formats
        )
    }
}

impl Display for FileFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let resolution = if let Some(resolution) = &self.resolution {
            resolution.to_string()
        } else {
            "None".to_string()
        };
        write!(
            f,
            r#"FileFormat (id: {}, extension: {}, resolution: {}, file size: {}, file encoding: {})"#,
            self.id, self.extension, resolution, self.file_size, self.file_encoding
        )
    }
}

impl FileFormat {
    fn try_new(raw: RawFileFormat, duration: f64) -> Result<FileFormat, AppError> {
        let resolution = match (raw.width, raw.height) {
            (Some(width), Some(height)) => Some(Resolution::try_new(width, height)?),
            _ => None,
        };

        let file_size = match raw.filesize {
            Some(filesize) => FileSize::new(filesize),
            None => {
                let Some(tbr) = raw.tbr else {
                    return Err(AppError::MissingField("tbr"));
                };
                let file_size = duration * tbr * 125f64;
                FileSize::new(file_size)
            }
        };

        Ok(FileFormat {
            id: raw.format_id.clone(),
            extension: raw.ext.clone(),
            resolution,
            file_size,
            file_encoding: FileEncoding::from(raw),
        })
    }
}

impl From<RawFileFormat> for FileEncoding {
    fn from(value: RawFileFormat) -> Self {
        match (
            value.acodec.as_str(),
            value.vcodec.as_str(),
            value.width,
            value.height,
        ) {
            (acodec, vcodec, Some(_), Some(_)) if acodec != "none" && vcodec != "none" => {
                FileEncoding::VideoAndAudio
            }
            ("none", vcodec, Some(_), Some(_)) if vcodec != "none" => FileEncoding::VideoOnly,
            (acodec, "none", None, None) if acodec != "none" => FileEncoding::AudioOnly,
            ("none", vcodec, Some(_), Some(_)) if vcodec != "none" => FileEncoding::Image,
            _ => FileEncoding::Unknown,
        }
    }
}

impl Display for FileEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            FileEncoding::VideoAndAudio => write!(f, "Video and Audio"),
            FileEncoding::VideoOnly => write!(f, "Video Only"),
            FileEncoding::AudioOnly => write!(f, "Audio Only"),
            FileEncoding::Image => write!(f, "Image"),
            FileEncoding::Unknown => write!(f, "Unknown"),
        }
    }
}

const YOUTUBE_REGEX: Regex = Regex::new(r"https?://(www\.)?(youtube\.com|youtu\.be)/.+").unwrap();
const INSTAGRAM_REGEX: Regex =
    Regex::new(r"https?://(www\.)?instagram\.com/(p|reel|stories)/[A-Za-z0-9_.-]+(/[\w-]+)?/?")
        .unwrap();

enum Extractor {
    Instagram(InstagramContentType),
    Youtube,
}

enum InstagramContentType {
    Story,
    Post,
    Reel,
}

fn get_extractor(url: &str) -> Option<Extractor> {
    if YOUTUBE_REGEX.is_match(url) {
        Some(Extractor::Youtube)
    } else if INSTAGRAM_REGEX.is_match(url) {
        get_instagram_content_type(url).map(|content_type| Extractor::Instagram(content_type))
    } else {
        None
    }
}

fn get_instagram_content_type(url: &str) -> Option<InstagramContentType> {
    if let Some(captures) = INSTAGRAM_REGEX.captures(url) {
        match captures.get(2).map(|m| m.as_str()) {
            Some("p") => Some(InstagramContentType::Post),
            Some("reel") => Some(InstagramContentType::Reel),
            Some("stories") => Some(InstagramContentType::Story),
            _ => None,
        }
    } else {
        None
    }
}

fn get_file_formats() -> () {
    let output = Command::new("yt-dlp")
        .arg("-q")
        .args([
            "-J",
            "https://www.instagram.com/stories/pretty._wendy/3593101623165471287?igsh=YjJxbnoycDA2ZHNk",
        ])
        .stderr(Stdio::null()) // Suppresses unnecessary download messages
        .output()
        .expect("Failed to execute yt-dlp");

    // Check if the command was successful
    if !output.status.success() {
        eprintln!("yt-dlp command failed. {output:#?}");
        return;
    }

    // Print the output as a string
    let result = String::from_utf8_lossy(&output.stdout);
    let json: Value = serde_json::from_str(&result).unwrap();
    let file_details: FileDetails = serde_json::from_value(json).unwrap();
    println!("{}", file_details);

    let mut best_formats = BestFormats::new();

    // for format in file_details.formats {
    //     match format.file_encoding {
    //         FileEncoding::VideoAndAudio => {
    //             let Some(resolution) = format.resolution.clone() else {
    //                 continue;
    //             };
    //             best_formats.video_and_audio.entry(resolution).and_modify(|best_format| {
    //                 if format.file_size > best_format.file_size {
    //                     *best_format = format.clone();
    //                 }
    //             }).or_insert(format);
    //         },
    //         FileEncoding::VideoOnly => {
    //             let Some(resolution) = format.resolution.clone() else {
    //                 continue;
    //             };
    //             best_formats.video_only.entry(resolution).and_modify(|best_format| {
    //                 if format.file_size > best_format.file_size {
    //                     *best_format = format.clone();
    //                 }
    //             }).or_insert(format);
    //         },
    //         FileEncoding::AudioOnly => {
    //             if let Some(best_format) = best_formats.audio_only.clone() {
    //                 if format.file_size > best_format.file_size {
    //                     best_formats.audio_only = Some(format);
    //                 }
    //             }
    //         },
    //         _ => continue,
    //     }
    // }
}

struct BestFormats {
    video_and_audio: HashMap<Resolution, FileFormat>,
    video_only: HashMap<Resolution, FileFormat>,
    audio_only: Option<FileFormat>,
}

impl BestFormats {
    fn new() -> Self {
        Self {
            video_and_audio: HashMap::new(),
            video_only: HashMap::new(),
            audio_only: None,
        }
    }
}

fn main() {
    get_file_formats();
}
