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
    size: (usize, usize),
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

        let mut ret = WindowState {
            hwnd,
            hdc_mem,
            alive: true,

            dib: Default::default(),
            bits: Default::default(),
            size: (0, 0),
        };

        ret.realloc_bitmap(width, height)?;
        Ok(ret)
    }

    pub fn realloc_bitmap(&mut self, width: usize, height: usize) -> Result<()> { unsafe {
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
            Some(self.hdc_mem),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )?;

        let old_bitmap = SelectObject(self.hdc_mem, dib.into());
        if !old_bitmap.is_invalid() {
            let ret = DeleteObject(old_bitmap as _);
            if ! ret.as_bool() {
                log::warn!("DeleteObject failed");
            }
        }

        self.size = (width, height);
        self.bits = bits as *mut u8;
        self.dib = dib;
        Ok(())
    }}

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


use crate::sprites::{ClipBank, Sprites};
use super::super::{AnimatorUpdate, DecoderUpdate};
use crate::sensing::Sensors;

use std::collections::BinaryHeap;
use std::{mem, ptr, sync};
use std::time::{Instant, Duration};
use core::cmp::Reverse as R;

pub struct Animator {
    windows: Vec<WindowState>,

    class: PCWSTR,
    wc: WNDCLASSW,
    hdc_screen: HDC,

    sprites: sync::Arc<sync::RwLock<Sprites>>,
    sensors: sync::Arc<sync::RwLock<Sensors>>,
    clipbank: sync::Arc<sync::RwLock<ClipBank>>,
    queue: BinaryHeap<R<(Instant, usize)>>,
    update_queue: sync::mpsc::Receiver<AnimatorUpdate>,
    decoder_update_queue: sync::mpsc::Sender<DecoderUpdate>,
}

const BLEND: BLENDFUNCTION = BLENDFUNCTION {
    BlendOp: AC_SRC_OVER as u8,
    BlendFlags: 0,
    SourceConstantAlpha: 255,
    AlphaFormat: AC_SRC_ALPHA as u8,
};

impl Animator {
    pub fn init(
        sprites: sync::Arc<sync::RwLock<Sprites>>,
        sensors: sync::Arc<sync::RwLock<Sensors>>,
        clipbank: sync::Arc<sync::RwLock<ClipBank>>,
        update_queue: sync::mpsc::Receiver<AnimatorUpdate>,
        decoder_update_queue: sync::mpsc::Sender<DecoderUpdate>,
    ) -> Result<Self> { unsafe {
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
            windows: vec![],
            class, wc, hdc_screen,

            sprites, sensors, clipbank,
            queue: BinaryHeap::new(),
            update_queue, decoder_update_queue,
        })
    }}

    fn apply_sprite_updates(&mut self) {
        let need_update = self.sprites.read().expect("poisoned sprites lock").is_pending_update_present();

        if ! need_update { return; }

        let mut sprites = self.sprites.write().expect("poisoned sprites lock");
        let mut sensors = self.sensors.write().expect("poisoned sensors lock");
        let mut clipbank = self.clipbank.write().expect("poisoned clipbank lock");
        let Some(new_indices) = sprites.apply_sprite_updates(&mut sensors, &mut clipbank) else { return };

        let mut updated = vec![false; sprites.len()];

        // relocate queue

        let mut new_queue = BinaryHeap::new();
        for R((instant, sprite_idx)) in self.queue.as_slice() { // BinaryHeap::as_mut_slice() is yet unstable
            let Some(new_idx) = new_indices[*sprite_idx] else { continue };
            updated[new_idx] = true;
            new_queue.push(R((*instant, new_idx)));
        }

        for (new_sprite_idx, _) in updated.iter().enumerate().filter(|(_,b)| !**b) {
            // for sprites that are not ssen in the queue, these are newly added ones
            new_queue.push(R((Instant::now(), new_sprite_idx)));
        }

        self.queue = new_queue;

        // relocate windows

        unsafe {
            let mut new_windows = vec![];
            for _ in 0 .. sprites.len() { new_windows.push(mem::zeroed()); }
            for (_sprite_idx, (new_idx, ws)) in new_indices.iter().zip(self.windows.drain(..)).enumerate() {
                if let Some(new_idx) = new_idx {
                    new_windows[*new_idx] = ws;
                } else {
                    if let Err(e) = ws.destroy() {
                        log::error!("{}", e.message());
                    }
                }
            }
            for (new_sprite_idx, _) in updated.iter().enumerate().filter(|(_,b)| !**b) {
                let (width, height) = sprites.get_bounding_size(new_sprite_idx, &clipbank);
                let ws = match WindowState::new(self.class, self.wc, self.hdc_screen, width, height) {
                    Ok(ws) => ws,
                    Err(e) => {
                        log::error!("{}", e.message());
                        panic!();
                    }
                };
                new_windows[new_sprite_idx] = ws;
            }
        }
    }

    fn process_update(&mut self, upd: AnimatorUpdate) -> Result<()> {
        log::debug!("processing window update message {upd:?}");

        match upd {
            AnimatorUpdate::UpdateQueued => {
                self.apply_sprite_updates();
                _ = self.decoder_update_queue.send(DecoderUpdate::Rescan);
            }
        }

        Ok(())
    }

    fn redraw_sprite(&mut self, sprite_idx: usize) -> Result<Option<u32>> {
        let mut sprites = self.sprites.write().expect("poisoned sprites lock");
        let mut sensors = self.sensors.write().expect("poisoned sensors lock");
        let mut clipbank = self.clipbank.write().expect("poisoned clipbank lock");
        let frame = sprites.get_current_frame(sprite_idx, &clipbank);

        let ws = &mut self.windows[sprite_idx];
        let (width, height) = frame.size;
        let (x, y) = frame.pos;

        if ws.size.0 < width || ws.size.1 < height {
            let (w, h) = sprites.get_bounding_size(sprite_idx, &clipbank);
            ws.realloc_bitmap(w, h)?;
        }

        let size = SIZE {
            cx: width  as i32,
            cy: height as i32,
        };
        let src = POINT { x: 0, y: 0 };
        let dst = POINT { x: x, y: y };

        unsafe {
            ptr::copy_nonoverlapping(
                frame.pixels.as_ptr(),
                ws.bits,
                frame.pixels.len(),
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
            )?;
        }

        let delay = frame.delay_ms;
        // drop(frame) to mutably borrow clipbank

        sprites.advance(sprite_idx, &mut sensors, &mut clipbank);
        if let Some(clipid) = sprites.get_current_clipid(sprite_idx) {
            _ = self.decoder_update_queue.send(DecoderUpdate::Advanced(clipid));
        }

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
                None => Duration::from_secs(1), // would check for update_queue at every second
            };

            // log::trace!("wait_duration: {wait_duration:?}");
            match self.update_queue.recv_timeout(wait_duration) {
                Ok(upd) =>
                    self.process_update(upd)?,

                Err(std::sync::mpsc::RecvTimeoutError::Timeout) =>
                    self.handle_due_redraws()?,

                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }

            self.apply_sprite_updates();
        }

        log::info!("WindowsRednerer exiting");
        Ok(())
    }
}

impl Drop for Animator {
    fn drop(&mut self) {
        for ws in self.windows.drain(..) {
            if let Err(e) = ws.destroy() {
                log::error!("error while destroying window ({e})");
            }
        }
    }
}