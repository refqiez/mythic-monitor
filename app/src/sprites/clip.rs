use libwebp_sys::*;
use std::{fs, mem, ptr, sync, slice, ops};
use std::collections::{HashMap, hash_map};
use std::sync::OnceLock;

use crate::base::{AutoSize, AppPath};

#[derive(Debug)]
pub enum ClipLoadError {
    CannotRead(std::io::Error),
    WebPAnimDecoderOptionsInit,
    WebPAnimDecoderNew,
    WebPAnimDecoderGetInfo,
    WebPAnimDecoderGetNext,
}

/// A single decoded frame of animation
pub struct Frame<'a> {
    pub pixels: &'a [u8],   // BGRA, premultiplied alpha, canvas-sized
    pub delay_ms: u32,   // duration for this frame
    pub size: (usize, usize),
    pub pos: (i32, i32),
}

impl<'a> std::fmt::Display for Frame<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Frame {{ pixels: [..; {}], delay_ms: {}, size: {:?} pos: {:?} }}", self.pixels.len(), self.delay_ms, self.size, self.pos)
    }
}

static WEBP_DATA_ERROR:   &[u8] = include_bytes!("../../../res/error.webp");
static WEBP_DATA_LOADING: &[u8] = include_bytes!("../../../res/loading.webp");

static WEBP_FRAME_ERROR: OnceLock<(Vec<u8>,usize,usize)> = OnceLock::new();
static WEBP_FRAME_LOADING: OnceLock<(Vec<u8>,usize,usize)> = OnceLock::new();

fn decode_static_frame(data: &[u8]) -> Option<(Vec<u8>, usize, usize)> { unsafe {
    let (mut width, mut height) = (0, 0);
    let ret = WebPGetInfo(data.as_ptr(), data.len(), &mut width, &mut height);
    assert!(ret > 0);
    let (width, height) = (width as usize, height as usize);

    let buffer_size = width * height * 4;
    let mut pixels = vec![0u8; buffer_size];
    let ret = WebPDecodeBGRAInto(data.as_ptr(), data.len(), pixels.as_mut_ptr(), buffer_size, width as i32 * 4);
    if ret.is_null() { None } else { Some((pixels, width, height)) }
}}

impl Frame<'static> {
    fn decode_and_init(global: &'static OnceLock<(Vec<u8>, usize, usize)>, data: &[u8], delay_ms: u32, pos: (i32, i32)) -> Self {
        // let buffer = WEBP_FRAME_ERROR.get_or_try_init(|| decode_static_frame(WEBP_DATA_ERROR, 30, 30));
        if global.get().is_none() {
            if let Some(glob) = decode_static_frame(data) {
                _ = global.set(glob);
            }
        };
        let glob = global.get();

        if let Some(glob) = glob {
            Frame { pixels: &glob.0, delay_ms, size: (glob.1,glob.2), pos }
        } else {
            Self::empty(pos)
        }
    }

    pub fn error(pos: (i32, i32)) -> Self {
        Self::decode_and_init(&WEBP_FRAME_ERROR, WEBP_DATA_ERROR, 5000, pos)
    }

    pub fn loading(pos: (i32, i32)) -> Self {
        Self::decode_and_init(&WEBP_FRAME_LOADING, WEBP_DATA_LOADING, 500, pos)
    }

    pub fn empty(pos: (i32, i32)) -> Self {
        Frame { pixels: &[], delay_ms: 5000, size: (0,0), pos }
    }
}

struct WebPAnimDecoderWrapper(ptr::NonNull<WebPAnimDecoder>);

// libwebp is thread safe
unsafe impl Send for WebPAnimDecoderWrapper {}
unsafe impl Sync for WebPAnimDecoderWrapper {}

impl Drop for WebPAnimDecoderWrapper {
    fn drop(&mut self) { unsafe {
        WebPAnimDecoderDelete(self.0.as_mut());
    }}

}

struct PackedWebPData {
    // data source
    path: AppPath,
    data: Vec<u8>, // compressed webp binary

    // metadata
    width: usize,
    height: usize,
    loop_count: u32,
    frame_count: usize,

    webp_data: WebPData,
}

// WebPData prevenst Sync and Send but libwebp functions are thread safe
unsafe impl Send for PackedWebPData {}
unsafe impl Sync for PackedWebPData {}

