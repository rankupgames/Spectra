use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, StrokeDash, Transform};

use crate::{
    Error, Result,
    glyphs::glyph,
    png::encode_opaque_png,
    scene::{
        BACKGROUND, CARD_HEIGHT, EDGE, Ink, PANEL, SUBTEXT, Scene, SceneEdge, SceneNode, TEXT,
        UNCERTAIN, truncate,
    },
};

pub(crate) fn render_scene_png(scene: &Scene) -> Result<Vec<u8>> {
    let mut pixmap = Pixmap::new(scene.options.width, scene.options.height)
        .ok_or_else(|| Error::Render("unable to allocate PNG surface".into()))?;
    pixmap.fill(Color::from_rgba8(
        BACKGROUND.rgba[0],
        BACKGROUND.rgba[1],
        BACKGROUND.rgba[2],
        BACKGROUND.rgba[3],
    ));
    draw_bitmap_text(
        &mut pixmap,
        &format!("SPECTRA - {}", truncate(&scene.query, 90)),
        32,
        20,
        3,
        TEXT,
    );
    for edge in &scene.edges {
        draw_edge(&mut pixmap, edge);
    }
    for node in &scene.nodes {
        draw_node(&mut pixmap, node);
    }
    draw_bitmap_text(
        &mut pixmap,
        "SOLID CONFIRMED - AMBER RUNTIME BOUNDARY - THICK QUERY ANCHOR",
        32,
        scene.options.height as i32 - 30,
        2,
        SUBTEXT,
    );
    encode_opaque_png(&pixmap)
}

fn draw_edge(pixmap: &mut Pixmap, edge: &SceneEdge) {
    let color = if edge.uncertain { UNCERTAIN } else { EDGE };
    let mut builder = PathBuilder::new();
    builder.move_to(edge.x1 as f32, edge.y1 as f32);
    builder.cubic_to(
        (edge.x1 + 49) as f32,
        edge.y1 as f32,
        (edge.x2 - 48) as f32,
        edge.y2 as f32,
        edge.x2 as f32,
        edge.y2 as f32,
    );
    if let Some(path) = builder.finish() {
        let edge_paint = paint(color, if edge.containment { 77 } else { 173 });
        let stroke = Stroke {
            width: if edge.containment { 1.0 } else { 2.0 },
            dash: edge
                .uncertain
                .then(|| StrokeDash::new(vec![7.0, 6.0], 0.0))
                .flatten(),
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &edge_paint, &stroke, Transform::identity(), None);
    }
    let mut arrow = PathBuilder::new();
    arrow.move_to(edge.x2 as f32, edge.y2 as f32);
    arrow.line_to((edge.x2 - 7) as f32, (edge.y2 - 4) as f32);
    arrow.line_to((edge.x2 - 7) as f32, (edge.y2 + 4) as f32);
    arrow.close();
    if let Some(path) = arrow.finish() {
        pixmap.fill_path(
            &path,
            &paint(color, 255),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn draw_node(pixmap: &mut Pixmap, node: &SceneNode) {
    let Some(rect) = Rect::from_xywh(
        node.x as f32,
        node.y as f32,
        node.width as f32,
        CARD_HEIGHT as f32,
    ) else {
        return;
    };
    pixmap.fill_rect(rect, &paint(PANEL, 255), Transform::identity(), None);
    let path = PathBuilder::from_rect(rect);
    pixmap.stroke_path(
        &path,
        &paint(node.color, 255),
        &Stroke {
            width: if node.anchor.is_some() { 4.0 } else { 2.0 },
            ..Stroke::default()
        },
        Transform::identity(),
        None,
    );
    if let Some(anchor) = &node.anchor {
        if let Some(path) = PathBuilder::from_circle(
            (node.x + node.width - 16) as f32,
            (node.y + 10) as f32,
            13.0,
        ) {
            pixmap.fill_path(
                &path,
                &paint(node.color, 255),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        draw_bitmap_text(
            pixmap,
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
    draw_bitmap_text(
        pixmap,
        &truncate(&node.title, title_limit),
        node.x + 12,
        node.y + 9,
        2,
        TEXT,
    );
    draw_bitmap_text(
        pixmap,
        &truncate(&node.subtitle, subtitle_limit),
        node.x + 12,
        node.y + 33,
        2,
        SUBTEXT,
    );
}

fn paint(color: Ink, alpha: u8) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.rgba[0], color.rgba[1], color.rgba[2], alpha);
    paint.anti_alias = false;
    paint
}

fn draw_bitmap_text(pixmap: &mut Pixmap, text: &str, x: i32, y: i32, scale: i32, color: Ink) {
    let mut builder = PathBuilder::new();
    for (character_index, character) in text.chars().enumerate() {
        for (row, bits) in glyph(character).iter().enumerate() {
            for column in 0..3 {
                if bits & (1 << (2 - column)) == 0 {
                    continue;
                }
                let Some(rect) = Rect::from_xywh(
                    (x + character_index as i32 * 4 * scale + column * scale) as f32,
                    (y + row as i32 * scale) as f32,
                    scale as f32,
                    scale as f32,
                ) else {
                    continue;
                };
                builder.push_rect(rect);
            }
        }
    }
    if let Some(path) = builder.finish() {
        pixmap.fill_path(
            &path,
            &paint(color, 255),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}
