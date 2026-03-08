//! FFmpeg process supervision and frame reader pipeline.
//!
//! Public API remains small for engine integration: spawn/kill by process or PID,
//! plus preview/output constants consumed by UI and pixel conversion helpers.

mod filter;
mod process;
mod reader;

pub use process::{kill, kill_pid, spawn_ffmpeg};

// Preview stream dimensions shown in the GUI canvas (RGB24)
pub const PREVIEW_W: u32 = 640;
pub const PREVIEW_H: u32 = 360;
pub const PREVIEW_FRAME_BYTES: usize = (PREVIEW_W * PREVIEW_H * 3) as usize;

// Fixed output resolution: 720p for lower latency and bandwidth.
pub const OUTPUT_W: u32 = 1280;
pub const OUTPUT_H: u32 = 720;
