//! IMFVirtualCamera-based virtual webcam for Windows 11 22H2+.
//!
//! # Why dynamic loading?
//!
//! windows-rs 0.58 ships an incorrect binding for `IMFVirtualCamera`:
//! it links from `mfsensorgroup.dll` (driver-level sensor-group API, IID
//! 1c08a864-...) instead of the user-mode virtual camera API in `Mf.dll`
//! (IID a925bb0d-04e4-4bdf-9fc5-b5a0efb0ca3c).  Calling `MFCreateVirtualCamera`
//! via the wrong import gives E_INVALIDARG immediately.
//!
//! Fix: we load `mfsensorgroup.dll` at runtime with `GetProcAddress` (the correct
//! DLL per MSDN) and call `MFCreateVirtualCamera` directly.  All subsequent
//! operations on the returned COM object use raw vtable dispatch with the correct
//! IID `a925bb0d-...` so we are not affected by windows-rs 0.58's wrong interface.
//!
//! # User-mode IMFVirtualCamera vtable (IID a925bb0d-..., inherits IUnknown)
//!
//! ```text
//!   0  QueryInterface
//!   1  AddRef
//!   2  Release
//!   3  AddMediaSource(IUnknown*, IMFAttributes*) -> HRESULT
//!   4  Start(IMFAttributes*)                     -> HRESULT
//!   5  Stop()                                    -> HRESULT
//!   6  Remove()                                  -> HRESULT
//! ```
//!
//! # Frame delivery flow
//!
//! ```text
//! VirtualCamWriter::write_frame(nv12)
//!     └─ if pending token → queue MEMediaSample on stream event queue
//!     └─ else             → store in latest_frame
//!
//! AndroidCamStream::RequestSample(token)
//!     └─ if frame ready  → queue MEMediaSample immediately
//!     └─ else            → push token into pending_tokens
//! ```

use log::{error, info};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use windows::{
    core::*,
    Win32::Foundation::*,
    Win32::Media::MediaFoundation::*,
    Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW},
};

// ── Raw user-mode IMFVirtualCamera handle ─────────────────────────────────────
//
// Wraps the raw COM pointer returned by Mf.dll's MFCreateVirtualCamera.
// We bypass windows-rs type system entirely here because it has the wrong
// interface definition for IMFVirtualCamera in 0.58.

struct VirtualCamHandle(*mut std::ffi::c_void);

// The frame-reader thread creates VirtualCamWriter which holds VirtualCamHandle.
unsafe impl Send for VirtualCamHandle {}

impl VirtualCamHandle {
    #[inline]
    unsafe fn vtable(&self) -> *const *const () {
        *(self.0 as *const *const *const ())
    }

    // Slot 3: AddMediaSource(this, pSource: IUnknown*, pAttributes: IMFAttributes*)
    unsafe fn add_media_source(&self, source_unk: *mut std::ffi::c_void) -> HRESULT {
        let f: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> HRESULT = std::mem::transmute(*self.vtable().add(3));
        f(self.0, source_unk, std::ptr::null_mut())
    }

    // Slot 4: Start(this, pAttributes: IMFAttributes*)
    unsafe fn start(&self) -> HRESULT {
        let f: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> HRESULT = std::mem::transmute(*self.vtable().add(4));
        f(self.0, std::ptr::null_mut())
    }

    // Slot 6: Remove(this)
    unsafe fn remove(&self) -> HRESULT {
        let f: unsafe extern "system" fn(*mut std::ffi::c_void) -> HRESULT =
            std::mem::transmute(*self.vtable().add(6));
        f(self.0)
    }

    // Slot 2: Release(this)
    unsafe fn release_com(&self) -> u32 {
        let f: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32 =
            std::mem::transmute(*self.vtable().add(2));
        f(self.0)
    }
}

impl Drop for VirtualCamHandle {
    fn drop(&mut self) {
        if self.0.is_null() { return; }
        unsafe {
            let _ = self.remove();
            self.release_com();
        }
    }
}

// ── Dynamic load of MFCreateVirtualCamera from mfsensorgroup.dll ─────────────

type MFCreateVirtualCameraFn = unsafe extern "system" fn(
    r#type:         i32,            // MFVirtualCameraType  (0 = SoftwareCameraSource)
    lifetime:       i32,            // MFVirtualCameraLifetime (0 = Session)
    access:         i32,            // MFVirtualCameraAccess   (0 = CurrentUser)
    friendly_name:  *const u16,     // LPCWSTR
    source_id:      *const u16,     // LPCWSTR (optional, null = auto-generate)
    categories:     *const GUID,    // optional, null OK
    category_count: u32,
    camera:         *mut *mut std::ffi::c_void,  // out: IMFVirtualCamera**
) -> HRESULT;

