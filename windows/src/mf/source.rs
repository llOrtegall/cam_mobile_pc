use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use log::info;
use windows::core::*;
use windows::Win32::Foundation::{E_INVALIDARG, E_NOTIMPL, E_POINTER, S_OK, ERROR_SET_NOT_FOUND, WIN32_ERROR};
use windows::Win32::Media::KernelStreaming::*;
use windows::Win32::Media::MediaFoundation::*;

use super::constants::{
    ANDROID_CAM_FRIENDLY_NAME,
    KSCATEGORY_VIDEO_CAMERA,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_CATEGORY,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
};
use super::stream::AndroidCamStream;
use super::types::StreamShared;

fn hresult_from_win32(error: WIN32_ERROR) -> HRESULT {
    HRESULT((((error.0 as u32) & 0x0000_FFFF) | (7 << 16) | 0x8000_0000) as i32)
}

#[implement(IMFMediaSourceEx, IMFMediaEventGenerator, IMFGetService, IKsControl, IMFSampleAllocatorControl)]
pub(super) struct AndroidCamSource {
    pub(super) shared: Arc<StreamShared>,
    pub(super) presentation_desc: IMFPresentationDescriptor,
    pub(super) stream_desc: IMFStreamDescriptor,
    pub(super) source_attrs: IMFAttributes,
    pub(super) event_queue: IMFMediaEventQueue,
    pub(super) stream: Mutex<Option<IMFMediaStream>>,
}

pub(super) unsafe fn build_source_attributes(seed: Option<&IMFAttributes>) -> Result<IMFAttributes> {
    let mut attrs: Option<IMFAttributes> = None;
    MFCreateAttributes(&mut attrs, 8)?;
    let attrs = attrs.ok_or(windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;

    if let Some(seed) = seed {
        seed.CopyAllItems(&attrs)?;
    }

    attrs.SetGUID(
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
    )?;
    attrs.SetGUID(
        &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_CATEGORY,
        &KSCATEGORY_VIDEO_CAMERA,
    )?;
    attrs.SetString(
        &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
        &windows::core::HSTRING::from(ANDROID_CAM_FRIENDLY_NAME),
    )?;

    Ok(attrs)
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
        pvvalue: *const PROPVARIANT,
    ) -> Result<()> {
        let pv = if pvvalue.is_null() { None } else { Some(pvvalue) };
        unsafe {
            let ev = MFCreateMediaEvent(met, guidextendedtype, hrstatus, pv)?;
            self.event_queue.QueueEvent(&ev)
        }
    }
}

impl IMFMediaSourceEx_Impl for AndroidCamSource_Impl {
    fn GetStreamAttributes(&self, dwstreamindex: u32) -> Result<IMFAttributes> {
        info!("[vcam] GetStreamAttributes(index={dwstreamindex}) called");
        if dwstreamindex == 0 {
            self.stream_desc.cast()
        } else {
            Err(E_INVALIDARG.into())
        }
    }

    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        info!("[vcam] GetSourceAttributes() called");
        Ok(self.source_attrs.clone())
    }

    fn SetD3DManager(&self, _pmanager: Option<&IUnknown>) -> Result<()> {
        info!("[vcam] SetD3DManager() called");
        Err(windows::core::Error::from(E_NOTIMPL))
    }
}

impl IMFMediaSource_Impl for AndroidCamSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        info!("[vcam] GetCharacteristics() called");
        Ok(MFMEDIASOURCE_IS_LIVE.0 as u32)
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        info!("[vcam] CreatePresentationDescriptor() called");
        Ok(self.presentation_desc.clone())
    }

    fn Start(
        &self,
        _pdescriptor: Option<&IMFPresentationDescriptor>,
        _pguidtimeformat: *const GUID,
        _pvarstartposition: *const PROPVARIANT,
    ) -> Result<()> {
        info!("[vcam] Source::Start() called");
        let source_intf: IMFMediaSource = unsafe { self.cast()? };
        let stream_obj = AndroidCamStream {
            shared: Arc::clone(&self.shared),
            stream_desc: self.stream_desc.clone(),
            source: source_intf,
        };
        let stream2: IMFMediaStream2 = stream_obj.into();
        let stream: IMFMediaStream = stream2.cast()?;

        // The stream event queue was pre-created in VirtualCamWriter::try_new
        // (before any MF callback) to avoid a re-entrant call into mfplat.
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.stream_started = true;
        }
        *self.stream.lock().unwrap() = Some(stream.clone());

        let stream_unk: IUnknown = stream.cast()?;
        unsafe {
            self.event_queue.QueueEventParamUnk(
                MENewStream.0 as u32,
                &GUID::zeroed(),
                S_OK,
                &stream_unk,
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

    fn Pause(&self) -> Result<()> {
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        self.shared.running.store(false, Ordering::SeqCst);
        unsafe {
            self.event_queue.Shutdown()?;
        }
        Ok(())
    }
}

impl IMFGetService_Impl for AndroidCamSource_Impl {
    fn GetService(
        &self,
        _guidservice: *const GUID,
        _riid: *const GUID,
        ppvobject: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        if ppvobject.is_null() {
            return Err(E_POINTER.into());
        }

        unsafe {
            *ppvobject = std::ptr::null_mut();
        }
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for AndroidCamSource_Impl {
    fn KsProperty(
        &self,
        _property: *const KSIDENTIFIER,
        _propertylength: u32,
        _propertydata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> Result<()> {
        Err(hresult_from_win32(ERROR_SET_NOT_FOUND).into())
    }

    fn KsMethod(
        &self,
        _method: *const KSIDENTIFIER,
        _methodlength: u32,
        _methoddata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> Result<()> {
        Err(hresult_from_win32(ERROR_SET_NOT_FOUND).into())
    }

    fn KsEvent(
        &self,
        _event: *const KSIDENTIFIER,
        _eventlength: u32,
        _eventdata: *mut core::ffi::c_void,
        _datalength: u32,
        _bytesreturned: *mut u32,
    ) -> Result<()> {
        Err(hresult_from_win32(ERROR_SET_NOT_FOUND).into())
    }
}

impl IMFSampleAllocatorControl_Impl for AndroidCamSource_Impl {
    fn SetDefaultAllocator(&self, _dwoutputstreamid: u32, _pallocator: Option<&IUnknown>) -> Result<()> {
        Ok(())
    }

    fn GetAllocatorUsage(
        &self,
        dwoutputstreamid: u32,
        pdwinputstreamid: *mut u32,
        peusage: *mut MFSampleAllocatorUsage,
    ) -> Result<()> {
        if pdwinputstreamid.is_null() || peusage.is_null() {
            return Err(E_POINTER.into());
        }

        unsafe {
            *pdwinputstreamid = dwoutputstreamid;
            *peusage = MFSampleAllocatorUsage_UsesCustomAllocator;
        }
        Ok(())
    }
}
