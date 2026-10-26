//! Thumbnails have bounded decoded memory and a single frame; originals stay on the server.

use gpui_kit::RenderImage;
use std::io::Cursor;
use std::sync::Arc;

pub fn thumbnail(bytes: &[u8]) -> Result<Arc<RenderImage>, image::ImageError> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let mut pixels = reader.decode()?.thumbnail(384, 256).to_rgba8();
    // GPUI's render image stores BGRA, while image decoders produce RGBA.
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn thumbnails_bound_dimensions_and_use_gpui_pixel_order() {
        let pixels = image::RgbaImage::from_pixel(1024, 512, image::Rgba([255, 0, 0, 255]));
        let mut encoded = Cursor::new(Vec::new());
        pixels
            .write_to(&mut encoded, image::ImageFormat::Png)
            .unwrap();
        let preview = thumbnail(encoded.get_ref()).unwrap();
        assert_eq!(preview.size(0).width.0, 384);
        assert_eq!(preview.size(0).height.0, 192);
        assert_eq!(&preview.as_bytes(0).unwrap()[..4], &[0, 0, 255, 255]);
        assert!(thumbnail(b"not an image").is_err());
    }
    #[test]
    fn animated_images_render_one_static_frame() {
        let pixels = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255]));
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            encoder
                .encode_frame(image::Frame::new(pixels.clone()))
                .unwrap();
            encoder.encode_frame(image::Frame::new(pixels)).unwrap();
        }
        assert_eq!(thumbnail(&bytes).unwrap().frame_count(), 1);
    }
}
