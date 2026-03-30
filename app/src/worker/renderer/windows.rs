use std::{mem, ptr};
use std::time::{Instant, Duration};
use core::cmp::Reverse as R;

use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{
            LibraryLoader::GetModuleHandleW,
        },
        UI::WindowsAndMessaging::*,
    },
};

struct WindowState {
    hwnd: HWND,
    hdc_mem: HDC,
    dib: HBITMAP,
    bits: *mut u8,
    alive: bool,
}

impl WindowState {
    pub unsafe fn new(class: PCWSTR, wc: WNDCLASSW, hdc_screen: HDC, width: usize, height: usize) -> Result<Self> {
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_TOPMOST
                | WS_EX_NOACTIVATE,
            class,
            w!(""),
            WS_POPUP,
            // these values doesn't matter. layered window's position and size are determined by
            // UpdateLayerdWindow pptdst (pos), psize (width/height)
            0, 0, 10, 10,
            None,
            None,
            Some(wc.hInstance),
            None,
        )?;

        // Windows api has bug. usually return value of 0 indicates success, while
        // BOOL.ok() return Err when non-zero. ShowWindow should not return BOOL, it should be HRESULT.
        // ShowWindow(hwnd, SW_SHOW).ok()?;
        let ret = ShowWindow(hwnd, SW_SHOW);
        assert_eq!(ret.0, 0);

        // Creates a memory DC compatible with the screen.
        // This is:
        // - An off-screen drawing surface
        // - Where your frame pixels live
        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: - (height as i32), // negative for top-down
                biPlanes: 1,
                biBitCount: 32, // we have RGBA 4 byte pixels
                biCompression: BI_RGB.0, // uncompressed raw pixels
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits = ptr::null_mut();
        let dib = CreateDIBSection(
            Some(hdc_mem),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )?;

        SelectObject(hdc_mem, dib.into());

        Ok(WindowState {
            hwnd,
            hdc_mem,
            dib,
            bits: bits as *mut u8,
            alive: true,
        })
    }

    pub fn destroy(mut self) -> Result<()> { unsafe {
        DestroyWindow(self.hwnd)?;
        // we should have saved original bitmat when selecting to swap back at this point
        // but deleting selected bitmap usually cause no problem.
        // SelectObject(self.hdc_mem, GetStockObject(old_bitmap));
        DeleteObject(self.dib.into()).ok()?;
        DeleteDC(self.hdc_mem).ok()?;
        self.alive = false;
        Ok(())
    }}
}

