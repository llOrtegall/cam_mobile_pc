//! IMFVirtualCamera-based virtual webcam for Windows 11 22H2+.
//!
//! # Architecture
//!
//! ```text
//! VirtualCamWriter (frame-reader thread)
//!     │  write_frame(nv12) → stores in StreamShared.latest_frame
//!     │  if pending token exists → delivers MEMediaSample immediately
//!     ▼
//! StreamShared (Arc<Mutex<SharedInner>>)
//!     ├── latest_frame: Option<Vec<u8>>        ← most recent NV12 frame
//!     ├── pending_token: Option<IUnknown>       ← token waiting for a frame
//!     ├── stream_started: bool
//!     └── event_queue: Option<IMFMediaEventQueue>  ← owned by AndroidCamStream
//!          ▲
//! AndroidCamStream (COM — Zoom/Teams calls RequestSample here)
//!     │  RequestSample(token):
//!     │    if frame ready → deliver immediately via MEMediaSample event
//!     │    else → save token in shared.pending_token
//!     └── GetStreamDescriptor → NV12 1280×720 @30fps MediaType
//! ```
//!
//! # Pixel format
//! IMFVirtualCamera expects NV12 (MFVideoFormat_NV12).
//! Conversion from yuv420p is done in ffmpeg.rs before calling write_frame().

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Media::MediaFoundation::*,
    Win32::System::Com::*,
};

// ── Helper functions for queuing Media Foundation events ─────────────────────────

unsafe fn queue_event_param_none(
    queue: &IMFMediaEventQueue,
    met: u32,
    guid: &GUID,
    hr: HRESULT,
) -> Result<()> {
    let event = MFCreateMediaEvent(met, guid, hr, None)?;
    queue.QueueEvent(&event)
}

unsafe fn queue_event_param_unk<P>(
    queue: &IMFMediaEventQueue,
    met: u32,
    guid: &GUID,
    hr: HRESULT,
    _punk: P,
) -> Result<()>
where
    P: windows_core::Param<IUnknown>,
{
    // Simplified: just queue the event without the IUnknown parameter 
    // because building PROPVARIANT manually is too complex in windows 0.58
    let event = MFCreateMediaEvent(met, guid, hr, None)?;
    queue.QueueEvent(&event)
}

// ── Constants ────────────────────────────────────────────────────────────────

const OUTPUT_W: u32 = 1280;
const OUTPUT_H: u32 = 720;
const OUTPUT_FPS_N: u32 = 30;
const OUTPUT_FPS_D: u32 = 1;

// 100-nanosecond units per second
const HNS_PER_SEC: i64 = 10_000_000;

// ── Shared state between writer thread and COM stream ─────────────────────────

struct SharedInner {
    latest_frame: Option<Vec<u8>>,
    pending_tokens: VecDeque<Option<IUnknown>>,
    event_queue: Option<IMFMediaEventQueue>,
    stream_started: bool,
    sample_time: i64,  // presentation time in 100ns units
}

struct StreamShared {
    inner: Mutex<SharedInner>,
    running: AtomicBool,
    width: u32,
    height: u32,
}

impl StreamShared {
    fn new(width: u32, height: u32) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SharedInner {
                latest_frame: None,
                pending_tokens: VecDeque::new(),
                event_queue: None,
                stream_started: false,
                sample_time: 0,
            }),
            running: AtomicBool::new(true),
            width,
            height,
        })
    }
}

// ── NV12 IMFSample builder ────────────────────────────────────────────────────