// Contains all frames, rescaled
struct UnpackedWebPData {
    // data source
    path: AppPath,
    buffer: Vec<u8>, // unpacked frames data
    delays: Vec<u32>,

    // metadata
    width: usize, height: usize, // desired size
    orig_width: usize, orig_height: usize, // original size
    loop_count: u32,
    frame_count: usize,
}

// Provides sequential frames, in desired size
struct ClipStreamed {
    // metadata
    width: usize, height: usize, // desired size
    loop_count: u32, // may have been overridden, 0 = undefinite

    // libwebp data
    packed: sync::Arc<PackedWebPData>,
    decoder: WebPAnimDecoderWrapper,

    // for online decoding
    buffer: Vec<u8>, // circular queue containig decoded frames
    delays: Vec<u32>, // delay_ms of decoded frames
    circ_front_idx: usize, // idx of frame to pop next in frames buffer
    circ_buff_size: usize, // # of vaild elements in circular frames buffer
    rescaled: Option<Vec<u8>>, // only used when lazily rescale
    timestamp_prev: i32,
    current_frame_idx: usize,
}

// Provides sequential frames, in desired size
struct ClipUnpacked {
    loop_count: u32, // may have been overridden, 0 = undefinite
    unpacked: sync::Arc<UnpackedWebPData>,
    current_frame_idx: usize,
}

enum Clip {
    Unpacked(ClipUnpacked),
    Streamed(ClipStreamed),
}

impl PackedWebPData {
    fn load(path: &AppPath) -> Result<Self, ClipLoadError> { unsafe {
        let data = fs::read(&path).map_err(ClipLoadError::CannotRead)?;

        let webp_data = WebPData {
            bytes: data.as_ptr(),
            size: data.len(),
        };

        // libwebp-sys does not export WebPDemux Constructor :/
        // let demux = WebPDemux(&webp_data);
        // let width = WebPDemuxGetI(demux, WebPFormatFeature::WEBP_FF_CANVAS_WIDTH);

        let decoder = WebPAnimDecoderNew(&webp_data, ptr::null());
        if decoder.is_null() { return Err(ClipLoadError::WebPAnimDecoderNew); }

        let mut info: WebPAnimInfo = mem::zeroed();
        let ret = WebPAnimDecoderGetInfo(decoder, &mut info);
        if ret == 0 { return Err(ClipLoadError::WebPAnimDecoderGetInfo); }

        WebPAnimDecoderDelete(decoder);

        Ok(Self {
            path: path.clone(),
            webp_data, data,

            width: info.canvas_width as usize,
            height: info.canvas_height as usize,
            loop_count: info.loop_count,
            frame_count: info.frame_count as usize,
            // info.bg_color
        })
    }}
}

impl UnpackedWebPData {
    fn from(packed: sync::Arc<PackedWebPData>, size: AutoSize) -> Result<Self, ClipLoadError> {
        let mut clip = ClipStreamed::from_packed_webp(packed, size, 0, Some(false), None)?;

        loop {
            match clip.decode_next_frame() {
                Ok(true) => continue,
                Ok(false) => break,
                Err(e) => return Err(e),
            }
        }

        Ok(Self {
            path: clip.packed.path.clone(),
            buffer: clip.buffer,
            delays: clip.delays,
            width:  clip.width,
            height: clip.height,
            orig_width:  clip.packed.width,
            orig_height: clip.packed.height,
            loop_count:  clip.packed.loop_count,
            frame_count: clip.packed.frame_count,
        })
    }
}

