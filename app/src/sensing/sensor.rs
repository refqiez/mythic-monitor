use libloading::{Library, Symbol};
use std::os::raw::c_void;
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
    pub create:     unsafe extern "C" fn(*mut *mut c_void) -> u32,
    pub destroy:    unsafe extern "C" fn(*mut c_void) -> u32,
    pub refresh:    unsafe extern "C" fn(*mut c_void) -> u32,
    pub register:   unsafe extern "C" fn(*mut c_void, *const u8, u64, *mut u16) -> u32,
    pub unregister: unsafe extern "C" fn(*mut c_void, u16) -> u32,
    pub message:    unsafe extern "C" fn(*mut c_void, u32, *mut *const u8, *mut u64) -> u32,
}

#[derive(Debug)]
pub struct Sensor {
    _lib: Library, // Library unloads shared lib when dropped
    vtable: NonNull<ABI>,
    handle: NonNull<c_void>,
    destroyed: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum OpaqueErrorMsgFail<'a> {
    Error(u32),
    Null,
    NotUtf8(&'a str),
}

#[derive(Debug)]
pub struct OpaqueError<'a> {
    pub errcode: u32,
    pub message: Result<&'a str, OpaqueErrorMsgFail<'a>>,
    pub misc: u16, // used to indicate errorneous identifier fields in 'register'
}

pub struct OpaqueErrorOwned {
    pub errcode: u32,
    pub message: Result<String, OpaqueErrorMsgFail<'static>>,
    pub misc: u16, // used to indicate errorneous identifier fields in 'register'
}

impl<'a> OpaqueError<'a>  {
    pub fn to_owned(&self) -> OpaqueErrorOwned {
        let message = match self.message {
            Ok(msg) => Ok(msg.to_string()),
            Err(OpaqueErrorMsgFail::NotUtf8(s)) => {
                let mut s = s.to_string();
                s.push(char::REPLACEMENT_CHARACTER);
                s.push_str("..<UTF8 ERR>");
                Ok(s)
            }
            // we need to fully reconstruct to change it's lifetime to 'static
            Err(OpaqueErrorMsgFail::Null)          => Err(OpaqueErrorMsgFail::Null),
            Err(OpaqueErrorMsgFail::Error(e)) => Err(OpaqueErrorMsgFail::Error(e)),
        };

        OpaqueErrorOwned { errcode: self.errcode, message, misc: self.misc }
    }

    fn with_misc(mut self, misc: u16) -> Self {
        self.misc = misc;
        self
    }
}

impl OpaqueErrorOwned {
    pub fn as_ref(&self) -> OpaqueError<'_> {
        OpaqueError {
            errcode: self.errcode,
            message: match &self.message {
                Ok(s) => Ok(&s),
                Err(e) => Err(*e),
            },
            misc: 0, // OpaqueErrorOwned is only returned by create/destroy, which do not use misc.
        }
    }
}

pub enum LoadError {
    LibLoading(libloading::Error),
    MagicMismatch(u32),
    MajorVersionMismatch(u8, u8),
    MinorVersionMismatch(u8, u8),
    NullVtable,
    NullHandle,
    Opaque(OpaqueErrorOwned),
}

impl From<libloading::Error> for LoadError { fn from(e: libloading::Error) -> Self { LoadError::LibLoading(e) } }
impl From<OpaqueErrorOwned>  for LoadError { fn from(e: OpaqueErrorOwned)  -> Self { LoadError::Opaque(e)     } }

impl Sensor {
    fn errmsg_(vtable: &NonNull<ABI>, errcode: u32) -> OpaqueError<'_> { unsafe {
        let handle = std::ptr::null_mut();
        let mut msg: *const u8 = std::ptr::null_mut();
        let mut len = 0;
        let ret = (vtable.as_ref().message)(handle, errcode, &mut msg, &mut len);