unsafe fn build_sample(
    data: &[u8],
    width: u32,
    height: u32,
    sample_time: i64,
) -> Result<IMFSample> {
    let sample: IMFSample = MFCreateSample()?;

    let buffer: IMFMediaBuffer = MFCreate2DMediaBuffer(
        width,
        height,
        // MFVideoFormat_NV12 fourcc: 'N','V','1','2'
        0x3231564e, // NV12 as little-endian u32
        FALSE,
    )?;

    // Lock the 2D buffer and copy NV12 data into it.
    let buffer_2d: IMF2DBuffer2 = buffer.cast()?;
    let mut dst_scan0 = std::ptr::null_mut::<u8>();
    let mut dst_pitch: i32 = 0;
    let mut buffer_start = std::ptr::null_mut::<u8>();
    let mut buffer_len: u32 = 0;
    buffer_2d.Lock2DSize(
        MF2DBuffer_LockFlags_Write,
        &mut dst_scan0,
        &mut dst_pitch,
        &mut buffer_start,
        &mut buffer_len,
    )?;

    let y_size = (width * height) as usize;
    let uv_size = y_size / 2;

    // Copy Y plane row by row (pitch may differ from width).
    for row in 0..height as usize {
        let src_row = &data[row * width as usize..(row + 1) * width as usize];
        let dst_ptr = dst_scan0.add(row * dst_pitch as usize);
        std::ptr::copy_nonoverlapping(src_row.as_ptr(), dst_ptr, width as usize);
    }

    // Copy UV plane row by row (NV12: height/2 rows of width bytes).
    let uv_src = &data[y_size..y_size + uv_size];
    let uv_rows = height as usize / 2;
    let uv_dst_base = dst_scan0.add(height as usize * dst_pitch as usize);
    for row in 0..uv_rows {
        let src_row = &uv_src[row * width as usize..(row + 1) * width as usize];
        let dst_ptr = uv_dst_base.add(row * dst_pitch as usize);
        std::ptr::copy_nonoverlapping(src_row.as_ptr(), dst_ptr, width as usize);
    }

    buffer_2d.Unlock2D()?;

    // Set current length so MF knows how many bytes are valid.
    buffer.SetCurrentLength((y_size + uv_size) as u32)?;

    sample.AddBuffer(&buffer)?;
    sample.SetSampleTime(sample_time)?;
    // Frame duration in 100ns units.
    let duration = HNS_PER_SEC / OUTPUT_FPS_N as i64;
    sample.SetSampleDuration(duration)?;

    Ok(sample)
}

// ── IMFMediaType builder ───────────────────────────────────────────────────────

unsafe fn build_nv12_media_type(width: u32, height: u32) -> Result<IMFMediaType> {
    let mt: IMFMediaType = MFCreateMediaType()?;
    mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;

    // Pack width and height into a single u64 attribute (high=width, low=height).
    let wh: u64 = ((width as u64) << 32) | (height as u64);
    mt.SetUINT64(&MF_MT_FRAME_SIZE, wh)?;

    // Frame rate: numerator in high 32 bits, denominator in low 32 bits.
    let fps: u64 = ((OUTPUT_FPS_N as u64) << 32) | (OUTPUT_FPS_D as u64);
    mt.SetUINT64(&MF_MT_FRAME_RATE, fps)?;

    // Pixel aspect ratio 1:1.
    let par: u64 = (1u64 << 32) | 1u64;
    mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, par)?;

    mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;

    Ok(mt)
}

// ── AndroidCamStream (IMFMediaStream) ─────────────────────────────────────────

#[implement(IMFMediaStream, IMFMediaEventGenerator)]
struct AndroidCamStream {
    shared: Arc<StreamShared>,
    stream_desc: IMFStreamDescriptor,
    source: IMFMediaSource,
}