impl ClipStreamed {
    fn from_packed_webp(
        packed: sync::Arc<PackedWebPData>,
        size: AutoSize,
        max_decoded_frames_count: usize, // set to 0 to use full-allocation
        lazy_rescale: Option<bool>,
        loop_count_override: Option<u32>,
    ) -> Result<Self, ClipLoadError> { unsafe {
        let mut options: WebPAnimDecoderOptions = mem::zeroed();
        let ret = WebPAnimDecoderOptionsInit(&mut options);
        if ret == 0 {
            return Err(ClipLoadError::WebPAnimDecoderOptionsInit);
        }
        options.color_mode = WEBP_CSP_MODE::MODE_BGRA;

        let decoder = WebPAnimDecoderNew(&packed.webp_data, &options);
        let Some(decoder) = ptr::NonNull::new(decoder) else { return Err(ClipLoadError::WebPAnimDecoderNew) };
        let decoder = WebPAnimDecoderWrapper(decoder);

        // desired size
        let (width, height) = size.complete(packed.width, packed.height);
        let frame_count = packed.frame_count as usize;

        let lazy_rescale = lazy_rescale.unwrap_or(width * height > packed.width * packed.height);
        let frame_size = if lazy_rescale { packed.width * packed.height * 4 } else { width * height * 4 };
        let decoded_frames_count = if max_decoded_frames_count == 0 { frame_count } else { max_decoded_frames_count.min(frame_count) };

        let buffer = vec![0u8; decoded_frames_count * frame_size];
        let delays = vec![0u32; decoded_frames_count];
        let rescaled = if lazy_rescale { Some(vec![0u8; width*height*4]) } else { None };

        let loop_count = loop_count_override.unwrap_or(packed.loop_count);

        Ok(Self {
            circ_front_idx: 0,
            circ_buff_size: 0,

            timestamp_prev: 0,
            current_frame_idx: 0,
            loop_count, width, height,
            buffer, delays, rescaled, packed, decoder,
        })
    }}

    /// returns:
    ///   Ok(true) on success
    ///   Ok(false) on no more frames (rewinded)
    ///   Err(_) on errors
    fn decode_next_frame(&mut self) -> Result<bool, ClipLoadError> { unsafe {
        if WebPAnimDecoderHasMoreFrames(self.decoder.0.as_mut()) == 0 {
            WebPAnimDecoderReset(self.decoder.0.as_mut());
            self.timestamp_prev = 0;
            self.current_frame_idx = 0;
            return Ok(false);
        }

        if self.circ_buff_size >= self.buffer_len() {
            log::debug!("tried to decode a frame beyond frames buffer size, ignoring");
            return Ok(true);
        }

        // Decode a frame

        let mut buf: *mut u8 = ptr::null_mut();
        let mut timestamp = 0;

        let ret = WebPAnimDecoderGetNext(self.decoder.0.as_mut(), &mut buf, &mut timestamp);
        if ret == 0 { return Err(ClipLoadError::WebPAnimDecoderGetNext); }

        let delay = (timestamp - self.timestamp_prev) as u32;
        let circ_back_idx = (self.circ_front_idx + self.circ_buff_size) % self.buffer_len();
        self.delays[circ_back_idx] = delay;
        self.timestamp_prev = timestamp;

        // Copy to frame buffer

        let lazy_rescale = self.rescaled.is_some();
        let frame_size = if lazy_rescale {
            self.packed.width * self.packed.height * 4
        } else {
            self.width * self.height * 4
        };

        let circ_back_offset = (circ_back_idx % self.buffer_len()) * frame_size;
        self.circ_buff_size += 1;

        let dst_buffer = &mut self.buffer[circ_back_offset .. circ_back_offset + frame_size];

        if lazy_rescale {
            ptr::copy_nonoverlapping(buf, dst_buffer.as_mut_ptr(), frame_size);
        } else {
            scale_bilinear(slice::from_raw_parts(buf, frame_size), dst_buffer, self.packed.width, self.packed.height, self.width, self.height);
        }

        // Preapply alpha

        for pixel in dst_buffer.chunks_exact_mut(4) {
            let b = pixel[0] as u32;
            let g = pixel[1] as u32;
            let r = pixel[2] as u32;
            let a = pixel[3] as u32;

            // + 127 for rounded division
            pixel[0] = ((b * a + 127) / 255) as u8;
            pixel[1] = ((g * a + 127) / 255) as u8;
            pixel[2] = ((r * a + 127) / 255) as u8;
        }

        Ok(true)
    }}

    fn calc_lazy_rescale(&mut self) {
        if self.circ_buff_size == 0 { return; }

        let frame_size = self.frame_size_bytes();
        let circ_front_offset = self.circ_front_idx * frame_size;
        let buffer = &self.buffer[circ_front_offset .. circ_front_offset + frame_size];

        let Some(pixels) = self.rescaled.as_mut() else { return };

        let view_frame_size = self.width * self.height * 4;
        pixels.resize(view_frame_size, 0);
        unsafe { scale_bilinear(buffer, pixels, self.packed.width, self.packed.height, self.width, self.height); }
    }

