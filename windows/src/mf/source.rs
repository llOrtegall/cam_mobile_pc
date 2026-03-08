use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use log::info;
use windows::core::*;
use windows::Win32::Foundation::{E_INVALIDARG, E_NOTIMPL, S_OK};
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

#[implement(IMFMediaSourceEx, IMFMediaEventGenerator)]
pub(super) struct AndroidCamSource {
    pub(super) shared: Arc<StreamShared>,
    pub(super) presentation_desc: IMFPresentationDescriptor,
    pub(super) stream_desc: IMFStreamDescriptor,
    pub(super) event_queue: IMFMediaEventQueue,
    pub(super) stream: Mutex<Option<IMFMediaStream>>,
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
        let mut attrs: Option<IMFAttributes> = None;
        unsafe {
            MFCreateAttributes(&mut attrs, 3)?;
            let a = attrs.as_ref().ok_or(windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
            a.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )?;
            a.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_CATEGORY,
                &KSCATEGORY_VIDEO_CAMERA,
            )?;
            a.SetString(
                &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME,
                &windows::core::HSTRING::from(ANDROID_CAM_FRIENDLY_NAME),
            )?;
        }
        attrs.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))
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
        let stream: IMFMediaStream = stream_obj.into();

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
        Ok(())
    }

    fn Shutdown(&self) -> Result<()> {
        self.shared.running.store(false, Ordering::SeqCst);
        unsafe {
            self.event_queue.Shutdown()?;
        }
        Ok(())
    }
}
