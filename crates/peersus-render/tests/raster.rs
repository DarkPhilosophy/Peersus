use peersus_api::canvas::display as dl;
use peersus_render::{DisplayList, RenderError, Renderer};

fn begin(w: u32, h: u32, scale: u32) -> Vec<u8> {
    let mut out = b"PSD1\x01\x03map".to_vec();
    out.extend(1i32.to_le_bytes()); // canvas data handle
    out.extend(1u64.to_le_bytes()); // seq
    out.extend(7u64.to_le_bytes()); // settled
    for n in [w, h, scale] {
        out.extend(n.to_le_bytes());
    }
    let mut palette = [0; dl::PALETTE_BYTES];
    palette[24..28].copy_from_slice(&[40, 90, 220, 255]); // accent
    out.extend(palette);
    out
}

fn floats(out: &mut Vec<u8>, values: &[f32]) {
    for f in values {
        out.extend(f.to_le_bytes());
    }
}

fn path(out: &mut Vec<u8>, stroke: bool, color: &[u8], opacity: u8) {
    out.extend([dl::PATH, u8::from(stroke)]);
    out.extend(color);
    floats(out, &[2.]);
    out.extend([1, 1, opacity]); // round cap/join
    out.extend(5u16.to_le_bytes());
    for (verb, point) in [(0, [1., 1.]), (1, [9., 1.]), (1, [9., 8.]), (1, [1., 8.])] {
        out.push(verb);
        floats(out, &point);
    }
    out.push(4); // close
}

fn pixel(rgba: &[u8], width: usize, x: usize, y: usize) -> &[u8] {
    let i = (y * width + x) * 4;
    &rgba[i..i + 4]
}

#[test]
fn fill_clip_and_affine_transform_render_premultiplied_rgba() {
    let mut out = begin(32, 24, 2000);
    out.extend([dl::CLEAR, 1]); // transparent
    out.push(dl::CLIP_PUSH);
    floats(&mut out, &[0., 0., 10., 10.]);
    out.push(dl::TRANSFORM_PUSH);
    floats(&mut out, &[1., 0., 0., 1., 4., 0.]);
    path(&mut out, false, &[0, 200, 20, 10, 255], 128);
    out.extend([dl::TRANSFORM_POP, dl::CLIP_POP, dl::END]);
    let list = DisplayList::parse(&out).unwrap();
    assert_eq!(list.header().settled, 7);
    let mut renderer = Renderer::default();
    let bytes = renderer.render(list).unwrap();
    assert_eq!(pixel(bytes, 32, 12, 8), [100, 10, 5, 128]);
    assert_eq!(pixel(bytes, 32, 21, 8), [0, 0, 0, 0]); // outside clip
    assert_eq!(pixel(bytes, 32, 6, 8), [0, 0, 0, 0]); // before translated start

    let mut empty = begin(32, 24, 1000);
    empty.push(dl::END);
    let bytes = renderer
        .render(DisplayList::parse(&empty).unwrap())
        .unwrap();
    assert!(
        bytes.iter().all(|&v| v == 0),
        "reused buffers start transparent"
    );
}