    fn get_current_frame(&self, pos: (i32, i32)) -> Frame<'_> {
        if self.circ_buff_size == 0 { return Frame::loading(pos); }

        let delay = self.delays[self.circ_front_idx];

        let pixels = if let Some(mut pixels) = self.rescaled.as_ref() {
            &pixels
        } else {
            let frame_size = self.frame_size_bytes();
            let circ_front_offset = self.circ_front_idx * frame_size;
            &self.buffer[circ_front_offset .. circ_front_offset + frame_size]
        };

        Frame {
            pixels, delay_ms: delay, pos,
            size: (self.width, self.height),
        }
    }

    fn frame_size_bytes(&self) -> usize {
        let lazy_rescale = self.rescaled.is_some();
        if lazy_rescale {
            self.packed.width * self.packed.height * 4
        } else {
            self.width * self.height * 4
        }
    }

    fn buffer_len(&self) -> usize {
        self.delays.len()
    }

    fn decode_shortfall(&self) -> usize {
        self.buffer_len() - self.circ_buff_size
    }
}

impl ClipUnpacked {
    fn from_unpacked_webp(unpacked: sync::Arc<UnpackedWebPData>, loop_count_override: Option<u32>) -> Result<Self, ClipLoadError> {
        Ok(Self {
            loop_count: loop_count_override.unwrap_or(unpacked.loop_count),
            current_frame_idx: 0,
            unpacked,
        })
    }

    fn get_current_frame(&self, pos: (i32, i32)) -> Frame<'_> {
        let frame_size = self.frame_size_bytes();
        let pixels = &self.unpacked.buffer[self.current_frame_idx * frame_size .. self.current_frame_idx * frame_size + frame_size];
        let delay = self.unpacked.delays[self.current_frame_idx];

        Frame {
            pixels, delay_ms: delay, pos,
            size: (self.unpacked.width, self.unpacked.height),
        }
    }

    fn frame_size_bytes(&self) -> usize {
        self.unpacked.width * self.unpacked.height * 4
    }
}

impl Clip {
    fn path(&self) -> &AppPath {
        match self {
            Clip::Streamed(cs) => &cs.packed.path,
            Clip::Unpacked(cu) => &cu.unpacked.path,
        }
    }

    fn current_frame(&self, pos: (i32, i32)) -> Frame<'_> {
        match self {
            Clip::Streamed(cs) => cs.get_current_frame(pos),
            Clip::Unpacked(cu) => cu.get_current_frame(pos),
        }
    }

    // pub fn len(&self) -> usize {
    //     match self {
    //         Clip::Streamed(cs) => cs.packed.frame_count as usize,
    //         Clip::Unpacked(cu) => cu.unpacked.frame_count as usize,
    //     }
    // }

    /// return if rewinded
    fn advance(&mut self) -> bool {
        match self {
            Clip::Streamed(cs) => {
                if cs.circ_buff_size == 0 {
                    return false; // cannot advance. animation stalls
                }
                cs.circ_buff_size -= 1;
                cs.circ_front_idx += 1;
                cs.circ_front_idx %= cs.buffer_len();

                cs.current_frame_idx += 1;
                cs.current_frame_idx %= cs.packed.frame_count;
                cs.current_frame_idx == 0
            }
            Clip::Unpacked(cu) => {
                cu.current_frame_idx += 1;
                cu.current_frame_idx %= cu.unpacked.frame_count;
                cu.current_frame_idx == 0
            }
        }
    }

    pub fn loop_count(&self) -> u32 {
        match self {
            Clip::Streamed(cs) => cs.loop_count,
            Clip::Unpacked(cu) => cu.loop_count,
        }
    }

    pub fn size(&self) -> (usize, usize) {
        match self {
            Clip::Streamed(cs) => (cs.width, cs.height),
            Clip::Unpacked(cu) => (cu.unpacked.width, cu.unpacked.height),
        }
    }

    pub fn decode_shortfall(&self) -> usize {
        match self {
            Clip::Streamed(cs) => cs.decode_shortfall(),
            Clip::Unpacked(_cu) => 0,
        }
    }

    pub fn decode_next_frame(&mut self) -> Result<bool, ClipLoadError> {
        match self {
            Clip::Streamed(cs) => cs.decode_next_frame(),
            Clip::Unpacked(_cu) => Ok(true),
        }
    }

    pub fn calc_lazy_rescale(&mut self) {
        match self {
            Clip::Streamed(cs) => cs.calc_lazy_rescale(),
            Clip::Unpacked(_cu) => (),
        }
    }

}

