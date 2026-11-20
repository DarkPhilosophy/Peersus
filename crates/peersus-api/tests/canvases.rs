use peersus_api::{
    canvas::{
        CanvasPalette, FRAME_PREFIX_BYTES, FrameHeader, FrameLayout, MAX_CANVAS_A11Y,
        MAX_CANVASES_PER_EXTENSION, MAX_FRAME_BYTES, MAX_FRAME_HEADER_BYTES,
    },
    elements::{Event, FEATURE_CANVAS, Node, PAGE_FEATURES},
};
use serde_json::json;

fn node(id: &str) -> serde_json::Value {
    json!({"k":"canvas", "id":id, "style":{"width":{"unit":"px","value":120}},
    "observe":["size","wheel"], "a11y":[
        {"id":"label", "role":"text", "label":"Published by the extension",
         "rect":{"x":1000,"y":2000,"width":6000,"height":3000}}
    ]})
}

#[test]
fn canvases_are_opt_in_bounded_page_nodes() {
    let valid = node("canvas");
    let tree = Node::parse(&valid).unwrap();
    assert_eq!(
        Node::parse(&serde_json::to_value(&tree).unwrap()).unwrap(),
        tree
    );
    assert!(!PAGE_FEATURES.contains(&FEATURE_CANVAS));

    let mut duplicate = node("canvas");
    duplicate["observe"] = json!(["size", "size"]);
    assert!(Node::parse(&duplicate).is_err());

    let mut too_many = node("canvas");
    too_many["a11y"] = json!(vec![valid["a11y"][0].clone(); MAX_CANVAS_A11Y + 1]);
    assert!(Node::parse(&too_many).is_err());

    let mut duplicate_id = node("canvas");
    duplicate_id["a11y"][0]["id"] = json!("canvas");
    assert!(Node::parse(&duplicate_id).is_err());
    let mut invalid_rect = node("canvas");
    invalid_rect["a11y"][0]["rect"]["width"] = json!(u32::MAX);
    assert!(Node::parse(&invalid_rect).is_err());
    assert!(Node::parse(&node(&"x".repeat(65))).is_err());

    let children: Vec<_> = (0..=MAX_CANVASES_PER_EXTENSION)
        .map(|i| json!({"k":"canvas","id":format!("canvas-{i}")}))
        .collect();
    assert!(Node::parse(&json!({"k":"column","children":children})).is_err());
    assert!(Node::parse(&json!({"k":"column","children":[valid, node("canvas")]})).is_err());
}

#[test]
fn canvas_size_keeps_old_size_strict_and_supplies_scale_and_theme() {
    let palette = CanvasPalette {
        dark: true,
        accent: [128, 82, 212, 255],
        ..Default::default()
    };
    let mut event = json!({"t":"canvas_size","id":"canvas","x":-2000,"y":3000,
        "width":80000,"height":60000,"scale":1500,"palette":palette});
    let parsed: Event = serde_json::from_value(event.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), event);
    event["scale"] = json!(0);
    assert!(
        serde_json::from_value::<Event>(event.clone())
            .unwrap()
            .validate()
            .is_err()
    );
    event["t"] = json!("size");
    assert!(serde_json::from_value::<Event>(event).is_err());
}

#[test]
fn frame_header_is_little_endian_and_framed_without_pixels() {
    let mut encoded = [0; MAX_FRAME_HEADER_BYTES];
    let header = FrameHeader {
        id: "map",
        layout: FrameLayout::new(0x123, 3, 0x490).unwrap(),
        seq: 0x0102_0304_0506_0708,
        settled: 0x8877_6655_4433_2211,
    };
    let wire = header.encode(&mut encoded).unwrap();
    assert_eq!(wire, b"PSF1\x03map\x23\x01\0\0\x03\0\0\0\x90\x04\0\0\x01\x08\x07\x06\x05\x04\x03\x02\x01\x11\x22\x33\x44\x55\x66\x77\x88");
    assert_eq!(
        FrameHeader::encoded_len(wire[..FRAME_PREFIX_BYTES].try_into().unwrap()).unwrap(),
        wire.len()
    );
    assert_eq!(FrameHeader::decode(wire).unwrap(), header);
    assert_eq!(header.layout.byte_len(), 0x490 * 3);
    for cut in 0..wire.len() {
        assert!(FrameHeader::decode(&wire[..cut]).is_err());
    }
    let mut bad = wire.to_vec();
    bad[5] = 0xff;
    assert!(FrameHeader::decode(&bad).is_err());
    let mut bad = wire.to_vec();
    bad[20] = 2;
    assert!(FrameHeader::decode(&bad).is_err());
    let mut bad = wire.to_vec();
    bad.push(0);
    assert!(FrameHeader::decode(&bad).is_err());
}

#[test]
fn frame_layout_rejects_oversize_and_invalid_strides_before_reading_pixels() {
    assert_eq!(
        FrameLayout::tight(4096, 2048).unwrap().byte_len(),
        MAX_FRAME_BYTES
    );
    assert!(FrameLayout::tight(4096, 2049).is_err());
    assert!(FrameLayout::tight(4097, 1).is_err());
    assert!(FrameLayout::tight(0, 1).is_err());
    assert!(FrameLayout::new(10, 10, 36).is_err());
    assert!(FrameLayout::new(10, 10, 41).is_err());
    assert!(FrameLayout::new(10, 10, u32::MAX - 3).is_err());
    assert!(
        FrameHeader {
            id: "map",
            layout: FrameLayout::tight(1, 1).unwrap(),
            seq: 0,
            settled: 0
        }
        .encode(&mut [0; MAX_FRAME_HEADER_BYTES])
        .is_err()
    );
}