#[test]
fn draws_strokes_theme_text_and_an_inline_image() {
    let mut out = begin(96, 32, 1000);
    path(&mut out, true, &[8], 255); // theme accent
    out.push(dl::TEXT);
    floats(&mut out, &[18.]);
    out.push(8); // accent
    floats(&mut out, &[54., 23.]);
    out.push(1); // centered
    let text = b"Peersus";
    out.extend(u16::try_from(text.len()).unwrap().to_le_bytes());
    out.extend(text);
    out.push(dl::IMAGE_DATA);
    out.extend(7u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend([0, 240, 0, 255]);
    out.push(dl::IMAGE);
    out.extend(7u32.to_le_bytes());
    floats(&mut out, &[9., 14., 4., 4.]);
    out.push(dl::END);
    let mut renderer = Renderer::default();
    let bytes = renderer.render(DisplayList::parse(&out).unwrap()).unwrap();
    assert_eq!(pixel(bytes, 96, 4, 1), [40, 90, 220, 255]);
    assert_eq!(pixel(bytes, 96, 4, 4), [0, 0, 0, 0]);
    assert_eq!(pixel(bytes, 96, 10, 16), [0, 240, 0, 255]);
    assert!((18..90).any(|x| (5..24).any(|y| pixel(bytes, 96, x, y)[3] > 127)));
    assert_eq!(pixel(bytes, 96, 2, 28), [0, 0, 0, 0]);
}

fn render(bytes: &[u8]) -> Result<(), RenderError> {
    Renderer::default().render(DisplayList::parse(bytes)?)?;
    Ok(())
}

#[test]
fn refuses_incomplete_commands_unmatched_stacks_and_nonfinite_coordinates() {
    let mut valid = begin(8, 8, 1000);
    path(&mut valid, false, &[1], 255);
    valid.push(dl::END);
    for cut in 0..valid.len() {
        assert!(
            render(&valid[..cut]).is_err(),
            "sat through an incomplete list at {cut}"
        );
    }
    let invalid = |suffix: &[u8]| {
        let mut invalid = begin(8, 8, 1000);
        invalid.extend(suffix);
        assert!(
            render(&invalid).is_err(),
            "accepted an invalid command sequence"
        );
    };
    invalid(&[dl::CLIP_POP, dl::END]);
    invalid(&[dl::END, dl::END]);
    invalid(&[dl::IMAGE, 1, 0, 0, 0]);
    invalid(&[dl::TEXT, 0, 0, 0, 0]);
    invalid(&[100, dl::END]);
    for number in [f32::NAN, f32::INFINITY, 1e20] {
        let mut bad = vec![dl::TRANSFORM_PUSH];
        floats(&mut bad, &[1., 0., 0., 1., number, 0.]);
        bad.extend([dl::TRANSFORM_POP, dl::END]);
        invalid(&bad);
    }
}

#[test]
fn refuses_byte_dimension_operation_and_stack_limits() {
    assert!(matches!(
        DisplayList::parse(&vec![0; dl::MAX_DISPLAY_LIST_BYTES + 1]),
        Err(RenderError::Limit)
    ));
    assert!(DisplayList::parse(&begin(4096, 2049, 1000)).is_err());
    assert!(DisplayList::parse(&begin(8, 8, 0)).is_err());
    let mut out = begin(1, 1, 1000);
    for _ in 0..=dl::MAX_COMMANDS {
        out.extend([dl::CLEAR, 1]);
    }
    out.push(dl::END);
    assert!(matches!(render(&out), Err(RenderError::Limit)));

    let mut out = begin(1, 1, 1000);
    for _ in 0..=dl::MAX_STACK_DEPTH {
        out.push(dl::CLIP_PUSH);
        floats(&mut out, &[0., 0., 1., 1.]);
    }
    assert!(matches!(render(&out), Err(RenderError::Limit)));
}

#[test]
fn uses_system_fallback_when_quicksand_lacks_glyphs() {
    use skia_safe::{FontMgr, FontStyle};

    let system = FontMgr::new();
    if ['北', '京'].into_iter().any(|ch| {
        system
            .match_family_style_character("sans-serif", FontStyle::normal(), &[], ch as i32)
            .is_none_or(|face| face.unichar_to_glyph(ch as i32) == 0)
    }) {
        return; // An OS image without a CJK font cannot exercise its fallback.
    }

    let list = |text: &str| {
        let mut out = begin(32, 32, 1000);
        out.push(dl::TEXT);
        floats(&mut out, &[22.]);
        out.push(8);
        floats(&mut out, &[3., 25.]);
        out.push(0);
        out.extend((text.len() as u16).to_le_bytes());
        out.extend(text.as_bytes());
        out.push(dl::END);
        out
    };
    let mut renderer = Renderer::default();
    let north = renderer
        .render(DisplayList::parse(&list("北")).unwrap())
        .unwrap()
        .to_vec();
    let capital = renderer
        .render(DisplayList::parse(&list("京")).unwrap())
        .unwrap();
    assert_ne!(
        north, capital,
        "different characters must not both render the missing-glyph box"
    );
    assert!(capital.chunks_exact(4).any(|rgba| rgba[3] == 255));
}
