//! MediaFoundation virtual camera wrapper.
//!
//! Keeps COM-heavy implementation behind a small API so the frame reader only
//! depends on `VirtualCamWriter::new` and `write_frame`.

mod activate;
mod camera;
mod constants;
mod factory;
mod source;
mod stream;
mod types;

use log::{error, info};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use windows::Win32::System::Com::{
    CoRegisterClassObject, CoRevokeClassObject, IClassFactory, CLSCTX_LOCAL_SERVER,
    REGCLS_MULTIPLEUSE,
};
use windows::core::*;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::MediaFoundation::*;

use self::camera::{load_mf_create_virtual_camera, VirtualCamHandle};
use self::constants::{
    KSCATEGORY_VIDEO_CAMERA,
    ANDROID_CAM_SOURCE_CLSID,
    ANDROID_CAM_FRIENDLY_NAME,
    ANDROID_CAM_SOURCE_ID,
    DEVPKEY_DEVICEINTERFACE_VCAMCREATE_ACCESS,
    DEVPKEY_DEVICEINTERFACE_VCAMCREATE_FRIENDLYNAME,
    DEVPKEY_DEVICEINTERFACE_VCAMCREATE_LIFETIME,
    DEVPKEY_DEVICEINTERFACE_VCAMCREATE_SOURCEID,
    DEVPROP_TYPE_INT32,
    DEVPROP_TYPE_STRING,
    MF_DEVICESTREAM_FRAMESOURCE_TYPES_ATTR,
    MF_DEVICESTREAM_FRAMESERVER_SHARED_ATTR,
    MF_DEVICESTREAM_STREAM_CATEGORY_ATTR,
    MF_DEVICESTREAM_STREAM_ID_ATTR,
    MF_FRAMESOURCE_TYPES_COLOR,
    OUTPUT_FPS_N,
    PINNAME_VIDEO_CAPTURE,
};
use self::source::{build_source_attributes, AndroidCamSource};
use self::types::{build_nv12_media_type, build_sample, StreamShared};

pub(crate) struct ComInitGuard;

impl ComInitGuard {
    pub(crate) fn new() -> Self {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComInitGuard
    }
}

impl Drop for ComInitGuard {
    fn drop(&mut self) {
        use windows::Win32::System::Com::CoUninitialize;
        unsafe { CoUninitialize() };
    }
}

pub struct VirtualCamWriter {
    _camera: VirtualCamHandle,
    _source: IMFMediaSourceEx,
    /// IClassFactory kept alive so Frame Server can call CoCreateInstance at any time.
    _factory: Option<IClassFactory>,
    /// Cookie from CoRegisterClassObject; 0 if classic interface was used.
    reg_cookie: u32,
    shared: Arc<StreamShared>,
}

impl VirtualCamWriter {
    pub fn new(width: u32, height: u32) -> Option<Self> {
        unsafe {
            match Self::try_new(width, height) {
                Ok(v) => Some(v),
                Err(e) => {
                    error!("[vcam] FAILED: {:#010x} - {e}", e.code().0 as u32);
                    None
                }
            }
        }
    }

    unsafe fn try_new(width: u32, height: u32) -> Result<Self> {
        info!("[vcam] step 1: MFStartup");
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)?;

        info!("[vcam] step 2: MFCreateEventQueue (stream)");
        // Pre-create the stream event queue here, on this thread, while no MF
        // callback is in progress.  Creating it inside Source::Start() would
        // re-enter mfplat while IMFVirtualCamera::Start() already holds an
        // internal lock, causing an access violation inside mfplat.dll.
        let stream_eq: IMFMediaEventQueue = MFCreateEventQueue()?;
        let shared = StreamShared::new(width, height, stream_eq);

        info!("[vcam] step 3: build_nv12_media_type");
        let mt = build_nv12_media_type(width, height)?;
        let mts: [Option<IMFMediaType>; 1] = [Some(mt)];

