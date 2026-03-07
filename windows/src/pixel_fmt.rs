//! Pixel format conversion helpers shared by the frame pipeline.
//!
//! These routines are intentionally pure: they do not touch COM, threads,
//! or process state. Keeping conversions isolated makes the FFmpeg reader
//! easier to follow and safer to evolve.

use crate::ffmpeg::{PREVIEW_FRAME_BYTES, PREVIEW_H, PREVIEW_W};

/// Convert yuv420p (planar: Y, U, V) to NV12 (semi-planar: Y, interleaved UV).
///
/// Windows Media Foundation virtual cameras consume NV12, while FFmpeg emits
/// yuv420p in this project.
pub fn yuv420p_to_nv12(yuv: &[u8], w: u32, h: u32) -> Vec<u8> {
    let y_size = (w * h) as usize;
    let uv_size = y_size / 2;

    let mut nv12 = vec![0u8; y_size + uv_size];

    // Y is byte-identical between yuv420p and NV12.
    nv12[..y_size].copy_from_slice(&yuv[..y_size]);

    // NV12 UV plane stores U and V interleaved per sample pair.
    let u_plane = &yuv[y_size..y_size + uv_size / 2];
    let v_plane = &yuv[y_size + uv_size / 2..];
    for i in 0..uv_size / 2 {
        nv12[y_size + i * 2] = u_plane[i];
        nv12[y_size + i * 2 + 1] = v_plane[i];
    }

    nv12
}

/// Downsample a yuv420p frame to PREVIEW_W x PREVIEW_H and convert to RGB24.
///
/// The preview path prioritizes low CPU cost over quality, so nearest-neighbor
/// sampling plus integer BT.601 math is used.
pub fn yuv420p_to_preview_rgb(yuv: &[u8], src_w: u32, src_h: u32) -> Vec<u8> {
    let mut rgb = vec![0u8; PREVIEW_FRAME_BYTES];

    let y_plane = &yuv[..(src_w * src_h) as usize];
    let u_plane = &yuv[(src_w * src_h) as usize..(src_w * src_h * 5 / 4) as usize];
    let v_plane = &yuv[(src_w * src_h * 5 / 4) as usize..];

    let scale_x = (src_w / PREVIEW_W).max(1);
    let scale_y = (src_h / PREVIEW_H).max(1);

    for py in 0..PREVIEW_H {
        let sy = py * scale_y;
        for px in 0..PREVIEW_W {
            let sx = px * scale_x;

            // Input is limited-range YUV after FFmpeg scale out_range=limited.
            let y = (((y_plane[(sy * src_w + sx) as usize] as i32) - 16) * 255 / 219).clamp(0, 255);
            let u = u_plane[((sy / 2) * (src_w / 2) + (sx / 2)) as usize] as i32 - 128;
            let v = v_plane[((sy / 2) * (src_w / 2) + (sx / 2)) as usize] as i32 - 128;

            let r = (y + 1402 * v / 1000).clamp(0, 255) as u8;
            let g = (y - 344 * u / 1000 - 714 * v / 1000).clamp(0, 255) as u8;
            let b = (y + 1772 * u / 1000).clamp(0, 255) as u8;

            let idx = ((py * PREVIEW_W + px) * 3) as usize;
            rgb[idx] = r;
            rgb[idx + 1] = g;
            rgb[idx + 2] = b;
        }
    }

    rgb
}