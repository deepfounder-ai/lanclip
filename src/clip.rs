// System clipboard access (text, images as PNG, copied file lists).
use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Default)]
pub struct ClipState {
    // On Linux the clipboard owner must stay alive, so keep one instance for the daemon's lifetime.
    board: Mutex<Option<arboard::Clipboard>>,
    pub last_text: Mutex<Option<String>>,
    pub last_image: Mutex<Option<u64>>,
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

impl ClipState {
    fn with<R>(&self, f: impl FnOnce(&mut arboard::Clipboard) -> Option<R>) -> Option<R> {
        let mut guard = self.board.lock().unwrap();
        if guard.is_none() {
            *guard = arboard::Clipboard::new().ok();
        }
        f(guard.as_mut()?)
    }

    pub fn read_text(&self) -> Option<String> {
        self.with(|c| c.get_text().ok()).filter(|t| !t.is_empty())
    }

    /// Clipboard image as (png bytes, content hash).
    pub fn read_image(&self) -> Option<(Vec<u8>, u64)> {
        let img = self.with(|c| c.get_image().ok())?;
        let h = hash(&img.bytes);
        Some((encode_png(img.width as u32, img.height as u32, &img.bytes)?, h))
    }

    pub fn image_hash(&self) -> Option<u64> {
        self.with(|c| c.get_image().ok()).map(|img| hash(&img.bytes))
    }

    /// Files copied in Explorer / Finder / Linux file managers.
    pub fn read_files(&self) -> Vec<PathBuf> {
        self.with(|c| c.get().file_list().ok()).unwrap_or_default().into_iter().filter(|p| p.exists()).collect()
    }

    pub fn write_text(&self, text: &str) -> bool {
        let ok = self.with(|c| c.set_text(text.to_string()).ok()).is_some();
        if ok {
            *self.last_text.lock().unwrap() = Some(text.to_string());
        }
        ok
    }

    pub fn write_png(&self, path: &Path) -> bool {
        let Some((w, h, rgba)) = decode_png(path) else { return false };
        let img = arboard::ImageData { width: w as usize, height: h as usize, bytes: Cow::Owned(rgba) };
        let ok = self.with(|c| c.set_image(img).ok()).is_some();
        if ok {
            *self.last_image.lock().unwrap() = self.image_hash();
            *self.last_text.lock().unwrap() = self.read_text();
        }
        ok
    }

    /// Record the current clipboard so auto-sync doesn't resend it.
    pub fn remember(&self) {
        *self.last_text.lock().unwrap() = self.read_text();
        *self.last_image.lock().unwrap() = self.image_hash();
    }
}

pub fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
    }
    Some(out)
}

pub fn decode_png(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let mut dec = png::Decoder::new(File::open(path).ok()?);
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    Some((info.width, info.height, rgba))
}