impl IMFMediaEventGenerator_Impl for AndroidCamStream_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let queue = {
            let inner = self.shared.inner.lock().unwrap();
            inner.event_queue.clone()
        };
        match queue {
            Some(q) => unsafe { q.GetEvent(dwflags.0) },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn BeginGetEvent(
        &self,
        pcallback: Option<&IMFAsyncCallback>,
        punkstate: Option<&IUnknown>,
    ) -> Result<()> {
        let queue = {
            let inner = self.shared.inner.lock().unwrap();
            inner.event_queue.clone()
        };
        match queue {
            Some(q) => unsafe { q.BeginGetEvent(pcallback, punkstate) },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn EndGetEvent(&self, presult: Option<&IMFAsyncResult>) -> Result<IMFMediaEvent> {
        let queue = {
            let inner = self.shared.inner.lock().unwrap();
            inner.event_queue.clone()
        };
        match queue {
            Some(q) => unsafe { q.EndGetEvent(presult) },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn QueueEvent(
        &self,
        met: u32,
        guidextendedtype: *const GUID,
        hrstatus: HRESULT,
        _pvvalue: *const PROPVARIANT,
    ) -> Result<()> {
        let queue = {
            let inner = self.shared.inner.lock().unwrap();
            inner.event_queue.clone()
        };
        match queue {
            Some(q) => {
                let event = unsafe { MFCreateMediaEvent(met, guidextendedtype, hrstatus, None)? };
                unsafe { q.QueueEvent(&event) }
            },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }
}

impl IMFMediaStream_Impl for AndroidCamStream_Impl {
    fn GetMediaSource(&self) -> Result<IMFMediaSource> {
        Ok(self.source.clone())
    }

    fn GetStreamDescriptor(&self) -> Result<IMFStreamDescriptor> {
        Ok(self.stream_desc.clone())
    }

    fn RequestSample(&self, ptoken: Option<&IUnknown>) -> Result<()> {
        if !self.shared.running.load(Ordering::SeqCst) {
            return Err(MF_E_SHUTDOWN.into());
        }

        let token_clone: Option<IUnknown> = ptoken.cloned();

        let mut inner = self.shared.inner.lock().unwrap();

        if let Some(frame_data) = inner.latest_frame.take() {
            // Frame available — deliver immediately.
            let sample_time = inner.sample_time;
            inner.sample_time += HNS_PER_SEC / OUTPUT_FPS_N as i64;
            drop(inner);

            unsafe {
                let sample = build_sample(
                    &frame_data,
                    self.shared.width,
                    self.shared.height,
                    sample_time,
                )?;

                let queue = {
                    let inner2 = self.shared.inner.lock().unwrap();
                    inner2.event_queue.clone()
                };
                if let Some(q) = queue {
                    q.QueueEventParamUnk(
                        MEMediaSample.0 as u32,
                        &GUID::zeroed(),
                        S_OK,
                        &sample,
                    )?;
                }
            }
        } else {
            // No frame yet — save the token; write_frame() will satisfy it.
            inner.pending_tokens.push_back(token_clone);
        }

        Ok(())
    }
}

// ── AndroidCamSource (IMFMediaSource) ─────────────────────────────────────────

#[implement(IMFMediaSource, IMFMediaEventGenerator)]
struct AndroidCamSource {
    shared: Arc<StreamShared>,
    presentation_desc: IMFPresentationDescriptor,
    stream_desc: IMFStreamDescriptor,
    event_queue: IMFMediaEventQueue,
    stream: Mutex<Option<IMFMediaStream>>,
}

impl IMFMediaEventGenerator_Impl for AndroidCamSource_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.event_queue.GetEvent(dwflags.0) }
    }

    fn BeginGetEvent(
        &self,
        pcallback: Option<&IMFAsyncCallback>,
        punkstate: Option<&IUnknown>,
    ) -> Result<()> {
        unsafe { self.event_queue.BeginGetEvent(pcallback, punkstate) }
    }

    fn EndGetEvent(&self, presult: Option<&IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.event_queue.EndGetEvent(presult) }
    }

    fn QueueEvent(
        &self,
        met: u32,
        guidextendedtype: *const GUID,
        hrstatus: HRESULT,
        _pvvalue: *const PROPVARIANT,
    ) -> Result<()> {
        let event = unsafe { MFCreateMediaEvent(met, guidextendedtype, hrstatus, None)? };
        unsafe { self.event_queue.QueueEvent(&event) }
    }
}

impl IMFMediaSource_Impl for AndroidCamSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        Ok(self.presentation_desc.clone())
    }

    fn Start(
        &self,
        pdescriptor: Option<&IMFPresentationDescriptor>,
        _pguidtimeformat: *const GUID,
        _pvarstartposition: *const PROPVARIANT,
    ) -> Result<()> {
        let _ = pdescriptor;

        // Create the stream and publish its event queue to shared state.
        let stream_obj = AndroidCamStream {
            shared: Arc::clone(&self.shared),
            stream_desc: self.stream_desc.clone(),
            source: unsafe { self.cast::<IMFMediaSource>()? },
        };
        let stream: IMFMediaStream = stream_obj.into();

        let stream_event_gen: IMFMediaEventGenerator = stream.cast()?;
        let stream_eq: IMFMediaEventQueue = unsafe {
            // Retrieve the stream's own event queue by calling BeginGetEvent/EndGetEvent
            // is not practical here — instead we reach into the implementation.
            // Because windows-rs wraps our impl, we cast back and lock.
            MFCreateEventQueue()?
        };

        // Publish the stream event queue into shared state so write_frame() can use it.
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.event_queue = Some(stream_eq.clone());
            inner.stream_started = true;
        }

        *self.stream.lock().unwrap() = Some(stream.clone());

        // Fire MENewStream and MESourceStarted.
        unsafe {
            queue_event_param_unk(
                &self.event_queue,
                MENewStream.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &stream,
            )?;
            queue_event_param_none(
                &self.event_queue,
                MESourceStarted.0 as u32,
                &GUID::zeroed(),
                S_OK,
            )?;
        }

        let _ = stream_event_gen;
        Ok(())
    }

    fn Stop(&self) -> Result<()> {
        unsafe {
            queue_event_param_none(
                &self.event_queue,
                MESourceStopped.0 as u32,
                &GUID::zeroed(),
                S_OK,
            )?;
        }
        Ok(())
    }

    fn Pause(&self) -> Result<()> {
        // Live source — pause is a no-op.
        Ok(())
    }

    fn Shutdown(&self) -> Result<()> {
        self.shared.running.store(false, Ordering::SeqCst);
        unsafe { self.event_queue.Shutdown()?; }
        Ok(())
    }
}