        info!("[vcam] step 4: MFCreateStreamDescriptor");
        let stream_desc: IMFStreamDescriptor = MFCreateStreamDescriptor(0, &mts)?;

        info!("[vcam] step 5: set stream attributes");
        stream_desc.SetUINT32(&MF_DEVICESTREAM_STREAM_ID_ATTR, 0)?;
        stream_desc.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY_ATTR, &PINNAME_VIDEO_CAPTURE)?;
        stream_desc.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED_ATTR, 1)?;
        stream_desc.SetUINT32(&MF_DEVICESTREAM_FRAMESOURCE_TYPES_ATTR, MF_FRAMESOURCE_TYPES_COLOR)?;

        info!("[vcam] step 6: SetCurrentMediaType");
        let handler: IMFMediaTypeHandler = stream_desc.GetMediaTypeHandler()?;
        handler.SetCurrentMediaType(&build_nv12_media_type(width, height)?)?;

        info!("[vcam] step 7: MFCreatePresentationDescriptor");
        let sds: [Option<IMFStreamDescriptor>; 1] = [Some(stream_desc.clone())];
        let presentation_desc: IMFPresentationDescriptor =
            MFCreatePresentationDescriptor(Some(&sds[..]))?;
        presentation_desc.SelectStream(0)?;

        info!("[vcam] step 8: build AndroidCamSource");
        let source_eq: IMFMediaEventQueue = MFCreateEventQueue()?;
        let source_attrs = build_source_attributes(None)?;
        // Clone descriptors so the factory can also use them (classic path moves them).
        let source_obj = AndroidCamSource {
            shared: Arc::clone(&shared),
            presentation_desc: presentation_desc.clone(),
            stream_desc: stream_desc.clone(),
            source_attrs,
            event_queue: source_eq,
            stream: Mutex::new(None),
        };
        let source: IMFMediaSourceEx = source_obj.into();

        info!("[vcam] step 9: load MFCreateVirtualCamera");
        let create_fn = load_mf_create_virtual_camera()?;

        info!("[vcam] step 10: call MFCreateVirtualCamera");
        let name: Vec<u16> = "AndroidCam\0".encode_utf16().collect();
        let source_id: Vec<u16> = ANDROID_CAM_SOURCE_ID.encode_utf16().collect();
        let categories = [KSCATEGORY_VIDEO_CAMERA];
        let mut cam_ptr: *mut std::ffi::c_void = std::ptr::null_mut();

        let hr = create_fn(
            0,
            0,
            0,
            name.as_ptr(),
            source_id.as_ptr(),
            categories.as_ptr(),
            categories.len() as u32,
            &mut cam_ptr,
        );

        info!("[vcam] step 10 result: hr={:#010x}", hr.0 as u32);
        if hr == HRESULT(0x80070057u32 as i32) {
            error!("[vcam] MFCreateVirtualCamera failed: E_INVALIDARG (0x80070057)");
            error!("[vcam] HINT: Enable Developer Mode in Windows Settings");
            return Err(hr.into());
        }
        hr.ok()?;

        info!("[vcam] step 11: add_media_source / CoRegisterClassObject");
        let camera = VirtualCamHandle::new(cam_ptr);
        let source_id_bytes = source_id.as_ptr() as *const u8;
        let source_id_size = (source_id.len() * std::mem::size_of::<u16>()) as u32;
        let friendly_name: Vec<u16> = format!("{ANDROID_CAM_FRIENDLY_NAME} Windows Virtual Camera\0")
            .encode_utf16()
            .collect();
        let friendly_name_bytes = friendly_name.as_ptr() as *const u8;
        let friendly_name_size = (friendly_name.len() * std::mem::size_of::<u16>()) as u32;
        let lifetime: i32 = 0;
        let access: i32 = 0;

        let hr_source = camera.add_property(
            &DEVPKEY_DEVICEINTERFACE_VCAMCREATE_SOURCEID,
            DEVPROP_TYPE_STRING,
            source_id_bytes,
            source_id_size,
        );
        info!("[vcam] step 11a AddProperty(SourceId) hr={:#010x}", hr_source.0 as u32);
        hr_source.ok()?;

        let hr_name = camera.add_property(
            &DEVPKEY_DEVICEINTERFACE_VCAMCREATE_FRIENDLYNAME,
            DEVPROP_TYPE_STRING,
            friendly_name_bytes,
            friendly_name_size,
        );
        info!("[vcam] step 11b AddProperty(FriendlyName) hr={:#010x}", hr_name.0 as u32);
        hr_name.ok()?;

        let hr_lifetime = camera.add_property(
            &DEVPKEY_DEVICEINTERFACE_VCAMCREATE_LIFETIME,
            DEVPROP_TYPE_INT32,
            &lifetime as *const i32 as *const u8,
            std::mem::size_of::<i32>() as u32,
        );
        info!("[vcam] step 11c AddProperty(Lifetime) hr={:#010x}", hr_lifetime.0 as u32);
        hr_lifetime.ok()?;

        let hr_access = camera.add_property(
            &DEVPKEY_DEVICEINTERFACE_VCAMCREATE_ACCESS,
            DEVPROP_TYPE_INT32,
            &access as *const i32 as *const u8,
            std::mem::size_of::<i32>() as u32,
        );
        info!("[vcam] step 11d AddProperty(Access) hr={:#010x}", hr_access.0 as u32);
        hr_access.ok()?;

        // Always register the class factory via CoRegisterClassObject so
        // that Frame Server can CoCreateInstance the source CLSID during Start().
        let factory_obj = factory::AndroidCamSourceFactory {
            shared: Arc::clone(&shared),
            presentation_desc,
            stream_desc,
        };
        let factory_com: IClassFactory = factory_obj.into();
        let reg_cookie = CoRegisterClassObject(
            &ANDROID_CAM_SOURCE_CLSID,
            &factory_com,
            CLSCTX_LOCAL_SERVER,
            REGCLS_MULTIPLEUSE,
        )?;
        info!("[vcam] step 11 (CoRegisterClassObject) cookie={}", reg_cookie);

        info!("[vcam] step 12: camera.start()");
        let hr_start = camera.start();
        info!("[vcam] step 12 result: hr={:#010x}", hr_start.0 as u32);
        hr_start.ok()?;

        info!(
            "[vcam] IMFVirtualCamera ready ({}x{} NV12 @{}fps)",
            width,
            height,
            OUTPUT_FPS_N
        );

        Ok(Self {
            _camera: camera,
            _source: source,
            _factory: Some(factory_com),
            reg_cookie,
            shared,
        })
    }

    /// Writes one NV12 frame. Returns false if stream is no longer writable.
    pub fn write_frame(&mut self, nv12: &[u8]) -> bool {
        if !self.shared.running.load(Ordering::SeqCst) {
            return false;
        }

        let mut inner = match self.shared.inner.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };

        if let Some(_token) = inner.pending_tokens.pop_front() {
            let sample_time = inner.sample_time;
            inner.sample_time += constants::HNS_PER_SEC / constants::OUTPUT_FPS_N as i64;

            if let Some(ref eq) = inner.event_queue {
                let eq_clone = eq.clone();
                let data = nv12.to_vec();
                let (w, h) = (self.shared.width, self.shared.height);
                drop(inner);

                unsafe {
                    if let Ok(sample) = build_sample(&data, w, h, sample_time) {
                        let _ = eq_clone.QueueEventParamUnk(
                            MEMediaSample.0 as u32,
                            &GUID::zeroed(),
                            S_OK,
                            &sample,
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
        if self.reg_cookie != 0 {
            unsafe {
                let _ = CoRevokeClassObject(self.reg_cookie);
            }
        }
        unsafe {
            let _ = MFShutdown();
        }
        info!("[vcam] VirtualCamWriter dropped");
    }
}