unsafe fn load_mf_create_virtual_camera() -> Result<MFCreateVirtualCameraFn> {
    let hmod = LoadLibraryW(w!("mfsensorgroup.dll"))
        .map_err(|e| { error!("[vcam] LoadLibraryW(mfsensorgroup.dll) failed: {e}"); e })?;

    let proc = GetProcAddress(hmod, PCSTR(b"MFCreateVirtualCamera\0".as_ptr()))
        .ok_or_else(|| {
            error!("[vcam] GetProcAddress(MFCreateVirtualCamera) failed");
            Error::from(E_NOTIMPL)
        })?;

    Ok(std::mem::transmute(proc))
}

// ── Constants ─────────────────────────────────────────────────────────────────

const OUTPUT_FPS_N: u32 = 30;
const OUTPUT_FPS_D: u32 = 1;
const HNS_PER_SEC:  i64 = 10_000_000;

// Unique source ID for AndroidCamSource. MFCreateVirtualCamera requires a
// non-null GUID string to identify the virtual camera source; this value is
// fixed so the same camera is re-opened across calls within a session.
const ANDROID_CAM_SOURCE_ID: &str = "{5B9A4C2D-8E1F-4A3B-9C7D-0F2E1A5B6C4D}\0";

// MF_DEVICESTREAM_STREAM_ID  {11CA3D03-4A3B-4CF3-8938-8A8E0F0F0A56}
// Must be set on each stream descriptor; AddMediaSource validates it via
// IMFMediaSourceEx::GetStreamAttributes().
const MF_DEVICESTREAM_STREAM_ID_ATTR: GUID = GUID {
    data1: 0x11CA3D03, data2: 0x4A3B, data3: 0x4CF3,
    data4: [0x89, 0x38, 0x8A, 0x8E, 0x0F, 0x0F, 0x0A, 0x56],
};

// MF_DEVICESTREAM_STREAM_CATEGORY  {149C20AC-2B6C-4C2B-8B37-3E47B88DA38B}
// Value must be PINNAME_VIDEO_CAPTURE to identify this as a video stream.
const MF_DEVICESTREAM_STREAM_CATEGORY_ATTR: GUID = GUID {
    data1: 0x149C20AC, data2: 0x2B6C, data3: 0x4C2B,
    data4: [0x8B, 0x37, 0x3E, 0x47, 0xB8, 0x8D, 0xA3, 0x8B],
};

// PINNAME_VIDEO_CAPTURE  {FB6C4281-0353-11D1-905F-0000C0CC16BA}
const PINNAME_VIDEO_CAPTURE: GUID = GUID {
    data1: 0xFB6C4281, data2: 0x0353, data3: 0x11D1,
    data4: [0x90, 0x5F, 0x00, 0x00, 0xC0, 0xCC, 0x16, 0xBA],
};

// ── Shared state between writer thread and COM stream ─────────────────────────

struct SharedInner {
    latest_frame:   Option<Vec<u8>>,
    pending_tokens: VecDeque<Option<IUnknown>>,
    event_queue:    Option<IMFMediaEventQueue>,
    stream_started: bool,
    sample_time:    i64,
}

struct StreamShared {
    inner:   Mutex<SharedInner>,
    running: AtomicBool,
    width:   u32,
    height:  u32,
}

impl StreamShared {
    fn new(width: u32, height: u32) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(SharedInner {
                latest_frame:   None,
                pending_tokens: VecDeque::new(),
                event_queue:    None,
                stream_started: false,
                sample_time:    0,
            }),
            running: AtomicBool::new(true),
            width,
            height,
        })
    }
}

// ── NV12 IMFSample builder ────────────────────────────────────────────────────

