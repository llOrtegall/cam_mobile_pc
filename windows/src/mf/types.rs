use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use windows::core::{IUnknown, Interface, Result};
use windows::Win32::Foundation::FALSE;
use windows::Win32::Media::MediaFoundation::*;

use super::constants::{HNS_PER_SEC, OUTPUT_FPS_D, OUTPUT_FPS_N};

pub(super) struct SharedInner {
    pub(super) latest_frame: Option<Vec<u8>>,
    pub(super) pending_tokens: VecDeque<Option<IUnknown>>,
    pub(super) event_queue: Option<IMFMediaEventQueue>,
    pub(super) stream_started: bool,
    pub(super) sample_time: i64,
}

pub(super) struct StreamShared {
    pub(super) inner: Mutex<SharedInner>,
    pub(super) running: AtomicBool,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl StreamShared {
    /// Creates a `StreamShared` with the stream event queue already populated.
    ///
    /// The queue must be created *outside* any MF callback (e.g. right after
    /// `MFStartup`) to avoid re-entering mfplat while it holds an internal lock,
    /// which causes an access violation inside mfplat.dll.
    pub(super) fn new(width: u32, height: u32, stream_event_queue: IMFMediaEventQueue) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SharedInner {
                latest_frame: None,
                pending_tokens: VecDeque::new(),
                event_queue: Some(stream_event_queue),
                stream_started: false,
                sample_time: 0,
            }),
            running: AtomicBool::new(true),
            width,
            height,
        })
    }
}

pub(super) unsafe fn build_sample(
    data: &[u8],
    width: u32,
    height: u32,
    sample_time: i64,
) -> Result<IMFSample> {
    let sample: IMFSample = MFCreateSample()?;

    let buffer: IMFMediaBuffer = MFCreate2DMediaBuffer(width, height, 0x3231564e, FALSE)?;
    let buffer_2d: IMF2DBuffer2 = buffer.cast()?;
    let mut dst_scan0 = std::ptr::null_mut::<u8>();
    let mut dst_pitch: i32 = 0;
    let mut buf_start = std::ptr::null_mut::<u8>();
    let mut buf_len: u32 = 0;
    buffer_2d.Lock2DSize(
        MF2DBuffer_LockFlags_Write,
        &mut dst_scan0,
        &mut dst_pitch,
        &mut buf_start,
        &mut buf_len,
    )?;

    let y_size = (width * height) as usize;
    let uv_size = y_size / 2;

    for row in 0..height as usize {
        let src = &data[row * width as usize..(row + 1) * width as usize];
        let dst = dst_scan0.add(row * dst_pitch as usize);
        std::ptr::copy_nonoverlapping(src.as_ptr(), dst, width as usize);
    }

    let uv_src = &data[y_size..y_size + uv_size];
    let uv_dst_base = dst_scan0.add(height as usize * dst_pitch as usize);
    for row in 0..height as usize / 2 {
        let src = &uv_src[row * width as usize..(row + 1) * width as usize];
        let dst = uv_dst_base.add(row * dst_pitch as usize);
        std::ptr::copy_nonoverlapping(src.as_ptr(), dst, width as usize);
    }

    buffer_2d.Unlock2D()?;
    buffer.SetCurrentLength((y_size + uv_size) as u32)?;
    sample.AddBuffer(&buffer)?;
    sample.SetSampleTime(sample_time)?;
    sample.SetSampleDuration(HNS_PER_SEC / OUTPUT_FPS_N as i64)?;
    Ok(sample)
}

pub(super) unsafe fn build_nv12_media_type(width: u32, height: u32) -> Result<IMFMediaType> {
    let mt: IMFMediaType = MFCreateMediaType()?;
    mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
    mt.SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | (height as u64))?;
    mt.SetUINT64(&MF_MT_FRAME_RATE, ((OUTPUT_FPS_N as u64) << 32) | (OUTPUT_FPS_D as u64))?;
    mt.SetUINT64(
        &MF_MT_FRAME_RATE_RANGE_MAX,
        ((OUTPUT_FPS_N as u64) << 32) | (OUTPUT_FPS_D as u64),
    )?;
    mt.SetUINT64(
        &MF_MT_FRAME_RATE_RANGE_MIN,
        ((OUTPUT_FPS_N as u64) << 32) | (OUTPUT_FPS_D as u64),
    )?;
    mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)?;
    mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    Ok(mt)
}
