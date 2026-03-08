use std::sync::{Arc, Mutex};

use log::info;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Media::MediaFoundation::*;

use super::source::AndroidCamSource;
use super::types::StreamShared;

/// IMFActivate wrapper returned by IClassFactory::CreateInstance.
///
/// Frame Server (new IMFVirtualCamera interface) asks CoCreateInstance for IMFActivate
/// ({7fee9e9a-...}), not directly for IMFMediaSource. It then calls
/// IMFActivate::ActivateObject to get the actual IMFMediaSource.
///
/// IMFAttributes (30 methods) is the base interface; we delegate everything to
/// an inner store created by MFCreateAttributes.
#[implement(IMFActivate)]
pub(super) struct AndroidCamActivate {
    shared: Arc<StreamShared>,
    presentation_desc: IMFPresentationDescriptor,
    stream_desc: IMFStreamDescriptor,
    /// Backing attribute store — delegates all IMFAttributes calls.
    attrs: IMFAttributes,
    /// Cached source returned by ActivateObject (reset by ShutdownObject/DetachObject).
    active_source: Mutex<Option<IMFMediaSourceEx>>,
}

impl AndroidCamActivate {
    pub(super) unsafe fn new(
        shared: Arc<StreamShared>,
        presentation_desc: IMFPresentationDescriptor,
        stream_desc: IMFStreamDescriptor,
    ) -> Result<Self> {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 0)?;
        let attrs = attrs.unwrap();
        Ok(Self {
            shared,
            presentation_desc,
            stream_desc,
            attrs,
            active_source: Mutex::new(None),
        })
    }
}

// ---------------------------------------------------------------------------
// IMFAttributes delegation — all 30 methods forwarded to the inner store.
// Calling signatures in windows-rs 0.58 differ from raw COM in several places:
//   • PROPVARIANT out params → Option<*mut PROPVARIANT>
//   • Buffer params (PWSTR, u32) → &mut [u16]
//   • Buffer params (u8*, u32)   → &mut [u8] / &[u8]
//   • GetUnknown is generic → delegate via IUnknown + query()
// ---------------------------------------------------------------------------
impl IMFAttributes_Impl for AndroidCamActivate_Impl {
    fn GetItem(&self, guidkey: *const GUID, pvalue: *mut PROPVARIANT) -> Result<()> {
        unsafe {
            let pv = if pvalue.is_null() { None } else { Some(pvalue) };
            self.attrs.GetItem(guidkey, pv)
        }
    }

    fn GetItemType(&self, guidkey: *const GUID) -> Result<MF_ATTRIBUTE_TYPE> {
        unsafe { self.attrs.GetItemType(guidkey) }
    }

    fn CompareItem(&self, guidkey: *const GUID, value: *const PROPVARIANT) -> Result<BOOL> {
        unsafe { self.attrs.CompareItem(guidkey, value) }
    }

    fn Compare(
        &self,
        ptheirs: Option<&IMFAttributes>,
        matchtype: MF_ATTRIBUTES_MATCH_TYPE,
    ) -> Result<BOOL> {
        unsafe { self.attrs.Compare(ptheirs, matchtype) }
    }

