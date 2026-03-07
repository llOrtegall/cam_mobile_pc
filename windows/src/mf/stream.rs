use std::sync::atomic::Ordering;
use std::sync::Arc;

use windows::core::*;
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::MediaFoundation::*;

use super::constants::{HNS_PER_SEC, OUTPUT_FPS_N};
use super::types::{build_sample, StreamShared};

#[implement(IMFMediaStream, IMFMediaEventGenerator)]
pub(super) struct AndroidCamStream {
    pub(super) shared: Arc<StreamShared>,
    pub(super) stream_desc: IMFStreamDescriptor,
    pub(super) source: IMFMediaSource,
}

impl IMFMediaEventGenerator_Impl for AndroidCamStream_Impl {
    fn GetEvent(&self, dwflags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        let q = self.shared.inner.lock().unwrap().event_queue.clone();
        match q {
            Some(q) => unsafe { q.GetEvent(dwflags.0) },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn BeginGetEvent(
        &self,
        pcallback: Option<&IMFAsyncCallback>,
        punkstate: Option<&IUnknown>,
    ) -> Result<()> {
        let q = self.shared.inner.lock().unwrap().event_queue.clone();
        match q {
            Some(q) => unsafe { q.BeginGetEvent(pcallback, punkstate) },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn EndGetEvent(&self, presult: Option<&IMFAsyncResult>) -> Result<IMFMediaEvent> {
        let q = self.shared.inner.lock().unwrap().event_queue.clone();
        match q {
            Some(q) => unsafe { q.EndGetEvent(presult) },
            None => Err(MF_E_SHUTDOWN.into()),
        }
    }

    fn QueueEvent(
        &self,
        met: u32,
        guidextendedtype: *const GUID,
        hrstatus: HRESULT,
        pvvalue: *const PROPVARIANT,
    ) -> Result<()> {
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
        // Core token flow:
        // - If a frame is already ready, send MEMediaSample immediately.
        // - Otherwise queue token and satisfy it from the next writer frame.
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
