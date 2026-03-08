use std::sync::Arc;

use log::info;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::*;

use super::activate::AndroidCamActivate;
use super::source::{build_source_attributes, AndroidCamSource};
use super::types::StreamShared;

/// IClassFactory for AndroidCamActivate, used when the new IMFVirtualCamera interface
/// (IID 1c08a864) is present.  In that case, IMFVirtualCamera::Start() calls
/// CoCreateInstance(sourceId CLSID) and requests IMFActivate from the factory.
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

            // Try returning an AndroidCamSource directly (Frame Server often
            // requests IMFMediaSource / IMFMediaSourceEx).
            let source_eq: IMFMediaEventQueue = MFCreateEventQueue()?;
            let source_attrs = build_source_attributes(None)?;
            let source_obj = AndroidCamSource {
                shared: Arc::clone(&self.shared),
                presentation_desc: self.presentation_desc.clone(),
                stream_desc: self.stream_desc.clone(),
                source_attrs,
                event_queue: source_eq,
                stream: std::sync::Mutex::new(None),
            };
            let source: IMFMediaSourceEx = source_obj.into();
            let unk: IUnknown = source.cast()?;
            let hr = unk.query(riid, ppv);
            if hr.is_ok() {
                info!("[vcam] CreateInstance → IMFMediaSourceEx QI ok");
                return Ok(());
            }
            info!("[vcam] CreateInstance → source QI failed ({:#010x}), trying IMFActivate", hr.0 as u32);

            // Fall back to IMFActivate wrapper.
            let activate_obj = AndroidCamActivate::new(
                Arc::clone(&self.shared),
                self.presentation_desc.clone(),
                self.stream_desc.clone(),
            )?;
            let activate: IMFActivate = activate_obj.into();
            let unk: IUnknown = activate.cast()?;
            let hr = unk.query(riid, ppv);
            info!("[vcam] CreateInstance → IMFActivate QI result: hr={:#010x}", hr.0 as u32);
            hr.ok()
        }
    }

    fn LockServer(&self, _f_lock: BOOL) -> Result<()> {
        Ok(())
    }
}
