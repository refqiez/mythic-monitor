use libloading::{Library, Symbol};
use std::os::raw::{c_void, c_float};
use std::path::Path;
use std::ptr::NonNull;
use crate::base::MYTHIC_VERSION;

pub const API_MAGIC: u32 = 0x5ABAD0B1;

#[repr(C)]
pub struct ABI {
    pub magic: u32,
    pub version_major: u8,
    pub version_minor: u8,
    pub tier: u8,
    pub create:     unsafe extern "C" fn() -> *mut c_void,
    pub destroy:    unsafe extern "C" fn(*mut c_void),
    pub refresh:    unsafe extern "C" fn(*mut c_void),
    pub read:       unsafe extern "C" fn(*mut c_void, u16) -> c_float,
    pub register:   unsafe extern "C" fn(*mut c_void, *const u8, u64) -> u16,
    pub unregister: unsafe extern "C" fn(*mut c_void, u16),
}

#[derive(Debug)]
pub struct Sensor {
    lib: Library,
    vtable: NonNull<ABI>,
    handle: NonNull<c_void>,
}

pub enum LoadError {
    LibLoading(libloading::Error),
    MagicMismatch(u32),
    MajorVersionMismatch(u8, u8),
    MinorVersionMismatch(u8, u8),
    NullVtable,
    NullHandle,
}

impl From<libloading::Error> for LoadError {
    fn from(e: libloading::Error) -> Self {
        LoadError::LibLoading(e)
    }
}

impl Sensor {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, LoadError> { unsafe {
        let lib = Library::new(path.as_ref().as_os_str())?;
        let get_vtable: Symbol<fn() -> *const ABI> = lib.get(b"get_vtable")?;

        let vtable= get_vtable();
        let vtable = NonNull::new(vtable as *mut ABI)
        .ok_or(LoadError::NullVtable)?;

        if vtable.as_ref().magic != API_MAGIC { return Err(LoadError::MagicMismatch(vtable.as_ref().magic)); }

        if vtable.as_ref().version_major != MYTHIC_VERSION.major { return Err(LoadError::MajorVersionMismatch(vtable.as_ref().version_major, MYTHIC_VERSION.major)); }
        if vtable.as_ref().version_minor >  MYTHIC_VERSION.minor { return Err(LoadError::MinorVersionMismatch(vtable.as_ref().version_minor, MYTHIC_VERSION.minor)); }

        let handle = (vtable.as_ref().create)();
        let handle = NonNull::new(handle)
        .ok_or(LoadError::NullHandle)?;

        Ok(Self { lib, vtable, handle })
    }}

    pub fn read(&self, sid: u16) -> f32 { unsafe {
        (self.vtable.as_ref().read)(self.handle.as_ptr(), sid)
    }}

    pub fn register(&mut self, path: &str) -> u16 { unsafe {
        (self.vtable.as_ref().register)(self.handle.as_ptr(), path.as_ptr(), path.len() as u64)
    }}

    pub fn unregister(&mut self, sid: u16) { unsafe {
        (self.vtable.as_ref().unregister)(self.handle.as_ptr(), sid)
    }}

    pub fn refresh(&mut self) { unsafe {
        (self.vtable.as_ref().refresh)(self.handle.as_ptr())
    }}
}

impl Drop for Sensor {
    fn drop(&mut self) { unsafe {
        (self.vtable.as_ref().destroy)(self.handle.as_ptr());
    }}
}