//! CPU rendering inside an extension's helper process. The guest supplies drawing commands;
//! the application receives only the completed pixel frame, never this display list.
#![forbid(unsafe_code)]

mod display;
pub use display::DisplayList;

use display::Reader;
use peersus_api::canvas::{FrameError, FrameLayout, display as dl};
use skia_safe::{
    AlphaType, Canvas, Color, ColorSpace, ColorType, Data, FontMgr, Image, ImageInfo, Matrix,
    Paint, PathBuilder, Rect, images, paint, surfaces,
    textlayout::{
        FontCollection, ParagraphBuilder, ParagraphStyle, TextAlign, TextStyle,
        TypefaceFontProvider,
    },
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const FONT: &[u8] = include_bytes!("../../peersus-ui/assets/fonts/Quicksand[wght].ttf");
const FONT_FAMILY: &str = "Quicksand";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("invalid canvas display list")]
    Invalid,
    #[error("canvas display list exceeds its work or size budget")]
    Limit,
    #[error("could not create a canvas raster resource")]
    Resource,
    #[error(transparent)]
    Frame(#[from] FrameError),
}

/// One reusable raster buffer per helper, no retained display lists or guest-supplied fonts.
#[derive(Default)]
pub struct Renderer {
    pixels: Vec<u8>,
    fonts: Option<FontCollection>,
}

impl Renderer {
    /// Render into tightly packed premultiplied sRGB. On failure no pixels are published.
    pub fn render(&mut self, list: DisplayList<'_>) -> Result<&[u8], RenderError> {
        let deadline = Instant::now() + Duration::from_millis(dl::MAX_RENDER_MILLIS);
        let layout = list.header.layout;
        let len = layout.byte_len();
        self.pixels
            .try_reserve_exact(len.saturating_sub(self.pixels.len()))
            .map_err(|_| RenderError::Resource)?;
        self.pixels.resize(len, 0);
        let info = image_info(layout, AlphaType::Premul);
        {
            let mut raster =
                surfaces::wrap_pixels(&info, &mut self.pixels, layout.stride() as usize, None)
                    .ok_or(RenderError::Resource)?;
            let canvas = raster.canvas();
            canvas.clear(Color::TRANSPARENT);
            let scale = list.scale as f32 / 1000.;
            canvas.scale((scale, scale));
            draw_commands(canvas, &mut self.fonts, list, deadline)?;
        }
        Ok(&self.pixels)
    }
}

fn image_info(layout: FrameLayout, alpha: AlphaType) -> ImageInfo {
    ImageInfo::new(
        (layout.width() as i32, layout.height() as i32),
        ColorType::RGBA8888,
        alpha,
        ColorSpace::new_srgb(),
    )
}

fn color(
    bytes: &mut Reader<'_>,
    palette: &[u8; dl::PALETTE_BYTES],
) -> Result<[u8; 4], RenderError> {
    match bytes.u8()? {
        0 => bytes.array(),
        1 => Ok([0; 4]),
        at @ 2..=10 => {
            let at = usize::from(at - 2) * 4;
            palette[at..at + 4]
                .try_into()
                .map_err(|_| RenderError::Invalid)
        }
        _ => Err(RenderError::Invalid),
    }
}

fn rgba([r, g, b, a]: [u8; 4]) -> Color {
    Color::from_argb(a, r, g, b)
}

fn point(bytes: &mut Reader<'_>) -> Result<(f32, f32), RenderError> {
    Ok((bytes.number()?, bytes.number()?))
}

fn rect(bytes: &mut Reader<'_>) -> Result<Rect, RenderError> {
    let (x, y) = point(bytes)?;
    let (w, h) = point(bytes)?;
    if w < 0. || h < 0. {
        return Err(RenderError::Invalid);
    }
    Ok(Rect::from_xywh(x, y, w, h))
}