    fn GetUINT32(&self, guidkey: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetUINT32(guidkey) }
    }

    fn GetUINT64(&self, guidkey: *const GUID) -> Result<u64> {
        unsafe { self.attrs.GetUINT64(guidkey) }
    }

    fn GetDouble(&self, guidkey: *const GUID) -> Result<f64> {
        unsafe { self.attrs.GetDouble(guidkey) }
    }

    fn GetGUID(&self, guidkey: *const GUID) -> Result<GUID> {
        unsafe { self.attrs.GetGUID(guidkey) }
    }

    fn GetStringLength(&self, guidkey: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetStringLength(guidkey) }
    }

    fn GetString(
        &self,
        guidkey: *const GUID,
        pwszvalue: PWSTR,
        cchbufsize: u32,
        pcchLength: *mut u32,
    ) -> Result<()> {
        unsafe {
            let slice = std::slice::from_raw_parts_mut(pwszvalue.0, cchbufsize as usize);
            let pcchl = if pcchLength.is_null() { None } else { Some(pcchLength) };
            self.attrs.GetString(guidkey, slice, pcchl)
        }
    }

    fn GetAllocatedString(
        &self,
        guidkey: *const GUID,
        ppwszvalue: *mut PWSTR,
        pcchLength: *mut u32,
    ) -> Result<()> {
        unsafe { self.attrs.GetAllocatedString(guidkey, ppwszvalue, pcchLength) }
    }

    fn GetBlobSize(&self, guidkey: *const GUID) -> Result<u32> {
        unsafe { self.attrs.GetBlobSize(guidkey) }
    }

    fn GetBlob(
        &self,
        guidkey: *const GUID,
        pbuf: *mut u8,
        cbbufsize: u32,
        pcbBlobSize: *mut u32,
    ) -> Result<()> {
        unsafe {
            let slice = std::slice::from_raw_parts_mut(pbuf, cbbufsize as usize);
            let pcbs = if pcbBlobSize.is_null() { None } else { Some(pcbBlobSize) };
            self.attrs.GetBlob(guidkey, slice, pcbs)
        }
    }

    fn GetAllocatedBlob(
        &self,
        guidkey: *const GUID,
        ppbuf: *mut *mut u8,
        pcbsize: *mut u32,
    ) -> Result<()> {
        unsafe { self.attrs.GetAllocatedBlob(guidkey, ppbuf, pcbsize) }
    }

    fn GetUnknown(
        &self,
        guidkey: *const GUID,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        unsafe {
            let unk: IUnknown = self.attrs.GetUnknown(guidkey)?;
            unk.query(riid, ppv).ok()
        }
    }

    fn SetItem(&self, guidkey: *const GUID, value: *const PROPVARIANT) -> Result<()> {
        unsafe { self.attrs.SetItem(guidkey, value) }
    }

    fn DeleteItem(&self, guidkey: *const GUID) -> Result<()> {
        unsafe { self.attrs.DeleteItem(guidkey) }
    }

    fn DeleteAllItems(&self) -> Result<()> {
        unsafe { self.attrs.DeleteAllItems() }
    }

    fn SetUINT32(&self, guidkey: *const GUID, unvalue: u32) -> Result<()> {
        unsafe { self.attrs.SetUINT32(guidkey, unvalue) }
    }

    fn SetUINT64(&self, guidkey: *const GUID, unvalue: u64) -> Result<()> {
        unsafe { self.attrs.SetUINT64(guidkey, unvalue) }
    }

    fn SetDouble(&self, guidkey: *const GUID, fvalue: f64) -> Result<()> {
        unsafe { self.attrs.SetDouble(guidkey, fvalue) }
    }

    fn SetGUID(&self, guidkey: *const GUID, guidvalue: *const GUID) -> Result<()> {
        unsafe { self.attrs.SetGUID(guidkey, guidvalue) }
    }

    fn SetString(&self, guidkey: *const GUID, wszvalue: &PCWSTR) -> Result<()> {
        unsafe { self.attrs.SetString(guidkey, *wszvalue) }
    }

    fn SetBlob(&self, guidkey: *const GUID, pbuf: *const u8, cbbufsize: u32) -> Result<()> {
        unsafe {
            let slice = std::slice::from_raw_parts(pbuf, cbbufsize as usize);
            self.attrs.SetBlob(guidkey, slice)
        }
    }

    fn SetUnknown(&self, guidkey: *const GUID, punk: Option<&IUnknown>) -> Result<()> {
        unsafe { self.attrs.SetUnknown(guidkey, punk) }
    }

    fn LockStore(&self) -> Result<()> {
        unsafe { self.attrs.LockStore() }
    }

    fn UnlockStore(&self) -> Result<()> {
        unsafe { self.attrs.UnlockStore() }
    }

    fn GetCount(&self) -> Result<u32> {
        unsafe { self.attrs.GetCount() }
    }

    fn GetItemByIndex(
        &self,
        unindex: u32,
        pguidkey: *mut GUID,
        pvalue: *mut PROPVARIANT,
    ) -> Result<()> {
        unsafe {
            let pv = if pvalue.is_null() { None } else { Some(pvalue) };
            self.attrs.GetItemByIndex(unindex, pguidkey, pv)
        }
    }

    fn CopyAllItems(&self, pdest: Option<&IMFAttributes>) -> Result<()> {
        unsafe { self.attrs.CopyAllItems(pdest) }
    }
}

// ---------------------------------------------------------------------------
// IMFActivate — the three methods Frame Server actually cares about.
// ---------------------------------------------------------------------------
impl IMFActivate_Impl for AndroidCamActivate_Impl {
    fn ActivateObject(
        &self,
        riid: *const GUID,
        ppv: *mut *mut core::ffi::c_void,
    ) -> Result<()> {
        info!("[vcam] IMFActivate::ActivateObject called");
        let mut lock = self.active_source.lock().unwrap();
        if lock.is_none() {
            let event_queue: IMFMediaEventQueue = unsafe { MFCreateEventQueue()? };
            let source_obj = AndroidCamSource {
                shared: Arc::clone(&self.shared),
                presentation_desc: self.presentation_desc.clone(),
                stream_desc: self.stream_desc.clone(),
                event_queue,
                stream: Mutex::new(None),
            };
            let source: IMFMediaSourceEx = source_obj.into();
            *lock = Some(source);
        }
        let source = lock.as_ref().unwrap();
        let unk: IUnknown = source.cast()?;
        unsafe {
            let hr = unk.query(riid, ppv);
            info!("[vcam] ActivateObject QI result: hr={:#010x}", hr.0 as u32);
            hr.ok()
        }
    }

    fn ShutdownObject(&self) -> Result<()> {
        info!("[vcam] IMFActivate::ShutdownObject called");
        *self.active_source.lock().unwrap() = None;
        Ok(())
    }

    fn DetachObject(&self) -> Result<()> {
        info!("[vcam] IMFActivate::DetachObject called");
        *self.active_source.lock().unwrap() = None;
        Ok(())
    }
}