impl Drop for WindowState {
    // drop cannot return error result, so we demand calling detroy explicitly.
    fn drop(&mut self) {
        if self.alive {
            panic!("WindowState::destroy() must be called before it being dropped");
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

use std::collections::{HashMap, BinaryHeap};

use super::RenderDataInterface;
use crate::base::Align;
use crate::sprites::SpriteId;
use crate::worker::watcher::{WindowUpdate, WindowUpdateKind};

pub struct Renderer {
    windows: HashMap<SpriteId, WindowState>,

    class: PCWSTR,
    wc: WNDCLASSW,
    hdc_screen: HDC,

    interface: RenderDataInterface,
    queue: BinaryHeap<R<(Instant, SpriteId)>>
}

const BLEND: BLENDFUNCTION = BLENDFUNCTION {
    BlendOp: AC_SRC_OVER as u8,
    BlendFlags: 0,
    SourceConstantAlpha: 255,
    AlphaFormat: AC_SRC_ALPHA as u8,
};

impl Renderer {
    pub fn init(interface: RenderDataInterface) -> Result<Self> { unsafe {
        let class = w!("webp_overlay");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            // HINSTANCE vs HMODULE https://m.blog.naver.com/kimsw3446/100184970052
            hInstance: HINSTANCE::from(GetModuleHandleW(PCWSTR::null())?),
            lpszClassName: class,
            ..Default::default()
        };

        RegisterClassW(&wc);

        // Gets a device context for the entire screen.
        // - Used as the destination DC
        // - Required by UpdateLayeredWindow
        // You are not drawing to this DC — it's a reference for the compositor.
        let hdc_screen = GetDC(None);

        Ok(Self {
            windows: HashMap::new(),
            class, wc, hdc_screen,

            interface,
            queue: BinaryHeap::new(),
        })
    }}

    fn process_update(&mut self, upd: WindowUpdate) -> Result<()> {
        log::debug!("processing window update message {upd:?}");
        match upd.kind {
            WindowUpdateKind::Create => {
                let (width, height) = self.interface.with_frame(upd.spriteid, |_, frame| (frame.width(), frame.height())).unwrap();
                let ws = unsafe { WindowState::new(self.class, self.wc, self.hdc_screen, width, height)? };
                if let Some(ws) = self.windows.insert(upd.spriteid, ws) {
                    log::error!("tried to insert twice with existing sprite_id = {:?}", upd.spriteid);
                    if let Err(e) = ws.destroy() {
                        log::error!("error while destroying window ({e})");
                    }
                }

                self.queue.push(R((Instant::now(), upd.spriteid)));
            }

            WindowUpdateKind::Delete => {
                if let Some(ws) = self.windows.remove(&upd.spriteid) {
                    if let Err(e) = ws.destroy() {
                        log::error!("error while destroying window ({e})");
                    }
                } else {
                    log::error!("tried to delete non-existing sprite with sprite_id = {:?}", upd.spriteid);
                }
                self.queue.retain(|R((_,spriteid))| *spriteid != upd.spriteid);
            }

            // WindowUpdate::ModSize => { }
            // WindowUpdate::ModBuffer => { }
        }

        Ok(())
    }

    fn redraw_sprite(&mut self, spriteid: SpriteId) -> Result<Option<u32>> {

        pub fn start_pos((align, pos): (Align, i32), stretch: i32) -> i32 {
            match align {
                Align::Start => pos,
                Align::Center => pos - stretch/2,
                Align::End => pos - stretch,
            }
        }

        let Some(delay): Option<Result<u32>> = self.interface.with_frame(spriteid, |sprite, frame| unsafe {
            let ws= self.windows.get_mut(&spriteid).unwrap();
            let width = frame.width();
            let height = frame.height();

            let x = start_pos(sprite.xpos, width as i32);
            let y = start_pos(sprite.ypos, height as i32);

            let size = SIZE {
                cx: frame.width()  as i32,
                cy: frame.height() as i32,
            };
            let src = POINT { x: 0, y: 0 };
            let dst = POINT { x: x, y: y };

            let pixels = frame.pixels();
            ptr::copy_nonoverlapping(
                pixels.as_ptr(),
                ws.bits,
                pixels.len(),
            );

            UpdateLayeredWindow(
                ws.hwnd,
                Some(self.hdc_screen),
                Some(&dst),
                Some(&size),
                Some(ws.hdc_mem),
                Some(&src),
                COLORREF(0),
                Some(&BLEND),
                ULW_ALPHA,
            ).map(|_| frame.delay())
            // Ok(frame.delay())
        }) else {
            // with_frame returns None when the sprite does not exist or don't have loaded SpriteController.
            // The former would happen on late delivery of Delete event.
            // The later indicate some bugs in the code.
            // Either case, we ignore and return.
            return Ok(None);
        };
        let delay  = delay?;

        self.interface.advance(spriteid);

        Ok(Some(delay))
    }

    fn handle_due_redraws(&mut self) -> Result<()> {
        let now = Instant::now();

        while let Some(R((next_run, _))) = self.queue.peek() {
            if next_run > &now {
                break;
            }

            let R((next_run, spriteid)) = self.queue.pop().unwrap();

            if let Some(snooze) = self.redraw_sprite(spriteid)? {
                self.queue.push(R((
                    next_run + Duration::from_millis(snooze as u64),
                    spriteid,
                )));
            }
        }

        Ok(())
    }

    pub fn run(&mut self) -> Result<()> {
        log::info!("WindowsRednerer started runing");
        loop {
            // log::trace!("window update iteration");

            let now = Instant::now();
            let wait_duration = match self.queue.peek() {
                Some(R((next_run, _spriteid))) => {
                    if next_run <= &now {
                        Duration::ZERO
                    } else {
                        next_run.duration_since(now)
                    }
                }
                None => Duration::from_secs(1),
            };

            // log::trace!("wait_duration: {wait_duration:?}");
            match self.interface.update_queue.recv_timeout(wait_duration) {
                Ok(upd) =>
                    self.process_update(upd)?,

                Err(std::sync::mpsc::RecvTimeoutError::Timeout) =>
                    self.handle_due_redraws()?,

                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        log::info!("WindowsRednerer exiting");
        Ok(())
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        for (_, ws) in self.windows.drain() {
            if let Err(e) = ws.destroy() {
                log::error!("error while destroying window ({e})");
            }
        }
    }
}