fn before(deadline: Instant) -> Result<(), RenderError> {
    if Instant::now() >= deadline {
        Err(RenderError::Limit)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Clip,
    Transform,
}

fn draw_commands(
    canvas: &Canvas,
    fonts: &mut Option<FontCollection>,
    list: DisplayList<'_>,
    deadline: Instant,
) -> Result<(), RenderError> {
    let mut bytes = list.commands;
    let mut scopes = [Scope::Clip; dl::MAX_STACK_DEPTH];
    let mut depth = 0usize;
    let mut verbs_left = dl::MAX_FRAME_PATH_VERBS;
    let mut images = BTreeMap::<u32, Image>::new();
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    for command in 0..=dl::MAX_COMMANDS {
        before(deadline)?;
        let op = bytes.u8()?;
        if op == dl::END {
            return if depth == 0 && bytes.is_empty() {
                Ok(())
            } else {
                Err(RenderError::Invalid)
            };
        }
        if command == dl::MAX_COMMANDS {
            return Err(RenderError::Limit);
        }
        match op {
            dl::CLEAR => {
                canvas.clear(rgba(color(&mut bytes, &list.palette)?));
            }
            dl::PATH => draw_path(
                canvas,
                &mut paint,
                &mut bytes,
                &list.palette,
                &mut verbs_left,
            )?,
            dl::TEXT => draw_text(canvas, fonts, &mut bytes, &list.palette)?,
            dl::IMAGE_DATA => {
                let id = bytes.u32()?;
                if images.len() >= dl::MAX_IMAGES {
                    return Err(RenderError::Limit);
                }
                if images.contains_key(&id) {
                    return Err(RenderError::Invalid);
                }
                let layout = FrameLayout::tight(u32::from(bytes.u16()?), u32::from(bytes.u16()?))?;
                let pixels = bytes.bytes(layout.byte_len())?;
                let info = image_info(layout, AlphaType::Unpremul);
                let image = images::raster_from_data(
                    &info,
                    Data::new_copy(pixels),
                    layout.stride() as usize,
                )
                .ok_or(RenderError::Resource)?;
                images.insert(id, image);
            }
            dl::IMAGE => {
                let image = images.get(&bytes.u32()?).ok_or(RenderError::Invalid)?;
                paint.set_color(Color::WHITE).set_style(paint::Style::Fill);
                canvas.draw_image_rect(image, None, rect(&mut bytes)?, &paint);
            }
            dl::CLIP_PUSH | dl::TRANSFORM_PUSH => {
                let scope = scopes.get_mut(depth).ok_or(RenderError::Limit)?;
                canvas.save();
                if op == dl::CLIP_PUSH {
                    canvas.clip_rect(rect(&mut bytes)?, None, true);
                    *scope = Scope::Clip;
                } else {
                    transform(canvas, &mut bytes)?;
                    *scope = Scope::Transform;
                }
                depth += 1;
            }
            dl::CLIP_POP | dl::TRANSFORM_POP => {
                depth = depth.checked_sub(1).ok_or(RenderError::Invalid)?;
                let expected = if op == dl::CLIP_POP {
                    Scope::Clip
                } else {
                    Scope::Transform
                };
                if scopes[depth] != expected {
                    return Err(RenderError::Invalid);
                }
                canvas.restore();
            }
            _ => return Err(RenderError::Invalid),
        }
    }
    Err(RenderError::Limit)
}

fn transform(canvas: &Canvas, bytes: &mut Reader<'_>) -> Result<(), RenderError> {
    let (a, b) = point(bytes)?;
    let (c, d) = point(bytes)?;
    let (e, f) = point(bytes)?;
    let next = Matrix::new_all(a, c, e, b, d, f, 0., 0., 1.);
    let composed = Matrix::concat(&canvas.local_to_device_as_3x3(), &next);
    if !(0..6).all(|i| composed[i].abs() <= dl::MAX_COORDINATE) {
        return Err(RenderError::Limit);
    }
    canvas.concat(&next);
    Ok(())
}

fn draw_path(
    canvas: &Canvas,
    paint: &mut Paint,
    bytes: &mut Reader<'_>,
    palette: &[u8; dl::PALETTE_BYTES],
    verbs_left: &mut usize,
) -> Result<(), RenderError> {
    let style = match bytes.u8()? {
        0 => paint::Style::Fill,
        1 => paint::Style::Stroke,
        _ => return Err(RenderError::Invalid),
    };
    let mut colour = color(bytes, palette)?;
    let width = bytes.number()?;
    if !(0. ..=dl::MAX_STROKE_WIDTH).contains(&width) {
        return Err(RenderError::Invalid);
    }
    let cap = match bytes.u8()? {
        0 => paint::Cap::Butt,
        1 => paint::Cap::Round,
        2 => paint::Cap::Square,
        _ => return Err(RenderError::Invalid),
    };
    let join = match bytes.u8()? {
        0 => paint::Join::Miter,
        1 => paint::Join::Round,
        2 => paint::Join::Bevel,
        _ => return Err(RenderError::Invalid),
    };
    let opacity = u16::from(bytes.u8()?);
    colour[3] = ((u16::from(colour[3]) * opacity + 127) / 255) as u8;
    let count = usize::from(bytes.u16()?);
    if count == 0 || count > dl::MAX_PATH_VERBS || count > *verbs_left {
        return Err(RenderError::Limit);
    }
    *verbs_left -= count;
    let mut path = PathBuilder::new();
    let mut contour = false;
    for _ in 0..count {
        match bytes.u8()? {
            0 => {
                path.move_to(point(bytes)?);
                contour = true;
            }
            1 if contour => {
                path.line_to(point(bytes)?);
            }
            2 if contour => {
                path.quad_to(point(bytes)?, point(bytes)?);
            }
            3 if contour => {
                path.cubic_to(point(bytes)?, point(bytes)?, point(bytes)?);
            }
            4 if contour => {
                path.close();
                contour = false;
            }
            _ => return Err(RenderError::Invalid),
        }
    }
    if colour[3] != 0 {
        paint
            .set_style(style)
            .set_color(rgba(colour))
            .set_stroke_width(width)
            .set_stroke_cap(cap)
            .set_stroke_join(join);
        canvas.draw_path(&path.detach(), paint);
    }
    Ok(())
}

fn draw_text(
    canvas: &Canvas,
    fonts: &mut Option<FontCollection>,
    bytes: &mut Reader<'_>,
    palette: &[u8; dl::PALETTE_BYTES],
) -> Result<(), RenderError> {
    let size = bytes.number()?;
    if size <= 0. || size > dl::MAX_TEXT_SIZE {
        return Err(RenderError::Invalid);
    }
    let colour = color(bytes, palette)?;
    let (x, y) = point(bytes)?;
    let align = bytes.u8()?;
    let len = usize::from(bytes.u16()?);
    if len > dl::MAX_TEXT_BYTES {
        return Err(RenderError::Limit);
    }
    let text = bytes.utf8(len)?;
    if align > 2 || text.chars().any(char::is_control) {
        return Err(RenderError::Invalid);
    }
    if text.is_empty() || colour[3] == 0 {
        return Ok(());
    }
    if fonts.is_none() {
        *fonts = Some(font_collection()?);
    }
    let fonts = fonts.as_ref().ok_or(RenderError::Resource)?;
    let mut text_style = TextStyle::new();
    text_style
        .set_font_families(&[FONT_FAMILY])
        .set_font_size(size)
        .set_color(rgba(colour));
    let mut style = ParagraphStyle::new();
    style
        .set_text_style(&text_style)
        .set_max_lines(1)
        .set_text_align(TextAlign::Left);
    let mut builder = ParagraphBuilder::new(&style, fonts);
    builder.push_style(&text_style).add_text(text);
    let mut paragraph = builder.build();
    paragraph.layout(f32::MAX);
    let x = x - paragraph.max_intrinsic_width() * f32::from(align) / 2.;
    paragraph.paint(canvas, (x, y - paragraph.alphabetic_baseline()));
    Ok(())
}

fn font_collection() -> Result<FontCollection, RenderError> {
    let system = FontMgr::new();
    let manager = FontMgr::custom_empty().unwrap_or_else(|| system.clone());
    let face = manager
        .new_from_data(Data::new_with_owner(FONT), None)
        .ok_or(RenderError::Resource)?;
    let mut embedded = TypefaceFontProvider::new();
    embedded.register_typeface(face, FONT_FAMILY);
    let mut fonts = FontCollection::new();
    fonts.set_default_font_manager(system, None);
    fonts.set_dynamic_font_manager(FontMgr::from(embedded));
    fonts.enable_font_fallback();
    fonts.paragraph_cache_mut().turn_on(false);
    Ok(fonts)
}
