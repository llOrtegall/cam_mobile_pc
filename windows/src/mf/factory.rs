use std::sync::{Arc, Mutex};

use log::info;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::*;

use super::source::AndroidCamSource;
use super::types::StreamShared;

/// IClassFactory for AndroidCamSource, used when the new IMFVirtualCamera interface
/// (IID 1c08a864) is present.  In that case, IMFVirtualCamera::Start() calls
/// CoCreateInstance(sourceId CLSID) to instantiate the media source.
/// We register this factory via CoRegisterClassObject before calling Start so
/// that Frame Server finds the running factory instead of trying to launch a new
/// process or looking in the registry.
#[implement(IClassFactory)]
pub(super) struct AndroidCamSourceFactory {
    pub(super) shared: Arc<StreamShared>,
    pub(super) presentation_desc: IMFPresentationDescriptor,
    pub(super) stream_desc: IMFStreamDescriptor,
}

impl IClassFactory_Impl for AndroidCamSourceFactory_Impl {
    fn CreateInstance(
        &self,
        _punk_outer: Option<&IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        unsafe {
            let g = &*riid;
            info!(
                "[vcam] IClassFactory::CreateInstance riid={{{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
                g.data1, g.data2, g.data3,
                g.data4[0], g.data4[1],
                g.data4[2], g.data4[3], g.data4[4], g.data4[5], g.data4[6], g.data4[7]
            );
            let event_queue: IMFMediaEventQueue = MFCreateEventQueue()?;
            let source_obj = AndroidCamSource {
                shared: Arc::clone(&self.shared),
                presentation_desc: self.presentation_desc.clone(),
                stream_desc: self.stream_desc.clone(),
                event_queue,
                stream: Mutex::new(None),
            };
            let source: IMFMediaSourceEx = source_obj.into();
            let unk: IUnknown = source.cast()?;
            let hr = unk.query(riid, ppv);
            info!("[vcam] CreateInstance QI result: hr={:#010x}", hr.0 as u32);
            hr.ok()
        }
    }

    fn LockServer(&self, _f_lock: BOOL) -> Result<()> {
        Ok(())
    }
}