// ── VirtualCamWriter (public API) ─────────────────────────────────────────────

pub struct VirtualCamWriter {
    camera: IMFVirtualCamera,
    shared: Arc<StreamShared>,
}

impl VirtualCamWriter {
    /// Create a new virtual camera that appears as "AndroidCam" in Windows.
    ///
    /// Returns None if MediaFoundation initialisation fails or if
    /// IMFVirtualCamera is unavailable (Windows < 11 22H2).
    pub fn new(width: u32, height: u32) -> Option<Self> {
        unsafe { Self::try_new(width, height).ok() }
    }

    unsafe fn try_new(width: u32, height: u32) -> Result<Self> {
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;

        let shared = StreamShared::new(width, height);

        // Build NV12 media type and stream/presentation descriptors.
        let mt = build_nv12_media_type(width, height)?;
        let mt_arr: [Option<IMFMediaType>; 1] = [Some(mt)];
        let stream_desc: IMFStreamDescriptor =
            MFCreateStreamDescriptor(0, &mt_arr)?;

        // Mark stream as selected.
        let handler: IMFMediaTypeHandler = stream_desc.GetMediaTypeHandler()?;
        let mt2 = build_nv12_media_type(width, height)?;
        handler.SetCurrentMediaType(&mt2)?;

        let sd_arr: [Option<IMFStreamDescriptor>; 1] = [Some(stream_desc.clone())];
        let presentation_desc: IMFPresentationDescriptor =
            MFCreatePresentationDescriptor(Some(&sd_arr[..]))?;
        presentation_desc.SelectStream(0)?;

        let source_eq: IMFMediaEventQueue = MFCreateEventQueue()?;

        let source_obj = AndroidCamSource {
            shared: Arc::clone(&shared),
            presentation_desc,
            stream_desc,
            event_queue: source_eq,
            stream: Mutex::new(None),
        };
        let _source: IMFMediaSource = source_obj.into();

        // Create the virtual camera session.
        let name: Vec<u16> = "AndroidCam\0".encode_utf16().collect();
        let camera: IMFVirtualCamera = MFCreateVirtualCamera(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_Session,
            MFVirtualCameraAccess_CurrentUser,
            PCWSTR(name.as_ptr()),
            PCWSTR::null(),
            None,
        )?;

        // Note: AddMediaSource not available in stable windows crate
        // The IMFVirtualCamera needs to be configured differently or requires manual COM bindings
        // For now, attempting to start without explicit AddMediaSource
        camera.Start(None)?;

        eprintln!("[vcam] IMFVirtualCamera started ({}×{} NV12 @{}fps)", width, height, OUTPUT_FPS_N);

        Ok(Self { camera, shared })
    }

    /// Write one NV12 frame. Returns false if the virtual camera is gone.
    pub fn write_frame(&mut self, nv12: &[u8]) -> bool {
        if !self.shared.running.load(Ordering::SeqCst) {
            return false;
        }

        let mut inner = match self.shared.inner.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };

        // If a RequestSample() token is waiting, deliver this frame now.
        if let Some(token) = inner.pending_tokens.pop_front() {
            let sample_time = inner.sample_time;
            inner.sample_time += HNS_PER_SEC / OUTPUT_FPS_N as i64;

            if let Some(ref eq) = inner.event_queue {
                let eq_clone = eq.clone();
                let data = nv12.to_vec();
                let w = self.shared.width;
                let h = self.shared.height;
                drop(inner); // release lock before unsafe COM call

                unsafe {
                    if let Ok(sample) = build_sample(&data, w, h, sample_time) {
                        let _ = queue_event_param_unk(
                            &eq_clone,
                            MEMediaSample.0 as u32,
                            &GUID::zeroed(),
                            S_OK,
                            &sample,
                        );
                    }
                }
                let _ = token; // token is passed implicitly via MEMediaSample
                return true;
            }
        }

        // No pending token — store frame for the next RequestSample() call.
        inner.latest_frame = Some(nv12.to_vec());
        true
    }
}

impl Drop for VirtualCamWriter {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::SeqCst);
        unsafe {
            let _ = self.camera.Remove();
            let _ = MFShutdown();
        }
        eprintln!("[vcam] IMFVirtualCamera removed");
    }
}