impl std::fmt::Debug for ClipStreamed {
    fn fmt(&self, f :&mut std::fmt::Formatter) -> std::fmt::Result {
        let lazy = if self.rescaled.is_some() { "lazy " } else { "" };
        write!(f, "<ClipStreamed {lazy}{}x [[{}x{} px {}frames/{}", self.loop_count, self.packed.width, self.packed.height, self.buffer_len(), self.packed.frame_count)?;

        let bytes = self.packed.frame_count as usize * self.packed.width * self.packed.height * 4;

        let digits = if bytes == 0 {0} else {bytes.ilog(1024)} as usize;
        let prefixes = [ "", "k", "M", "G", ];
        let (div, prefix) = if digits < prefixes.len() {(digits, prefixes[digits])} else {(3, "G")};
        write!(f, "{}{}B", bytes/1024usize.pow(div as u32), prefix)?;
        write!(f, ", {}]] ({})>", self.packed.path, sync::Arc::strong_count(&self.packed))
    }
}

impl std::fmt::Debug for ClipUnpacked {
    fn fmt(&self, f :&mut std::fmt::Formatter) -> std::fmt::Result {
        let duration = self.unpacked.delays.iter().sum::<u32>() as f32 / 1000.0;
        write!(f, "<ClipUnpacked {}x [[{}x{} px ~{duration:.2}s/{}", self.loop_count, self.unpacked.width, self.unpacked.height, self.unpacked.frame_count)?;

        let bytes = self.unpacked.frame_count as usize * self.unpacked.width * self.unpacked.height * 4;

        let digits = if bytes == 0 {0} else {bytes.ilog(1024)} as usize;
        let prefixes = [ "", "k", "M", "G", ];
        let (div, prefix) = if digits < prefixes.len() {(digits, prefixes[digits])} else {(3, "G")};
        write!(f, "{}{}B", bytes/1024usize.pow(div as u32), prefix)?;
        write!(f, ", {}]] ({})>", self.unpacked.path, sync::Arc::strong_count(&self.unpacked))
    }
}

impl std::fmt::Debug for Clip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Clip::Streamed(cs) => write!(f, "{:?}", cs),
            Clip::Unpacked(cu) => write!(f, "{:?}", cu),
        }
    }
}


/// ClipBank

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
pub struct ClipId(u32);

// ClipId(0) is used as nil
impl ClipId {
    pub unsafe fn nil() -> Self { Self(0) }
    pub unsafe fn is_nil(&self) -> bool { self.0 == 0 }
}

pub struct ClipBank {
    clips: HashMap<u32, Clip>,
    clipid_next: u32,
}

impl ClipBank {
    pub fn new() -> Self {
        Self {
            clips: HashMap::new(),
            clipid_next: 1, // ClipId(0) is used as nil
        }
    }

    fn push(&mut self, clip: Clip) -> ClipId {
        let clipid = self.clipid_next;
        self.clipid_next += 1;
        self.clips.insert(clipid, clip);
        ClipId(clipid)
    }

    fn get(&self, clipid: ClipId) -> Option<&Clip> {
        self.clips.get(&clipid.0)
    }

    fn get_mut(&mut self, clipid: ClipId) -> Option<&mut Clip> {
        self.clips.get_mut(&clipid.0)
    }

    fn find_packed_data(&self, path: &AppPath) -> Option<&sync::Arc<PackedWebPData>> {
        self.clips.values()
        .filter_map(|clip| match clip {
            Clip::Streamed(cs) => Some(&cs.packed),
            _ => None,
        })
        .find(|packed| &packed.path == path)
    }

