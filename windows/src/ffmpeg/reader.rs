use std::io::{BufReader, Read};
use std::process::ChildStdout;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::thread;

use log::warn;

use crate::mf::{ComInitGuard, VirtualCamWriter};
use crate::pixel_fmt::{yuv420p_to_nv12, yuv420p_to_preview_rgb};

/// Reads yuv420p frames from FFmpeg stdout, forwards them to the virtual camera,
/// and publishes throttled preview frames to the GUI channel.
pub(super) fn spawn_frame_reader(
    stdout: ChildStdout,
    preview_tx: SyncSender<Vec<u8>>,
    out_w: u32,
    out_h: u32,
    stream_fps: u32,
    preview_fps: u32,
) {
    thread::spawn(move || {
        // IMFVirtualCamera calls must run on a COM-initialized thread.
        let _com_guard = ComInitGuard::new();

        let frame_bytes = (out_w * out_h * 3 / 2) as usize;
        let mut reader = BufReader::with_capacity(frame_bytes, stdout);
        let mut yuv_buf = vec![0u8; frame_bytes];

        let mut vcam = VirtualCamWriter::new(out_w, out_h);
        if vcam.is_none() {
            warn!("[ffmpeg] VirtualCamera unavailable - preview only");
        }

        let preview_every = if preview_fps >= stream_fps {
            1usize
        } else {
            (stream_fps / preview_fps.max(1)).max(1) as usize
        };
        let mut frame_count: usize = 0;

        while let Ok(()) = reader.read_exact(&mut yuv_buf) {
            let nv12 = yuv420p_to_nv12(&yuv_buf, out_w, out_h);

            if let Some(ref mut writer) = vcam {
                if !writer.write_frame(&nv12) {
                    warn!("[ffmpeg] VirtualCamera write error - retrying open");
                    vcam = VirtualCamWriter::new(out_w, out_h);
                }
            }

            frame_count += 1;
            if frame_count % preview_every == 0 {
                let preview = yuv420p_to_preview_rgb(&yuv_buf, out_w, out_h);
                match preview_tx.try_send(preview) {
                    Ok(()) | Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
        }
    });
}
