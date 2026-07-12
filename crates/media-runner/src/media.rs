use std::path::Path;

use crate::SensitiveUrl;

#[derive(Debug, Clone, PartialEq)]
pub struct MediaProbe {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub duration_seconds: f64,
    pub bitrate: Option<u64>,
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

pub fn build_hls_ingest_command(
    input: &SensitiveUrl,
    output: &Path,
) -> Result<ProcessCommand, MediaProbeError> {
    let output = output.to_str().ok_or(MediaProbeError)?;
    Ok(ProcessCommand {
        program: "ffmpeg".to_owned(),
        args: vec![
            "-nostdin".to_owned(),
            "-hide_banner".to_owned(),
            "-loglevel".to_owned(),
            "error".to_owned(),
            "-y".to_owned(),
            "-rw_timeout".to_owned(),
            "30000000".to_owned(),
            "-i".to_owned(),
            input.as_url().as_str().to_owned(),
            "-map".to_owned(),
            "0:v:0".to_owned(),
            "-map".to_owned(),
            "0:a?".to_owned(),
            "-c".to_owned(),
            "copy".to_owned(),
            "-f".to_owned(),
            "matroska".to_owned(),
            output.to_owned(),
        ],
    })
}

pub fn build_rezka_vaapi_command(
    input: &Path,
    output: &Path,
    device: &Path,
    probe: &MediaProbe,
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

    Ok(ProcessCommand {
        program: "ffmpeg".to_owned(),
        args: vec![
            "-nostdin".to_owned(),
            "-hide_banner".to_owned(),
            "-loglevel".to_owned(),
            "error".to_owned(),
            "-y".to_owned(),
            "-vaapi_device".to_owned(),
            device.to_owned(),
            "-i".to_owned(),
            input.to_owned(),
            "-vf".to_owned(),
            "format=nv12,hwupload".to_owned(),
            "-c:v".to_owned(),
            "hevc_vaapi".to_owned(),
            "-c:a".to_owned(),
            "copy".to_owned(),
            "-map_metadata".to_owned(),
            "0".to_owned(),
            output.to_owned(),
        ],
    })
}