    fn find_unpacked_data(&self, path: &AppPath, size: AutoSize) -> Option<&sync::Arc<UnpackedWebPData>> {
        self.clips.values()
        .filter_map(|clip| match clip {
            Clip::Unpacked(cu) => Some(&cu.unpacked),
            _ => None,
        })
        .find(|unpacked|
            &unpacked.path == path && size.complete(unpacked.orig_width, unpacked.orig_height) == (unpacked.width, unpacked.height)
        )
    }


    /// These methods will be called from Watcher via Sprites-Watecher interface

    fn load_clip_streamed(&mut self,
        path: &AppPath,
        size: AutoSize,
        max_decoded_frames: usize,
        lazy_rescale: Option<bool>,
        loop_count: Option<u32>
    ) -> Result<ClipId, ClipLoadError> {
        // find reusable packed data
        let packed = match self.find_packed_data(path) {
            Some(packed) => packed.clone(),
            None => sync::Arc::new(PackedWebPData::load(path)?),
        };

        let clip = ClipStreamed::from_packed_webp(packed, size, max_decoded_frames, lazy_rescale, loop_count)?;
        let clip = Clip::Streamed(clip);
        let clipid = self.push(clip);
        Ok(clipid)
    }

    fn load_clip_unpacked(&mut self,
        path: &AppPath,
        size: AutoSize,
        loop_count: Option<u32>
    ) -> Result<ClipId, ClipLoadError> {
        // find reusable unpacked data
        let unpacked = match self.find_unpacked_data(path, size) {
            Some(unpacked) => unpacked.clone(),
            None => {
                // find reusable packed data
                let packed = match self.find_packed_data(path) {
                    // this temporary Arc will be dropped right away in UnpackedWebPData::from
                    Some(packed) => packed.clone(),
                    None => sync::Arc::new(PackedWebPData::load(path)?),
                };
                sync::Arc::new(UnpackedWebPData::from(packed, size)?)
            }
        };

        let clip = ClipUnpacked::from_unpacked_webp(unpacked, loop_count)?;
        let clip = Clip::Unpacked(clip);
        let clipid = self.push(clip);
        Ok(clipid)
    }

    pub fn load_clip(&mut self,
        path: &AppPath,
        size: AutoSize,
        max_decoded_frames: usize,
        lazy_rescale: Option<bool>,
        loop_count: Option<u32>
    ) -> Result<ClipId, ClipLoadError> {
        let clipid = if max_decoded_frames == 0 {
            self.load_clip_unpacked(path, size, loop_count)
        } else {
            self.load_clip_streamed(path, size, max_decoded_frames, lazy_rescale, loop_count)
        }?;

        log::debug!("clipbank.load_clip('{}', {:?}, {:?}) => {:?} {:?}", path, size.width(), size.height(), self.get(clipid), clipid);
        Ok(clipid)
    }

    pub fn unload_clip(&mut self, clipid: ClipId) {
        let Some(clip) = self.clips.remove(&clipid.0) else {
            log::error!("tried to unload freed clip with {clipid:?}");
            return;
        };

        log::debug!("clipbank.unload({clipid:?}) => {:?}", clip);
    }

