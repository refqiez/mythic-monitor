// #![no_std]

use std::ffi::{c_void, c_float};
use std::time::{Instant, Duration};

struct Handle {
    value: Duration,
    rc: u32,
    init_time: Instant,
}

unsafe extern "C" fn create() -> *mut c_void {
    let cache = Handle {
        value: Duration::new(0, 0),
        rc: 0,
        init_time: Instant::now(),
    };

    Box::into_raw(Box::new(cache)) as *mut c_void
}

unsafe extern "C" fn destroy(handle: *mut c_void) {
    if !handle.is_null() {
        drop(Box::from_raw(handle as *mut Handle));
    }
}

unsafe extern "C" fn refresh(handle: *mut c_void) {
    let handle = &mut *(handle as *mut Handle);
    if handle.rc > 0 {
        handle.value = handle.init_time.elapsed();
    }
}

unsafe extern "C" fn read(handle: *mut c_void, sid: u16) -> c_float {
    let handle = &mut *(handle as *mut Handle);
    if sid == 1 {
        handle.value.as_nanos() as c_float
    } else {
        0.0
    }
}

unsafe extern "C" fn register(handle: *mut c_void, path: *const u8, len: usize) -> u16 {
    let handle = &mut *(handle as *mut Handle);
    if str_eq(path, len, "value") {
        handle.rc += 1;
        1
    } else {
        0
    }
}

unsafe extern "C" fn unregister(handle: *mut c_void, sid: u16) {
    let handle = &mut *(handle as *mut Handle);
    if sid == 1 {
        handle.rc -= 1;
    }
}

unsafe fn str_eq(s1: *const u8, n1: usize, s2: &str) -> bool {
    if s2.len() != n1 { return false; }
    let s2 = s2.as_ptr();

    (0 .. n1).all(|i| *s1.add(i) == *s2.add(i))
}


// boilerplate

const ABI_MAGIC: u32 = 0x5ABAD0B1;
const VERSION_MAJOR: u8 = 1;
const VERSION_MINOR: u8 = 0;

#[repr(C)]
pub struct ABI {
    pub magic: u32,
    pub version_major: u8,
    pub version_minor: u8,
    pub create:     unsafe extern "C" fn() -> *mut c_void,
    pub destroy:    unsafe extern "C" fn(*mut c_void),
    pub refresh:    unsafe extern "C" fn(*mut c_void),
    pub read:       unsafe extern "C" fn(*mut c_void, u16) -> c_float,
    pub register:   unsafe extern "C" fn(*mut c_void, *const u8, usize) -> u16,
    pub unregister: unsafe extern "C" fn(*mut c_void, u16),
}

static VTABLE: ABI = ABI {
    magic: ABI_MAGIC,
    version_major: VERSION_MAJOR,
    version_minor: VERSION_MINOR,
    create,
    destroy,
    refresh,
    read,
    register,
    unregister,
};

#[no_mangle]
pub fn get_vtable() -> *const ABI {
    &VTABLE
}