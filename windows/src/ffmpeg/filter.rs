use crate::config::Config;
use super::{OUTPUT_H, OUTPUT_W};

/// Builds a `-vf` filter string from the current config.
///
/// Order matters for latency and predictable geometry:
/// rotate -> crop to 16:9 -> scale to fixed 720p -> set square pixels.
pub(super) fn build_vf_filter(cfg: &Config) -> String {
    let mut steps: Vec<String> = Vec::new();

    // Normalize deprecated yuvj420p: set full-range metadata then re-tag
    // the pixel format so swscaler never sees the deprecated yuvj420p tag.
    steps.push("setrange=full".to_string());
    steps.push("format=yuv420p".to_string());

    match cfg.rotation {
        90 => steps.push("transpose=1".to_string()),
        180 => steps.push("hflip,vflip".to_string()),
        270 => steps.push("transpose=2".to_string()),
        _ => {}
    }

    steps.push(format!(
        "crop=iw:iw*{OUTPUT_H}/{OUTPUT_W}:0:(ih-iw*{OUTPUT_H}/{OUTPUT_W})/2"
    ));

    // Consumers usually expect limited-range YUV in camera pipelines.
    steps.push(format!(
        "scale={OUTPUT_W}:{OUTPUT_H}:in_range=full:out_range=limited"
    ));

    steps.push("setsar=1".to_string());
    steps.join(",")
}

pub(super) fn build_ffmpeg_args(cfg: &Config, tcp_url: &str, vf: &str) -> Vec<String> {
    let fps_str = cfg.fps.to_string();
    vec![
        "-hide_banner".into(),
        "-fflags".into(),
        "nobuffer".into(),
        "-flags".into(),
        "low_delay".into(),
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        "-thread_queue_size".into(),
        "64".into(),
        "-f".into(),
        "mpjpeg".into(),
        "-i".into(),
        tcp_url.to_string(),
        "-vf".into(),
        vf.to_string(),
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-r".into(),
        fps_str,
        "pipe:1".into(),
    ]
}