unsafe fn build_sample(data: &[u8], width: u32, height: u32, sample_time: i64) -> Result<IMFSample> {
    let sample: IMFSample = MFCreateSample()?;

    let buffer: IMFMediaBuffer = MFCreate2DMediaBuffer(
        width, height,
        0x3231564e, // NV12 fourcc LE
        FALSE,
    )?;

    let buffer_2d: IMF2DBuffer2 = buffer.cast()?;
    let mut dst_scan0 = std::ptr::null_mut::<u8>();
    let mut dst_pitch: i32 = 0;
    let mut buf_start = std::ptr::null_mut::<u8>();
    let mut buf_len: u32 = 0;
    buffer_2d.Lock2DSize(
        MF2DBuffer_LockFlags_Write,
        &mut dst_scan0, &mut dst_pitch,
        &mut buf_start, &mut buf_len,
    )?;

    let y_size  = (width * height) as usize;
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

// ── IMFMediaType builder ───────────────────────────────────────────────────────

unsafe fn build_nv12_media_type(width: u32, height: u32) -> Result<IMFMediaType> {
    let mt: IMFMediaType = MFCreateMediaType()?;
    mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    mt.SetGUID(&MF_MT_SUBTYPE,    &MFVideoFormat_NV12)?;
    mt.SetUINT64(&MF_MT_FRAME_SIZE,        ((width as u64) << 32) | (height as u64))?;
    mt.SetUINT64(&MF_MT_FRAME_RATE,        ((OUTPUT_FPS_N as u64) << 32) | (OUTPUT_FPS_D as u64))?;
    mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)?;
    mt.SetUINT32(&MF_MT_INTERLACE_MODE,    MFVideoInterlace_Progressive.0 as u32)?;
    mt.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    Ok(mt)
}

// ── AndroidCamStream (IMFMediaStream) ─────────────────────────────────────────

#[implement(IMFMediaStream, IMFMediaEventGenerator)]
struct AndroidCamStream {
    shared:      Arc<StreamShared>,
    stream_desc: IMFStreamDescriptor,
    source:      IMFMediaSource,
}

