//! Top-level integration test — proves the public surface wires up
//! end-to-end even while the shaping body is a stub.

use sigilbuzz::{shape, Blob, Buffer, Face, Font};

fn minimal_font_with_cmap() -> Vec<u8> {
    let mut bytes = Vec::new();
    // SFNT header: TrueType
    bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    bytes.extend_from_slice(&1u16.to_be_bytes()); // numTables
    bytes.extend_from_slice(&[0; 6]);

    // One table record: cmap, body at offset 28 (header 12 + record 16),
    // length 2.
    bytes.extend_from_slice(b"cmap");
    bytes.extend_from_slice(&0u32.to_be_bytes()); // checksum
    bytes.extend_from_slice(&28u32.to_be_bytes()); // offset
    bytes.extend_from_slice(&2u32.to_be_bytes()); // length

    // cmap body (placeholder bytes)
    bytes.extend_from_slice(&[0xAB, 0xCD]);
    bytes
}

#[test]
fn pipeline_runs_from_blob_to_shape() {
    let data = minimal_font_with_cmap();
    let blob = Blob::new(&data);
    let face = Face::parse(&blob, 0).expect("parse face");
    assert!(face.record(*b"cmap").is_some());

    let font = Font::new(face, 16.0);

    let mut buffer = Buffer::new();
    buffer.push_str("hello");

    let shaped = shape(&font, &buffer, &[]).expect("shape");
    // Shaping is not implemented yet, so the result is empty — the
    // interesting assertion is that the pipeline returned without
    // error.
    assert!(shaped.is_empty());
}
