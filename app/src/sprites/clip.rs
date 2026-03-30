use libwebp_sys::*;
use std::{fs, mem, ptr};
use crate::base::{AutoSize, AppPath};

/// A single decoded frame of animation
struct Frame {
    pub pixels: Vec<u8>,   // BGRA, premultiplied alpha, canvas-sized
    pub delay_ms: u32,     // duration for this frame
}

pub struct Clip {
    path: AppPath,
    frames: Vec<Frame>,
    width: usize,
    height: usize,
    pub loop_count: u32,   // 0 = infinite

    // /// LRU cache of decoded frames (frame_index -> Frame)
    // frame_cache: lru::LruCache<u32, Frame>,
    // /// Compressed webp data in memory
    // compressed_data: Vec<u8>,
}

unsafe fn scale_bilinear(
    src:  *const u8,
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) -> Vec<u8> {
    let mut dst = vec![0u8; dst_w * dst_h * 4];

    let x_ratio = src_w as f32 / dst_w as f32;
    let y_ratio = src_h as f32 / dst_h as f32;

    for y in 0..dst_h {
        let sy = y as f32 * y_ratio;
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(src_h - 1);
        let fy = sy - y0 as f32;

        for x in 0..dst_w {
            let sx = x as f32 * x_ratio;
            let x0 = sx.floor() as usize;
            let x1 = (x0 + 1).min(src_w - 1);
            let fx = sx - x0 as f32;

            for c in 0..4 {
                let p00 = *src.add((y0 * src_w + x0) * 4 + c) as f32;
                let p10 = *src.add((y0 * src_w + x1) * 4 + c) as f32;
                let p01 = *src.add((y1 * src_w + x0) * 4 + c) as f32;
                let p11 = *src.add((y1 * src_w + x1) * 4 + c) as f32;

                let v0 = p00 + (p10 - p00) * fx;
                let v1 = p01 + (p11 - p01) * fx;
                let v = v0 + (v1 - v0) * fy;

                dst[(y * dst_w + x) * 4 + c] = v as u8;
            }
        }
    }

    dst
}

#[derive(Debug)]
pub enum ClipLoadError {
    CannotRead(std::io::Error),
    WebPAnimDecoderNew,
    WebPAnimDecoderGetInfo,
    WebPAnimDecoderGetNext,
}

impl Clip {
    pub fn load_webp(path: &AppPath, size: AutoSize) -> Result<Clip, ClipLoadError>  { unsafe {
        let data = fs::read(&path).map_err(ClipLoadError::CannotRead)?;

        let webp_data = WebPData {
            bytes: data.as_ptr(),
            size: data.len(),
        };

        let dec_opts = WebPAnimDecoderOptions {
            color_mode: WEBP_CSP_MODE::MODE_RGBA,
            ..mem::zeroed()
        };

        let dec = WebPAnimDecoderNew(&webp_data, &dec_opts);
        if dec.is_null() { return Err(ClipLoadError::WebPAnimDecoderNew); }

        let mut info: WebPAnimInfo = mem::zeroed();
        let ret = WebPAnimDecoderGetInfo(dec, &mut info);
        if ret == 0 { return Err(ClipLoadError::WebPAnimDecoderGetInfo); }

        let orig_width = info.canvas_width as usize;
        let orig_height = info.canvas_height as usize;

        let (width, height) = size.complete(orig_width, orig_height);

        let mut frames = Vec::with_capacity(info.frame_count as usize);

        let mut timestamp_prev = 0;
        let mut buf: *mut u8 = ptr::null_mut();
        let mut timestamp = 0;

        while WebPAnimDecoderHasMoreFrames(dec) != 0 {
            let ret = WebPAnimDecoderGetNext(dec, &mut buf, &mut timestamp);
            if ret == 0 { return Err(ClipLoadError::WebPAnimDecoderGetNext); }

            let delay = (timestamp - timestamp_prev) as u32;
            timestamp_prev = timestamp;

            let mut pixels = if orig_width == width && orig_height == height {
                let size = orig_width * orig_height * 4;
                let mut pixels = Vec::with_capacity(size);
                pixels.set_len(size);
                ptr::copy_nonoverlapping(buf, pixels.as_mut_ptr(), size);
                pixels

            } else {
                scale_bilinear(buf, orig_width, orig_height, width, height)
            };

            for pixel in pixels.chunks_exact_mut(4) {
                let r = pixel[0] as u32;
                let g = pixel[1] as u32;
                let b = pixel[2] as u32;
                let a = pixel[3] as u32;

                // pre-multiply alpha
                let r = (r * a + 127) / 255; // + 127 for rounded division
                let g = (g * a + 127) / 255;
                let b = (b * a + 127) / 255;

                // bgra channel layout
                pixel[0] = b as u8;
                pixel[1] = g as u8;
                pixel[2] = r as u8;
            }

            frames.push(Frame {
                pixels,
                delay_ms: delay,
            });
        }

        WebPAnimDecoderDelete(dec);

        Ok(Clip {
            path: path.clone(),
            frames,
            loop_count: info.loop_count,
            width, height,
            // frame_cache: lru::LruCache::new(std::num::NonZeroUsize::new(3).unwrap()),
        })
    }}

