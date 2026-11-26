//! A guest decodes a picture in its own helper (`peersus_v1.picture_decode`): the canvas helper has the decoder, the light
//! helper answers that it has none, and a guest's mistakes are answers, not faults.
use peersus_api::pictures::{DECODE_INVALID, DECODE_SHORT_OUTPUT, DECODE_UNSUPPORTED};
use peersus_plugin_runtime::{DataRequest, Transport};
use std::io;

/// No control messages and no data: the guest only calls the decoder and returns.
struct Quiet;

impl Transport for Quiet {
    type Stream = io::Cursor<Vec<u8>>;
    fn receive(&mut self, _into: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
    fn send(&mut self, _message: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn open_data(&mut self, _request: DataRequest) -> io::Result<Self::Stream> {
        Err(io::Error::other("no data"))
    }
    fn shutdown_data(&mut self, _stream: &mut Self::Stream) -> io::Result<()> {
        Ok(())
    }
}

/// A 2 by 1 PNG: one red pixel, one blue, both opaque.
const PNG: &str = "89504e470d0a1a0a0000000d4948445200000002000000010806000000f4227f8a0000000e49444154789c63f8cfc00042ff010ff903fd851199760000000049454e44ae426082";

fn escaped(hex: &str) -> String {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| format!("\\{}", std::str::from_utf8(pair).unwrap()))
        .collect()
}

/// A guest that decodes the PNG at 0 into 1024 and returns 0 when `expect` was answered (and, for a picture, its pixels are
/// right), otherwise a number saying which check failed.
fn guest(expect: i64, output_len: i32) -> Vec<u8> {
    let len = PNG.len() / 2;
    wat::parse_str(format!(
        r#"(module
            (import "peersus_v1" "picture_decode"
                (func $decode (param i32 i32 i32 i32 i32) (result i64)))
            (memory (export "memory") 1)
            (data (i32.const 0) "{png}")
            (func (export "peersus_main") (result i32)
                (local $answer i64)
                i32.const 0 i32.const {len} i32.const 256 i32.const 1024 i32.const {output_len}
                call $decode local.set $answer
                local.get $answer i64.const {expect} i64.ne if i32.const 1 return end
                local.get $answer i64.const 0 i64.lt_s if i32.const 0 return end
                ;; red, then blue
                i32.const 1024 i32.load8_u i32.const 255 i32.ne if i32.const 2 return end
                i32.const 1026 i32.load8_u i32.const 0 i32.ne if i32.const 3 return end
                i32.const 1030 i32.load8_u i32.const 255 i32.ne if i32.const 4 return end
                i32.const 1031 i32.load8_u i32.const 255 i32.ne if i32.const 5 return end
                i32.const 0))"#,
        png = escaped(PNG),
    ))
    .unwrap()
}

#[test]
fn the_canvas_helper_decodes_a_picture_into_the_guest_s_memory() {
    peersus_plugin_canvas::run(&guest(2 << 16 | 1, 8), &mut Quiet).unwrap();
}

#[test]
fn a_short_output_or_a_damaged_picture_is_an_answer_and_the_light_helper_has_no_decoder() {
    peersus_plugin_canvas::run(&guest(DECODE_SHORT_OUTPUT, 7), &mut Quiet).unwrap();
    peersus_plugin_runtime::run(&guest(DECODE_UNSUPPORTED, 8), &mut Quiet).unwrap();
    let damaged = r#"(module
        (import "peersus_v1" "picture_decode" (func $decode (param i32 i32 i32 i32 i32) (result i64)))
        (memory (export "memory") 1)
        (data (i32.const 0) "not a picture")
        (func (export "peersus_main") (result i32)
            i32.const 0 i32.const 13 i32.const 256 i32.const 1024 i32.const 4096 call $decode
            i64.const DAMAGED i64.ne))"#
        .replace("DAMAGED", &DECODE_INVALID.to_string());
    peersus_plugin_canvas::run(&wat::parse_str(damaged).unwrap(), &mut Quiet).unwrap();
}

#[test]
fn a_range_outside_the_guest_s_memory_ends_the_guest_and_not_the_helper() {
    let outside = r#"(module
        (import "peersus_v1" "picture_decode" (func $decode (param i32 i32 i32 i32 i32) (result i64)))
        (memory (export "memory") 1)
        (func (export "peersus_main") (result i32)
            i32.const 65530 i32.const 100 i32.const 256 i32.const 0 i32.const 16 call $decode drop
            i32.const 0))"#;
    assert!(peersus_plugin_canvas::run(&wat::parse_str(outside).unwrap(), &mut Quiet).is_err());
}
