use log::error;

use windows::core::{w, Error, PCSTR, Result, HRESULT};
use windows::Win32::Foundation::E_NOTIMPL;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

pub(super) struct VirtualCamHandle(pub(super) *mut std::ffi::c_void);

unsafe impl Send for VirtualCamHandle {}

impl VirtualCamHandle {
    #[inline]
    unsafe fn vtable(&self) -> *const *const () {
        *(self.0 as *const *const *const ())
    }

    pub(super) unsafe fn add_media_source(&self, source_unk: *mut std::ffi::c_void) -> HRESULT {
        let f: unsafe extern "system" fn(
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
            *mut std::ffi::c_void,
        ) -> HRESULT = std::mem::transmute(*self.vtable().add(3));
        f(self.0, source_unk, std::ptr::null_mut())
    }

    pub(super) unsafe fn start(&self) -> HRESULT {
        let f: unsafe extern "system" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> HRESULT =
            std::mem::transmute(*self.vtable().add(4));
        f(self.0, std::ptr::null_mut())
    }

    unsafe fn remove(&self) -> HRESULT {
        let f: unsafe extern "system" fn(*mut std::ffi::c_void) -> HRESULT =
            std::mem::transmute(*self.vtable().add(6));
        f(self.0)
    }

    unsafe fn release_com(&self) -> u32 {
        let f: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32 =
            std::mem::transmute(*self.vtable().add(2));
        f(self.0)
    }
}

impl Drop for VirtualCamHandle {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        unsafe {
            let _ = self.remove();
            self.release_com();
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

    let proc = GetProcAddress(hmod, PCSTR(b"MFCreateVirtualCamera\0".as_ptr())).ok_or_else(|| {
        error!("[vcam] GetProcAddress(MFCreateVirtualCamera) failed");
        Error::from(E_NOTIMPL)
    })?;

    Ok(std::mem::transmute(proc))
}