impl IMFMediaEventGenerator_Impl for AndroidCamStream_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.shared.inner.lock().unwrap().event_queue.clone();
        match q {
            Some(q) => unsafe { q.GetEvent(dwflags.0) },
            None    => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn BeginGetEvent(&self, pcallback: Option<&IMFAsyncCallback>, punkstate: Option<&IUnknown>) -> Result<()> {
        let q = self.shared.inner.lock().unwrap().event_queue.clone();
        match q {
            Some(q) => unsafe { q.BeginGetEvent(pcallback, punkstate) },
            None    => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn EndGetEvent(&self, presult: Option<&IMFAsyncResult>) -> Result<IMFMediaEvent> {
        let q = self.shared.inner.lock().unwrap().event_queue.clone();
        match q {
            Some(q) => unsafe { q.EndGetEvent(presult) },
            None    => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn QueueEvent(&self, met: u32, guidextendedtype: *const GUID, hrstatus: HRESULT, pvvalue: *const PROPVARIANT) -> Result<()> {
        let q = self.shared.inner.lock().unwrap().event_queue.clone();
        let pv = if pvvalue.is_null() { None } else { Some(pvvalue) };
        match q {
            Some(q) => unsafe {
                let ev = MFCreateMediaEvent(met, guidextendedtype, hrstatus, pv)?;
                q.QueueEvent(&ev)
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
            let sample_time = inner.sample_time;
            inner.sample_time += HNS_PER_SEC / OUTPUT_FPS_N as i64;
            let eq = inner.event_queue.clone();
            drop(inner);

            if let Some(q) = eq {
                unsafe {
                    let sample = build_sample(&frame_data, self.shared.width, self.shared.height, sample_time)?;
                    q.QueueEventParamUnk(MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample)?;
                }
            }
        } else {
            inner.pending_tokens.push_back(token_clone);
        }
        Ok(())
    }
}

// ── AndroidCamSource (IMFMediaSource) ─────────────────────────────────────────

#[implement(IMFMediaSourceEx, IMFMediaEventGenerator)]
struct AndroidCamSource {
    shared:            Arc<StreamShared>,
    presentation_desc: IMFPresentationDescriptor,
    stream_desc:       IMFStreamDescriptor,
    event_queue:       IMFMediaEventQueue,
    stream:            Mutex<Option<IMFMediaStream>>,
}

impl IMFMediaEventGenerator_Impl for AndroidCamSource_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        unsafe { self.event_queue.GetEvent(dwflags.0) }
    }

    fn BeginGetEvent(&self, pcallback: Option<&IMFAsyncCallback>, punkstate: Option<&IUnknown>) -> Result<()> {
        unsafe { self.event_queue.BeginGetEvent(pcallback, punkstate) }
    }

    fn EndGetEvent(&self, presult: Option<&IMFAsyncResult>) -> Result<IMFMediaEvent> {
        unsafe { self.event_queue.EndGetEvent(presult) }
    }

    fn QueueEvent(&self, met: u32, guidextendedtype: *const GUID, hrstatus: HRESULT, pvvalue: *const PROPVARIANT) -> Result<()> {
        let pv = if pvvalue.is_null() { None } else { Some(pvvalue) };
        unsafe {
            let ev = MFCreateMediaEvent(met, guidextendedtype, hrstatus, pv)?;
            self.event_queue.QueueEvent(&ev)
        }
    }
}

impl IMFMediaSourceEx_Impl for AndroidCamSource_Impl {
    fn GetStreamAttributes(&self, dwstreamindex: u32) -> Result<IMFAttributes> {
        if dwstreamindex == 0 {
            self.stream_desc.cast()
        } else {
            Err(E_INVALIDARG.into())
        }
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
        _pdescriptor:       Option<&IMFPresentationDescriptor>,
        _pguidtimeformat:   *const GUID,
        _pvarstartposition: *const PROPVARIANT,
    ) -> Result<()> {
        let source_intf: IMFMediaSource = unsafe { self.cast()? };
        let stream_obj = AndroidCamStream {
            shared:      Arc::clone(&self.shared),
            stream_desc: self.stream_desc.clone(),
            source:      source_intf,
        };
        let stream: IMFMediaStream = stream_obj.into();

        // Dedicated event queue for the stream — BeginGetEvent/RequestSample
        // both operate on this queue.
        let stream_eq: IMFMediaEventQueue = unsafe { MFCreateEventQueue()? };
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.event_queue    = Some(stream_eq);
            inner.stream_started = true;
        }
        *self.stream.lock().unwrap() = Some(stream.clone());

        // MENewStream must carry the stream IUnknown so the pipeline can call
        // BeginGetEvent / RequestSample on it.
        let stream_unk: IUnknown = stream.cast()?;
        unsafe {
            self.event_queue.QueueEventParamUnk(
                MENewStream.0 as u32, &GUID::zeroed(), S_OK, &stream_unk,
            )?;
            let ev = MFCreateMediaEvent(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, None)?;
            self.event_queue.QueueEvent(&ev)?;
        }
        Ok(())
    }

    fn Stop(&self) -> Result<()> {
        unsafe {
            let ev = MFCreateMediaEvent(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, None)?;
            self.event_queue.QueueEvent(&ev)?;
        }
        Ok(())
    }

    fn Pause(&self) -> Result<()> { Ok(()) }

    fn Shutdown(&self) -> Result<()> {
        self.shared.running.store(false, Ordering::SeqCst);
        unsafe { self.event_queue.Shutdown()?; }
        Ok(())
    }
}

// ── VirtualCamWriter (public API) ─────────────────────────────────────────────

pub struct VirtualCamWriter {
    /// Held for Drop — calls Remove() + Release() via VirtualCamHandle::drop().
    _camera: VirtualCamHandle,
    /// Keep the COM source alive for the lifetime of the virtual camera.
    _source: IMFMediaSourceEx,
    shared:  Arc<StreamShared>,
}

impl VirtualCamWriter {
    pub fn new(width: u32, height: u32) -> Option<Self> {
        unsafe {
            match Self::try_new(width, height) {
                Ok(v)  => Some(v),
                Err(e) => {
                    error!("[vcam] FAILED: {:#010x} — {e}", e.code().0 as u32);
                    None
                }
            }
        }
    }

    unsafe fn try_new(width: u32, height: u32) -> Result<Self> {
        info!("[vcam] MFStartup...");
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;

        info!("[vcam] Building MF objects...");
        let shared = StreamShared::new(width, height);

        let mt  = build_nv12_media_type(width, height)?;
        let mts: [Option<IMFMediaType>; 1] = [Some(mt)];
        let stream_desc: IMFStreamDescriptor = MFCreateStreamDescriptor(0, &mts)?;

        // Required by IMFVirtualCamera::AddMediaSource — the frame server reads
        // these via IMFMediaSourceEx::GetStreamAttributes(stream_index).
        stream_desc.SetUINT32(&MF_DEVICESTREAM_STREAM_ID_ATTR, 0)?;
        stream_desc.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY_ATTR, &PINNAME_VIDEO_CAPTURE)?;

        let handler: IMFMediaTypeHandler = stream_desc.GetMediaTypeHandler()?;
        handler.SetCurrentMediaType(&build_nv12_media_type(width, height)?)?;

        let sds: [Option<IMFStreamDescriptor>; 1] = [Some(stream_desc.clone())];
        let presentation_desc: IMFPresentationDescriptor =
            MFCreatePresentationDescriptor(Some(&sds[..]))?;
        presentation_desc.SelectStream(0)?;

        let source_eq: IMFMediaEventQueue = MFCreateEventQueue()?;
        let source_obj = AndroidCamSource {
            shared: Arc::clone(&shared),
            presentation_desc,
            stream_desc,
            event_queue: source_eq,
            stream: Mutex::new(None),
        };
        let source: IMFMediaSourceEx = source_obj.into();

        // Load MFCreateVirtualCamera from the correct DLL (Mf.dll, not mfsensorgroup.dll).
        info!("[vcam] Loading MFCreateVirtualCamera from mfsensorgroup.dll...");
        let create_fn = load_mf_create_virtual_camera()?;

        let name:      Vec<u16> = "AndroidCam\0".encode_utf16().collect();
        let source_id: Vec<u16> = ANDROID_CAM_SOURCE_ID.encode_utf16().collect();
        let mut cam_ptr: *mut std::ffi::c_void = std::ptr::null_mut();

        info!("[vcam] Calling MFCreateVirtualCamera...");
        let hr = create_fn(
            0, // MFVirtualCameraType_SoftwareCameraSource
            0, // MFVirtualCameraLifetime_Session
            0, // MFVirtualCameraAccess_CurrentUser
            name.as_ptr(),
            source_id.as_ptr(), // sourceId — required, must not be null
            std::ptr::null(),   // categories — null → default (VIDEO_CAMERA, VIDEO, CAPTURE)
            0,
            &mut cam_ptr,
        );
        if hr == HRESULT(0x80070057u32 as i32) {
            error!("[vcam] MFCreateVirtualCamera failed: E_INVALIDARG (0x80070057)");
            error!("[vcam] HINT: Enable Developer Mode in Windows Settings → Privacy & Security → For developers → Developer Mode");
            return Err(hr.into());
        }
        hr.ok().map_err(|e| { error!("[vcam] MFCreateVirtualCamera failed: {e}"); e })?;

        let camera = VirtualCamHandle(cam_ptr);

        info!("[vcam] AddMediaSource...");
        camera.add_media_source(source.as_raw() as *mut _)
            .ok().map_err(|e| { error!("[vcam] AddMediaSource failed: {e}"); e })?;

        info!("[vcam] Start...");
        camera.start()
            .ok().map_err(|e| { error!("[vcam] Start failed: {e}"); e })?;

        info!("[vcam] IMFVirtualCamera ready ({}×{} NV12 @{}fps)", width, height, OUTPUT_FPS_N);
        Ok(Self { _camera: camera, _source: source, shared })
    }

    /// Write one NV12 frame. Returns false if the virtual camera is gone.
    pub fn write_frame(&mut self, nv12: &[u8]) -> bool {
        if !self.shared.running.load(Ordering::SeqCst) {
            return false;
        }
        let mut inner = match self.shared.inner.lock() {
            Ok(g)  => g,
            Err(_) => return false,
        };

        if let Some(_token) = inner.pending_tokens.pop_front() {
            let sample_time = inner.sample_time;
            inner.sample_time += HNS_PER_SEC / OUTPUT_FPS_N as i64;

            if let Some(ref eq) = inner.event_queue {
                let eq_clone = eq.clone();
                let data = nv12.to_vec();
                let (w, h) = (self.shared.width, self.shared.height);
                drop(inner);

                unsafe {
                    if let Ok(sample) = build_sample(&data, w, h, sample_time) {
                        let _ = eq_clone.QueueEventParamUnk(
                            MEMediaSample.0 as u32, &GUID::zeroed(), S_OK, &sample,
                        );
                    }
                }
                return true;
            }
        }

        inner.latest_frame = Some(nv12.to_vec());
        true
    }
}

impl Drop for VirtualCamWriter {
    fn drop(&mut self) {
        self.shared.running.store(false, Ordering::SeqCst);
        // camera.drop() calls Remove() + Release() via VirtualCamHandle::drop()
        unsafe { let _ = MFShutdown(); }
        info!("[vcam] VirtualCamWriter dropped");
    }
}
