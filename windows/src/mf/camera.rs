use log::{error, info};

use windows::core::{w, Error, GUID, HRESULT, PCSTR, Result};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

// IID of the "classic" IMFVirtualCamera that has AddStreamConfig and AddMediaSource.
// windows-rs 0.58 binds a different (newer) interface (IID 1c08a864...) that inherits from
// IMFAttributes and has no AddMediaSource.  We need the classic one for Frame Server sources.
//
// Classic vtable layout (inherits from IUnknown only):
//   0  QueryInterface
//   1  AddRef
//   2  Release
//   3  AddStreamConfig(IMFSensorProfile*)
//   4  AddProperty(REFPROPERTYKEY, DEVPROPTYPE, data, size)
//   5  AddRegistryEntry(LPCWSTR, LPCWSTR, DWORD, data, size)
//   6  AddDeviceSourceInfo(LPCWSTR)
//   7  AddMediaSource(IUnknown*, IMFAttributes*)
//   8  Start(IMFAttributes*)
//   9  Stop()
//  10  Remove()
//  11  GetMediaSource(IMFMediaSource**)
const IID_CLASSIC_IMF_VIRTUAL_CAMERA: GUID = GUID {
    data1: 0xa831c8e9,
    data2: 0xdd16,
    data3: 0x4c89,
    data4: [0xbf, 0xaf, 0x1b, 0x7c, 0xf4, 0xe3, 0x9b, 0x35],
};

/// Wraps the raw COM pointer(s) returned by MFCreateVirtualCamera.
///
/// `MFCreateVirtualCamera` may return an object whose primary vtable inherits from
/// IMFAttributes (the windows-rs 0.58 interface, IID 1c08a864…).  That interface has no
/// AddMediaSource.  We call QueryInterface for the classic IID (a831c8e9…) to get a
/// separate interface pointer whose vtable starts immediately after IUnknown, placing
/// AddMediaSource at slot 7.
pub(super) struct VirtualCamHandle {
    /// Primary pointer returned by MFCreateVirtualCamera.
    raw: *mut std::ffi::c_void,
    /// Pointer to the classic IMFVirtualCamera interface.
    /// May equal `raw` if QI returned the same pointer, or be a separate pointer if the
    /// object exposes both interfaces.
    classic: *mut std::ffi::c_void,
    /// True if `classic` is a different pointer from `raw` and needs its own Release().
    classic_separate: bool,
}

unsafe impl Send for VirtualCamHandle {}

/// Calls QueryInterface on a raw COM pointer.
unsafe fn query_interface(
    ptr: *mut std::ffi::c_void,
    iid: *const GUID,
) -> std::result::Result<*mut std::ffi::c_void, HRESULT> {
    let qi: unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *const GUID,
        *mut *mut std::ffi::c_void,
    ) -> HRESULT = std::mem::transmute(*(*ptr.cast::<*const *const ()>()).add(0));
    let mut out: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = qi(ptr, iid, &mut out);
    if hr.is_ok() && !out.is_null() {
        Ok(out)
    } else {
        Err(hr)
    }
}

impl VirtualCamHandle {
    pub(super) unsafe fn new(raw: *mut std::ffi::c_void) -> Self {
        match query_interface(raw, &IID_CLASSIC_IMF_VIRTUAL_CAMERA) {
            Ok(classic) if classic != raw => {
                info!("[vcam] QI(classic IID) → separate interface ptr");
                VirtualCamHandle { raw, classic, classic_separate: true }
            }
            Ok(classic) => {
                info!("[vcam] QI(classic IID) → same ptr (object IS the classic interface)");
                VirtualCamHandle { raw, classic, classic_separate: false }
            }
            Err(hr) => {
                info!(
                    "[vcam] QI(classic IID) failed hr={:#010x} — using primary ptr at slot 7",
                    hr.0 as u32
                );
                VirtualCamHandle { raw, classic: raw, classic_separate: false }
            }
        }
    }

    #[inline]
    unsafe fn vtable(&self) -> *const *const () {
        *(self.classic as *const *const *const ())
    }

    pub(super) unsafe fn add_media_source(
        &self,
        source_unk: *mut std::ffi::c_void,
    ) -> HRESULT {
        // Slot 7 on the classic IMFVirtualCamera (IUnknown-based) vtable.
        let f: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> HRESULT = std::mem::transmute(*self.vtable().add(7));
        f(self.classic, source_unk, std::ptr::null_mut())
    }

    pub(super) unsafe fn start(&self) -> HRESULT {
        // Slot 8 on the classic vtable.
        let f: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> HRESULT = std::mem::transmute(*self.vtable().add(8));
        f(self.classic, std::ptr::null_mut())
    }

    unsafe fn remove(&self) -> HRESULT {
        // Slot 10 on the classic vtable.
        let f: unsafe extern "system" fn(*mut std::ffi::c_void) -> HRESULT =
            std::mem::transmute(*self.vtable().add(10));
        f(self.classic)
    }

    unsafe fn release_ptr(ptr: *mut std::ffi::c_void) -> u32 {
        let f: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32 =
            std::mem::transmute(*(*(ptr as *const *const *const ())).add(2));
        f(ptr)
    }
}

impl Drop for VirtualCamHandle {
    fn drop(&mut self) {
        if self.raw.is_null() {
            return;
        }
        unsafe {
            let _ = self.remove();
            if self.classic_separate && !self.classic.is_null() {
                Self::release_ptr(self.classic);
            }
            Self::release_ptr(self.raw);
        }
    }
}

pub(super) type MFCreateVirtualCameraFn = unsafe extern "system" fn(
    r#type: i32,
    lifetime: i32,
    access: i32,
    friendly_name: *const u16,
    source_id: *const u16,
    categories: *const windows::core::GUID,
    category_count: u32,
    camera: *mut *mut std::ffi::c_void,
) -> HRESULT;

pub(super) unsafe fn load_mf_create_virtual_camera() -> Result<MFCreateVirtualCameraFn> {
    // Keep runtime loading to avoid depending on incorrect windows-rs binding metadata.
    let hmod = LoadLibraryW(w!("mfsensorgroup.dll")).map_err(|e| {
        error!("[vcam] LoadLibraryW(mfsensorgroup.dll) failed: {e}");
        e
    })?;

    let proc =
        GetProcAddress(hmod, PCSTR(b"MFCreateVirtualCamera\0".as_ptr())).ok_or_else(|| {
            error!("[vcam] GetProcAddress(MFCreateVirtualCamera) failed");
            Error::from(E_NOTIMPL)
        })?;

    Ok(std::mem::transmute(proc))
}
