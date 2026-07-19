use tiny_skia::Pixmap;

use crate::{Error, Result};

pub(crate) fn encode_opaque_png(pixmap: &Pixmap) -> Result<Vec<u8>> {
    ensure_opaque(pixmap)?;
    let mut rgb = vec![0; pixmap.data().len() / 4 * 3];
    for (target, pixel) in rgb.chunks_exact_mut(3).zip(pixmap.data().chunks_exact(4)) {
        target.copy_from_slice(&pixel[..3]);
    }
    let mut data = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut data, pixmap.width(), pixmap.height());
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Balanced);
        let mut writer = encoder
            .write_header()
            .map_err(|error| Error::Render(error.to_string()))?;
        writer
            .write_image_data(&rgb)
            .map_err(|error| Error::Render(error.to_string()))?;
    }
    Ok(data)
}

fn ensure_opaque(pixmap: &Pixmap) -> Result<()> {
    if pixmap.data().chunks_exact(4).any(|pixel| pixel[3] != 255) {
        return Err(Error::Render(
            "PNG surface unexpectedly contains transparency".into(),
        ));
    }
    Ok(())
}