        let message = if ret != 0 {
            Err(OpaqueErrorMsgFail::Error(ret))
        } else if msg.is_null() {
            Err(OpaqueErrorMsgFail::Null)

        } else {
            let msg = std::slice::from_raw_parts(msg, len as usize);
            match std::str::from_utf8(msg) {
                Err(utf8err) => {
                    let len = utf8err.valid_up_to();
                    let msg = std::str::from_utf8_unchecked(&msg[..len]);
                    Err(OpaqueErrorMsgFail::NotUtf8(msg))
                }
                Ok(msg) => Ok(msg),
            }
        };

        OpaqueError { errcode, message, misc: 0, }
    }}

    // Returned OpaqueError keeps lifetime of &mut self.
    // If the returned message kept alive, the caller won't be able to make another call.
    // Error messages returned by vtable.message will persist until the next call to message, making this safe.
    fn errmsg(&mut self, errcode: u32) -> OpaqueError<'_> {
        Self::errmsg_(&self.vtable, errcode)
    }

    pub fn new(path: impl AsRef<Path>) -> Result<Self, LoadError> { unsafe {
        let lib = Library::new(path.as_ref().as_os_str())?;
        let get_vtable: Symbol<fn() -> *const ABI> = lib.get(b"get_vtable")?;

        let vtable= get_vtable();
        let vtable = NonNull::new(vtable as *mut ABI)
        .ok_or(LoadError::NullVtable)?;

        if vtable.as_ref().magic != API_MAGIC { return Err(LoadError::MagicMismatch(vtable.as_ref().magic)); }

        if vtable.as_ref().version_major != MYTHIC_VERSION.major { return Err(LoadError::MajorVersionMismatch(vtable.as_ref().version_major, MYTHIC_VERSION.major)); }
        if vtable.as_ref().version_minor >  MYTHIC_VERSION.minor { return Err(LoadError::MinorVersionMismatch(vtable.as_ref().version_minor, MYTHIC_VERSION.minor)); }

        let mut handle = std::ptr::null_mut();
        let errcode = (vtable.as_ref().create)(&mut handle);
        if errcode != 0 {
            Err(Self::errmsg_(&vtable, errcode).to_owned())?;
        }

        let handle = NonNull::new(handle)
            .ok_or(LoadError::NullHandle)?;

        Ok(Self { _lib: lib, vtable, handle, destroyed: false })
    }}

    #[inline]
    pub fn read(&self, sid: u16) -> f64 { unsafe {
        let ptr = self.handle.as_ptr() as *const f64;
        let val = ptr.add((sid & 0xFF) as usize);
        *val
    }}

    pub fn register(&mut self, path: &str) -> Result<u16, OpaqueError<'_>> { unsafe {
        let mut sid: u16 = 0;
        let errcode = (self.vtable.as_ref().register)(self.handle.as_ptr(), path.as_ptr(), path.len() as u64, &mut sid);
        log::trace!("registerd {path} with sid={sid} errcode={errcode}");
        if errcode != 0 {
            Err(self.errmsg(errcode).with_misc(sid))
        } else { Ok(sid) }
    }}

    pub fn unregister(&mut self, sid: u16) -> Result<(), OpaqueError<'_>> { unsafe {
        let errcode = (self.vtable.as_ref().unregister)(self.handle.as_ptr(), sid);
        if errcode != 0 {
            Err(self.errmsg(errcode))
        } else { Ok(()) }
    }}

    pub fn refresh(&mut self) -> Result<(), OpaqueError<'_>> { unsafe {
        let errcode = (self.vtable.as_ref().refresh)(self.handle.as_ptr());
        if errcode != 0 {
            Err(self.errmsg(errcode))
        } else { Ok(()) }
    }}

    fn destroy(&mut self) -> Result<(), OpaqueErrorOwned> { unsafe {
        let errcode = (self.vtable.as_ref().destroy)(self.handle.as_ptr());
        if errcode != 0 {
            Err(self.errmsg(errcode).to_owned())
        } else { Ok(()) }
    }}

    // destroy is only accessible via drop. prevents use-after-destroy
    pub fn drop(mut self) -> Result<(), OpaqueErrorOwned> {
        self.destroy()
    }
}

impl Drop for Sensor {
    fn drop(&mut self) {
        if ! self.destroyed {
            panic!("Sensor must be dropped manually");
        }
    }
}