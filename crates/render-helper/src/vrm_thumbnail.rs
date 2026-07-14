//! Extraction of the author-provided thumbnail embedded in VRM 0.x/1.0.
//!
//! VRM files are GLB containers. Keeping this parsing and image decoding in
//! the helper process preserves the shell provider's crash-isolation boundary.

use std::{fs, io::Cursor, path::Path};

use image::{
    imageops::{self, FilterType},
    io::{Limits, Reader as ImageReader},
    GenericImageView, ImageFormat, RgbaImage,
};
use serde_json::Value;

const JSON_CHUNK: &[u8; 4] = b"JSON";
const BIN_CHUNK: &[u8; 4] = b"BIN\0";
const MAX_ENCODED_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 8192;
const MAX_IMAGE_ALLOC_BYTES: u64 = 128 * 1024 * 1024;

pub fn load(path: &Path, size: u32) -> Option<Vec<u8>> {
    let is_vrm = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("vrm"))
        .unwrap_or(false);
    if !is_vrm {
        return None;
    }
    let data = fs::read(path).ok()?;
    thumbnail_pixels(&data, size)
}

fn thumbnail_pixels(glb: &[u8], size: u32) -> Option<Vec<u8>> {
    if size == 0 {
        return None;
    }
    let (json, bin) = glb_chunks(glb)?;
    let root: Value = serde_json::from_slice(json).ok()?;
    let image_index = vrm_thumbnail_image_index(&root)?;
    let image = root.get("images")?.as_array()?.get(image_index)?;
    let view_index = value_index(image.get("bufferView")?)?;
    let view = root.get("bufferViews")?.as_array()?.get(view_index)?;
    if view.get("buffer").and_then(Value::as_u64) != Some(0) {
        return None;
    }
    let offset: usize = view
        .get("byteOffset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .try_into()
        .ok()?;
    let length: usize = view.get("byteLength")?.as_u64()?.try_into().ok()?;
    if length > MAX_ENCODED_IMAGE_BYTES {
        return None;
    }
    let end = offset.checked_add(length)?;
    let encoded = bin.get(offset..end)?;
    let format = supported_image_format(image.get("mimeType").and_then(Value::as_str), encoded)?;
    decode_and_fit(encoded, format, size)
}

fn glb_chunks(data: &[u8]) -> Option<(&[u8], &[u8])> {
    if data.len() < 20 || &data[..4] != b"glTF" || read_u32(data, 4)? != 2 {
        return None;
    }
    let declared_length: usize = read_u32(data, 8)?.try_into().ok()?;
    if declared_length != data.len() {
        return None;
    }

    let mut offset = 12usize;
    let mut json = None;
    let mut bin = None;
    while offset < data.len() {
        let length: usize = read_u32(data, offset)?.try_into().ok()?;
        let kind = data.get(offset + 4..offset + 8)?;
        let start = offset.checked_add(8)?;
        let end = start.checked_add(length)?;
        let chunk = data.get(start..end)?;
        if kind == JSON_CHUNK && json.is_none() {
            json = Some(chunk);
        } else if kind == BIN_CHUNK && bin.is_none() {
            bin = Some(chunk);
        }
        offset = end;
    }
    Some((json?, bin?))
}

fn read_u32(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn vrm_thumbnail_image_index(root: &Value) -> Option<usize> {
    // VRM 1.0 points directly at glTF.images.
    if let Some(index) = root.pointer("/extensions/VRMC_vrm/meta/thumbnailImage") {
        return value_index(index);
    }

    // VRM 0.x points at glTF.textures, whose source points at glTF.images.
    let texture_index = value_index(root.pointer("/extensions/VRM/meta/texture")?)?;
    let texture = root.get("textures")?.as_array()?.get(texture_index)?;
    value_index(texture.get("source")?)
}

fn value_index(value: &Value) -> Option<usize> {
    value.as_u64()?.try_into().ok()
}

fn supported_image_format(mime_type: Option<&str>, encoded: &[u8]) -> Option<ImageFormat> {
    match mime_type {
        Some("image/png") => Some(ImageFormat::Png),
        Some("image/jpeg") => Some(ImageFormat::Jpeg),
        Some(_) => None,
        None => match image::guess_format(encoded).ok()? {
            ImageFormat::Png => Some(ImageFormat::Png),
            ImageFormat::Jpeg => Some(ImageFormat::Jpeg),
            _ => None,
        },
    }
}

fn decode_and_fit(encoded: &[u8], format: ImageFormat, size: u32) -> Option<Vec<u8>> {
    let mut reader = ImageReader::with_format(Cursor::new(encoded), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC_BYTES);
    reader.limits(limits);
    let image = reader.decode().ok()?;
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    let (target_width, target_height) = if width >= height {
        (
            size,
            ((height as u64 * size as u64) / width as u64).max(1) as u32,
        )
    } else {
        (
            ((width as u64 * size as u64) / height as u64).max(1) as u32,
            size,
        )
    };
    let resized = image
        .resize_exact(target_width, target_height, FilterType::Triangle)
        .to_rgba8();
    let mut output = RgbaImage::new(size, size);
    imageops::replace(
        &mut output,
        &resized,
        ((size - target_width) / 2) as i64,
        ((size - target_height) / 2) as i64,
    );
    Some(output.into_raw())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageOutputFormat, Rgba, RgbaImage};
    use serde_json::json;

    use super::thumbnail_pixels;

    fn red_png() -> Vec<u8> {
        let mut encoded = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 1, Rgba([255, 0, 0, 255])))
            .write_to(&mut encoded, ImageOutputFormat::Png)
            .unwrap();
        encoded.into_inner()
    }

    fn make_glb(vrm1: bool) -> Vec<u8> {
        let image = red_png();
        let extension = if vrm1 {
            json!({"VRMC_vrm": {"specVersion": "1.0", "meta": {"thumbnailImage": 0}}})
        } else {
            json!({"VRM": {"specVersion": "0.0", "meta": {"texture": 0}}})
        };
        let mut root = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": image.len()}],
            "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": image.len()}],
            "images": [{"bufferView": 0, "mimeType": "image/png"}],
            "extensions": extension
        });
        if !vrm1 {
            root["textures"] = json!([{"source": 0}]);
        }

        let mut json_bytes = serde_json::to_vec(&root).unwrap();
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }
        let image_length = image.len();
        let mut bin = image;
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let total_length = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total_length);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total_length as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin);
        assert!(image_length <= bin.len());
        glb
    }

    #[test]
    fn extracts_vrm1_thumbnail() {
        let pixels = thumbnail_pixels(&make_glb(true), 8).unwrap();
        assert_eq!(pixels.len(), 8 * 8 * 4);
        assert_eq!(
            &pixels[4 * (8 * 3 + 4)..4 * (8 * 3 + 4) + 4],
            &[255, 0, 0, 255]
        );
        assert_eq!(&pixels[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn extracts_vrm0_thumbnail() {
        assert!(thumbnail_pixels(&make_glb(false), 8).is_some());
    }

    #[test]
    fn rejects_truncated_glb() {
        let mut glb = make_glb(true);
        glb.pop();
        assert!(thumbnail_pixels(&glb, 8).is_none());
    }
}
