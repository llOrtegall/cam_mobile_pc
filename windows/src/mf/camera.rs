use log::{error, info};

use windows::core::{w, Error, GUID, HRESULT, PCSTR, Result};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use super::constants::DevPropKey;

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
/// Two IMFVirtualCamera vtable layouts exist on Windows 11:
///
/// Classic (IID a831c8e9, inherits IUnknown only) — vtable slots:
///   0-2  IUnknown, 3 AddStreamConfig, 4 AddProperty, 5 AddRegistryEntry,
///   6 AddDeviceSourceInfo, 7 AddMediaSource, 8 Start(IMFAttributes*),
///   9 Stop, 10 Remove, 11 GetMediaSource
///
/// New (IID 1c08a864, inherits IMFAttributes — 30 extra slots) — vtable slots:
///   0-2  IUnknown, 3-32 IMFAttributes (30 methods),
///   33 AddStreamConfig, 34 AddProperty, 35 AddRegistryEntry,
///   36 AddDeviceSourceInfo, 37 AddMediaSource, 38 Start(IMFAsyncCallback*),
///   39 Stop, 40 Remove, 41 GetMediaSource
///
/// We QI for the classic IID first.  If that fails (E_NOINTERFACE) we fall
/// back to the primary pointer and use the new (offset +30) slot numbers.
pub(super) struct VirtualCamHandle {
    /// Primary pointer returned by MFCreateVirtualCamera.
    raw: *mut std::ffi::c_void,
    /// Pointer used for vtable dispatch (classic IID ptr, or == raw if QI failed).
    classic: *mut std::ffi::c_void,
    /// True if `classic` is a different pointer from `raw` and needs its own Release().
    classic_separate: bool,
    /// True  → use classic offsets (AddMediaSource=7, Start=8, Remove=10).
    /// False → use new-interface offsets (AddMediaSource=37, Start=38, Remove=40).
    use_classic_offsets: bool,
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
                info!("[vcam] QI(classic IID) → separate ptr, using classic offsets (7/8/10)");
                VirtualCamHandle { raw, classic, classic_separate: true, use_classic_offsets: true }
            }
            Ok(classic) => {
                info!("[vcam] QI(classic IID) → same ptr, using classic offsets (7/8/10)");
                VirtualCamHandle { raw, classic, classic_separate: false, use_classic_offsets: true }
            }
            Err(hr) => {
                info!(
                    "[vcam] QI(classic IID) failed hr={:#010x} — using new-interface offsets (37/38/40)",
                    hr.0 as u32
                );
                VirtualCamHandle { raw, classic: raw, classic_separate: false, use_classic_offsets: false }
            }
        }
    }

    #[inline]
    unsafe fn vtable(&self) -> *const *const () {
        *(self.classic as *const *const *const ())
    }

    /// Returns true if the classic (IUnknown-based) interface was obtained via QI.
    /// False means the primary pointer uses the new IMFAttributes-based vtable layout.
    pub(super) fn supports_add_media_source(&self) -> bool {
        self.use_classic_offsets
    }

    pub(super) unsafe fn add_media_source(
        &self,
        source_unk: *mut std::ffi::c_void,
    ) -> HRESULT {
        // Only valid on classic (IUnknown-based) vtable: slot 7.
        // The new (IMFAttributes-based) interface does NOT have AddMediaSource.
        debug_assert!(self.use_classic_offsets, "add_media_source called on new interface");
        let f: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> HRESULT = std::mem::transmute(*self.vtable().add(7));
        f(self.classic, source_unk, std::ptr::null_mut())
    }

    pub(super) unsafe fn start(&self) -> HRESULT {
        // Classic (IUnknown-based) vtable: slot 8   Start(IMFAttributes*)
        // New (IMFAttributes-based) vtable: slot 36  Start(IMFAsyncCallback*)
        let slot = if self.use_classic_offsets { 8 } else { 36 };
        let f: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> HRESULT = std::mem::transmute(*self.vtable().add(slot));
        f(self.classic, std::ptr::null_mut())
    }

    pub(super) unsafe fn add_property(
        &self,
        key: &DevPropKey,
        prop_type: u32,
        data: *const u8,
        size: u32,
    ) -> HRESULT {
        let slot = if self.use_classic_offsets { 4 } else { 34 };
        let f: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *const DevPropKey,
            u32,
            *const u8,
            u32,
        ) -> HRESULT = std::mem::transmute(*self.vtable().add(slot));
        f(self.classic, key, prop_type, data, size)
    }

    unsafe fn remove(&self) -> HRESULT {
        // Classic: slot 10  |  New: slot 38
        let slot = if self.use_classic_offsets { 10 } else { 38 };
        let f: unsafe extern "system" fn(*mut std::ffi::c_void) -> HRESULT =
            std::mem::transmute(*self.vtable().add(slot));
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
