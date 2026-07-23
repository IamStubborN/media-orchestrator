use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct MediaProbe {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub duration_seconds: f64,
    pub bitrate: Option<u64>,
    pub video_profile: Option<String>,
    pub audio_language: Option<String>,
    pub audio_title: Option<String>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<u32>,
    pub audio_channel_layout: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AudioTrackMetadata {
    pub language: String,
    pub title: String,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, thiserror::Error)]
#[error("media probe is invalid")]
pub struct MediaProbeError;

#[derive(Clone, Eq, PartialEq)]
pub struct ProcessCommand {
    program: String,
    args: Vec<String>,
}

impl std::fmt::Debug for ProcessCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessCommand")
            .field("program", &self.program)
            .field("args", &"[REDACTED]")
            .finish()
    }
}

impl ProcessCommand {
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

pub fn build_rezka_vaapi_command(
    input: &Path,
    output: &Path,
    device: &Path,
    probe: &MediaProbe,
    audio: Option<&AudioTrackMetadata>,
) -> Result<ProcessCommand, MediaProbeError> {
    if probe.codec.is_empty()
        || probe.width == 0
        || probe.height == 0
        || !probe.duration_seconds.is_finite()
        || probe.duration_seconds <= 0.0
    {
        return Err(MediaProbeError);
    }
    let input = input.to_str().ok_or(MediaProbeError)?;
    let output = output.to_str().ok_or(MediaProbeError)?;
    let device = device.to_str().ok_or(MediaProbeError)?;

    let mut args = vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "error".to_owned(),
        "-y".to_owned(),
        "-vaapi_device".to_owned(),
        device.to_owned(),
        "-hwaccel".to_owned(),
        "vaapi".to_owned(),
        "-hwaccel_output_format".to_owned(),
        "vaapi".to_owned(),
        "-i".to_owned(),
        input.to_owned(),
        "-vf".to_owned(),
        "scale_vaapi=w=1920:h=1080:mode=fast".to_owned(),
        "-c:v".to_owned(),
        "hevc_vaapi".to_owned(),
        "-c:a".to_owned(),
        "copy".to_owned(),
        "-map_metadata".to_owned(),
        "0".to_owned(),
    ];
    if let Some(audio) = audio {
        if audio.language.len() != 3
            || !audio.language.bytes().all(|byte| byte.is_ascii_lowercase())
            || audio.title.trim().is_empty()
        {
            return Err(MediaProbeError);
        }
        args.extend([
            "-metadata:s:a:0".to_owned(),
            format!("language={}", audio.language),
            "-metadata:s:a:0".to_owned(),
            format!("title={}", audio.title.trim()),
        ]);
    }
    args.push(output.to_owned());

    Ok(ProcessCommand {
        program: "ffmpeg".to_owned(),
        args,
    })
}