    /// Reload all the clips that are loaded from 'path'.
    /// Returned iterator gives the clipids of updated clips, acompanied with possible clip load error.
    /// The reloading process is on-the-go; clips get reloaded as the ReloadIter iterator is consumed.
    /// If droped, the iterator will consume itself guraranteeing reload of all relevant clips.
    pub fn reload_context<'path>(&mut self, path: &'path AppPath) -> ClipDataReloadContext<'path, '_> {
        ClipDataReloadContext { bank: self, path, packed: None, unpacked: vec![] }
    }


    /// These methods are Clipbank-Animator interface

    /// returns if the clip was rewinded
    pub fn advance(&mut self, clipid: ClipId) -> bool {
        let Some(clip) = self.get_mut(clipid) else {
            log::error!("tried to advance on an unknown clipid {clipid:?}");
            return true;
        };
        clip.advance()
    }

    pub fn get_loop_count_max(&self, clipid: ClipId) -> u32 {
        let Some(clip) = self.get(clipid) else {
            log::error!("tried to get loop_count for an unknown clipid {clipid:?}");
            return 1;
        };
        clip.loop_count()
    }

    pub fn calc_lazy_rescale(&mut self, clipid: ClipId) {
        let Some(clip) = self.get_mut(clipid) else {
            log::error!("tried to get calc rescale on an unknown clipid {clipid:?}");
            return;
        };
        clip.calc_lazy_rescale()
    }

    pub fn get_current_frame(&self, clipid: ClipId, pos: (i32, i32)) -> Frame<'_> {
        let Some(clip) = self.get(clipid) else {
            log::error!("tried to get frame from an unknown clipid {clipid:?}");
            return Frame::error(pos);
        };
        clip.current_frame(pos)
    }

    pub fn get_size(&self, clipid: ClipId) -> (usize, usize) {
        let Some(clip) = self.get(clipid) else {
            log::error!("tried to get size with an unknown clipid {clipid:?}");
            return (0, 0);
        };
        clip.size()
    }

    //// These methods are Clipbank-Decoder interface

    pub fn get_decode_shortfall(&self, clipid: ClipId) -> usize {
        let Some(clip) = self.get(clipid) else {
            log::error!("tried to get decode_shortfall of an unknown clipid {clipid:?}");
            return 0;
        };

        clip.decode_shortfall()
    }

    pub fn decode_next_frame(&mut self, clipid: ClipId) -> Result<(), ClipLoadError> {
        let Some(clip) = self.get_mut(clipid) else {
            log::error!("tried to decode frame from an unknown clipid {clipid:?}");
            return Ok(())
        };

        match clip.decode_next_frame() {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn list_shortfalls(&self) -> impl Iterator<Item=(ClipId, usize)> + use<'_> {
        self.clips.iter().map(|(clipid, clip)|
            (ClipId(*clipid), clip.decode_shortfall())
        )
    }
}

pub struct ClipDataReloadContext<'path, 'bank> {
    bank: &'bank mut ClipBank,
    path: &'path AppPath,
    packed: Option<sync::Arc<PackedWebPData>>,
    unpacked: Vec<sync::Arc<UnpackedWebPData>>,
}

impl<'path, 'bank> ClipDataReloadContext<'path, 'bank> {
    pub fn reload(&mut self,
        clipid: ClipId,
        size: AutoSize,
        max_decoded_frames: usize,
        lazy_rescale: Option<bool>,
        loop_count: Option<u32>,
    ) -> Result<bool, ClipLoadError> {
        let Some(clip) = self.bank.get_mut(clipid) else {
            log::debug!("tried to reload with unkonwn clipid {clipid:?}");
            return Ok(false)
        };
        if clip.path() != self.path { return Ok(false) }

        let packed = match &self.packed {
            Some(packed) => packed,
            None => {
                let packed = sync::Arc::new(PackedWebPData::load(self.path)?);
                self.packed = Some(packed);
                self.packed.as_ref().unwrap()
            }
        };

        match clip {
            Clip::Streamed(cs) => {
                *cs = ClipStreamed::from_packed_webp(packed.clone(), size, max_decoded_frames, lazy_rescale, loop_count)?;
            }
            Clip::Unpacked(cu) => {
                let found = self.unpacked.iter().find(|unpacked|
                    unpacked.path == packed.path && size.complete(unpacked.orig_width, unpacked.orig_height) == (unpacked.width, unpacked.height)
                );
                let unpacked = match found {
                    Some(unpacked) => unpacked.clone(),
                    None => {
                        let new_unpacked = sync::Arc::new(UnpackedWebPData::from(packed.clone(), size)?);
                        self.unpacked.push(new_unpacked.clone());
                        new_unpacked
                    }
                };
                *cu = ClipUnpacked::from_unpacked_webp(unpacked, loop_count)?;
            }
        };

        Ok(true)
    }
}

unsafe fn scale_bilinear(
    src: &[u8],
    dst: &mut [u8],
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
) {
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
                let p00 = src[(y0 * src_w + x0) * 4 + c] as f32;
                let p10 = src[(y0 * src_w + x1) * 4 + c] as f32;
                let p01 = src[(y1 * src_w + x0) * 4 + c] as f32;
                let p11 = src[(y1 * src_w + x1) * 4 + c] as f32;

                let v0 = p00 + (p10 - p00) * fx;
                let v1 = p01 + (p11 - p01) * fx;
                let v = v0 + (v1 - v0) * fy;

                dst[(y * dst_w + x) * 4 + c] = v as u8;
            }
        }
    }
}