use crate::{
    RenderOptions,
    glyphs::glyph,
    scene::{BACKGROUND, CARD_HEIGHT, EDGE, Ink, PANEL, SUBTEXT, Scene, TEXT, UNCERTAIN, truncate},
};

pub(crate) fn render_scene_svg(scene: &Scene) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}">"#,
        scene.options.width, scene.options.height, scene.options.width, scene.options.height
    ));
    out.push_str(&format!(
        r#"<rect width="100%" height="100%" fill="{}"/>"#,
        BACKGROUND.hex
    ));
    svg_bitmap_text(
        &mut out,
        &format!("SPECTRA - {}", truncate(&scene.query, 90)),
        32,
        20,
        3,
        TEXT,
    );
    for edge in &scene.edges {
        let color = if edge.uncertain { UNCERTAIN } else { EDGE };
        out.push_str(&format!(
            r#"<path d="M{},{} C{},{} {},{} {},{}" fill="none" stroke="{}" stroke-width="{}" {} opacity="{}"/>"#,
            edge.x1,
            edge.y1,
            edge.x1 + 49,
            edge.y1,
            edge.x2 - 48,
            edge.y2,
            edge.x2,
            edge.y2,
            color.hex,
            if edge.containment { 1 } else { 2 },
            if edge.uncertain {
                r#"stroke-dasharray="7 6""#
            } else {
                ""
            },
            if edge.containment { "0.30" } else { "0.68" }
        ));
        out.push_str(&format!(
            r#"<path d="M{},{} L{},{} L{},{} z" fill="{}"/>"#,
            edge.x2,
            edge.y2,
            edge.x2 - 7,
            edge.y2 - 4,
            edge.x2 - 7,
            edge.y2 + 4,
            color.hex
        ));
    }
    for node in &scene.nodes {
        out.push_str(&format!(
            r#"<g data-stable-ref="{}"{} data-kind="{}">"#,
            escape(&node.stable_ref),
            node.object_hash
                .as_ref()
                .map(|hash| format!(r#" data-object-hash="{}""#, escape(hash)))
                .unwrap_or_default(),
            escape(&node.kind)
        ));
        out.push_str(&format!(
            r#"<rect x="{}" y="{}" width="{}" height="{}" rx="2" fill="{}" stroke="{}" stroke-width="{}"/>"#,
            node.x,
            node.y,
            node.width,
            CARD_HEIGHT,
            PANEL.hex,
            node.color.hex,
            if node.anchor.is_some() { 4 } else { 2 }
        ));
        if let Some(anchor) = &node.anchor {
            out.push_str(&format!(
                r#"<circle cx="{}" cy="{}" r="13" fill="{}"/>"#,
                node.x + node.width - 16,
                node.y + 10,
                node.color.hex
            ));
            svg_bitmap_text(
                &mut out,
                anchor,
                node.x + node.width - 23,
                node.y + 5,
                2,
                BACKGROUND,
            );
        }
        let reserved = if node.anchor.is_some() { 42 } else { 18 };
        let title_limit = ((node.width - reserved) / 8).max(1) as usize;
        let subtitle_limit = ((node.width - 18) / 8).max(1) as usize;
        svg_bitmap_text(
            &mut out,
            &truncate(&node.title, title_limit),
            node.x + 12,
            node.y + 9,
            2,
            TEXT,
        );
        svg_bitmap_text(
            &mut out,
            &truncate(&node.subtitle, subtitle_limit),
            node.x + 12,
            node.y + 33,
            2,
            SUBTEXT,
        );
        out.push_str("</g>");
    }
    svg_bitmap_text(
        &mut out,
        "SOLID CONFIRMED - AMBER RUNTIME BOUNDARY - THICK QUERY ANCHOR",
        32,
        scene.options.height as i32 - 30,
        2,
        SUBTEXT,
    );
    out.push_str("</svg>");
    out
}

pub(crate) fn error_svg(options: RenderOptions, message: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" data-render-error="{}"><rect width="100%" height="100%" fill="{}"/></svg>"#,
        options.width,
        options.height,
        escape(message),
        BACKGROUND.hex
    )
}

fn svg_bitmap_text(out: &mut String, text: &str, x: i32, y: i32, scale: i32, color: Ink) {
    out.push_str(&format!(
        r#"<g data-text="{}" fill="{}">"#,
        escape(text),
        color.hex
    ));
    let mut path = String::new();
    for (character_index, character) in text.chars().enumerate() {
        if character_index > 0 && character_index % 8 == 0 && !path.is_empty() {
            out.push_str(r#"<path d=""#);
            out.push_str(&path);
            out.push_str(r#""/>"#);
            path.clear();
        }
        for (row, bits) in glyph(character).iter().enumerate() {
            for column in 0..3 {
                if bits & (1 << (2 - column)) == 0 {
                    continue;
                }
                let left = x + character_index as i32 * 4 * scale + column * scale;
                let top = y + row as i32 * scale;
                path.push_str(&format!("M{left} {top}h{scale}v{scale}h-{scale}z"));
            }
        }
    }
    if !path.is_empty() {
        out.push_str(r#"<path d=""#);
        out.push_str(&path);
        out.push_str(r#""/>"#);
    }
    out.push_str("</g>");
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