    pub fn get_delay (&self, frame_idx: usize) -> u32      { self.frames[frame_idx].delay_ms }
    pub fn get_pixels(&self, frame_idx: usize) -> &Vec<u8> { &self.frames[frame_idx].pixels }

    pub fn get_width (&self) -> usize { self.width }
    pub fn get_height(&self) -> usize { self.height }
    pub fn len(&self) -> usize { self.frames.len() }
}

impl std::fmt::Debug for Clip {
    fn fmt(&self, f :&mut std::fmt::Formatter) -> std::fmt::Result {
        let dur: u32 = self.frames.iter().map(|f| f.delay_ms).sum();
        write!(f, "<Clip {}x{} px ~{:.2}s/{} x{}, ", self.width, self.height, dur as f32 / 1000.0, self.frames.len(), self.loop_count)?;

        let bytes = self.frames.len() * self.width * self.height * 4;

        let digits = if bytes == 0 {0} else {bytes.ilog(1024)} as usize;
        let prefixes = [ "", "k", "M", "G", ];
        let (div, prefix) = if digits < prefixes.len() {(digits, prefixes[digits])} else {(3, "G")};
        write!(f, "{}{}B>", bytes/1024usize.pow(div as u32), prefix)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipId(usize);

pub struct ClipBank {
    clips: Vec<Option<(Clip, u32)>> // none if unloaded, (clip, rc)
}

#[must_use]
pub struct ReloadIter<'a, I: Iterator<Item=(usize, &'a mut (Clip, u32))>> {
    iter: I,
}

impl<'a, I> Iterator for ReloadIter<'a, I> where I: Iterator<Item=(usize, &'a mut (Clip, u32))> {
    type Item = (ClipId, Option<ClipLoadError>);

    fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|(idx, (clip, _rc))| {
            match Clip::load_webp(&clip.path, AutoSize::new(Some(clip.width), Some(clip.height))) {
                Ok(new_clip) => {
                    *clip  = new_clip;
                    (ClipId(idx), None)
                }
                Err(e) => {
                    (ClipId(idx), Some(e))
                }
            }
        })
    }
}

impl<'a, I> Drop for ReloadIter<'a, I> where I: Iterator<Item=(usize, &'a mut (Clip, u32))> {
    fn drop(&mut self) {
        self.for_each(drop);
    }
}


impl ClipBank {
    pub fn new() -> Self {
        Self { clips: vec![] }
    }

    pub fn get(&self, clipid: ClipId) -> &Clip {
        let Some((clip, _)) = &self.clips[clipid.0] else {
            log::error!("tried to get freed clip with {clipid:?}");
            panic!("tried to get freed clip with {clipid:?}");
        };
        clip
    }

    pub fn load(&mut self, path: &AppPath, size: AutoSize) -> Result<ClipId, ClipLoadError> {
        if let Some((idx,_,rc)) = self.clips.iter_mut().enumerate()
        .filter_map(|(i,o)| o.as_mut().map(|(c,r)| (i,c,r)))
        .find(|(_,c,_)|
            &c.path == path && match (size.width, size.height) {
                (None, None) => true,
                (Some(w), None) => w == c.width,
                (None, Some(h)) => h == c.height,
                (Some(w), Some(h)) => w == c.width && h == c.height,
            }
        ) {
            let clipid = ClipId(idx);

            log::debug!("clipbank.load('{}', {:?}, {:?}) => {:?} using cached, rc: {} -> {}", path, size.width, size.height, clipid, rc, *rc+1);
            *rc += 1;
            return Ok(clipid);
        }

        let clip = Some((Clip::load_webp(path, size)?, 1));

        let idx = if let Some(idx) = self.clips.iter().position(|o| o.is_none()) {
            self.clips[idx] = clip;
            idx
        } else {
            self.clips.push(clip);
            self.clips.len()-1
        };

        let clipid = ClipId(idx);
        log::debug!("clipbank.load('{}', {:?}, {:?}) => {:?} adding new, rc: {} -> {}", path, size.width, size.height, clipid, 0, 1);
        Ok(clipid)
    }

    pub fn unload(&mut self, clipid: ClipId) {
        let Some((_, rc)) = &mut self.clips[clipid.0] else {
            log::error!("tried to unload freed clip with {clipid:?}, report the dev");
            return;
        };

        log::debug!("clipbank.unload({clipid:?}), rc: {} -> {}", rc, *rc-1);
        *rc -= 1;

        if *rc == 0 {
            self.clips[clipid.0] = None;
        }
    }

    /// Reload all the clips that are loaded from 'path'.
    /// Returned iterator gives the clipids of updated clips, acompanied with possible clip load error.
    /// The reloading process is on-the-go; clips get reloaded as the ReloadIter iterator is consumed.
    /// If droped, the iterator will consume itself guraranteeing reload of all relevant clips.
    pub fn reload<'path>(&mut self, path: &'path AppPath) -> ReloadIter<'_, impl Iterator<Item=(usize, &'_ mut (Clip, u32))> + use<'_, 'path>> {
        let iter = self.clips.iter_mut().enumerate()
        .filter_map(|(i,e)| e.as_mut().map(|e| (i,e)))
        .filter(move |e| &e.1.0.path == path);
        ReloadIter { iter }
    }
}