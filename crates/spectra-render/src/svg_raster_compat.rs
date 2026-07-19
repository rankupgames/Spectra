use crate::{Error, RenderOptions, Result, png::encode_opaque_png};

const MAX_CANONICAL_SVG_BYTES: usize = 2 * 1024 * 1024;
const CANONICAL_SVG_PREFIX: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\"";

pub(crate) fn rasterize_canonical_svg(svg_bytes: &[u8]) -> Result<Vec<u8>> {
    if svg_bytes.len() > MAX_CANONICAL_SVG_BYTES {
        return Err(Error::Render(format!(
            "canonical SVG exceeds the {MAX_CANONICAL_SVG_BYTES}-byte compatibility ceiling"
        )));
    }
    if !svg_bytes.starts_with(CANONICAL_SVG_PREFIX) {
        return Err(Error::Render(
            "compatibility rasterization requires canonical Spectra SVG bytes".into(),
        ));
    }

    let options = resvg::usvg::Options {
        resources_dir: None,
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..resvg::usvg::Options::default()
    };
    let tree = resvg::usvg::Tree::from_data(svg_bytes, &options)
        .map_err(|error| Error::Render(format!("unable to parse canonical SVG: {error}")))?;
    let size = tree.size().to_int_size();
    RenderOptions {
        width: size.width(),
        height: size.height(),
    }
    .validate()?;

    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or_else(|| Error::Render("unable to allocate SVG compatibility surface".into()))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    encode_opaque_png(&pixmap)
}
