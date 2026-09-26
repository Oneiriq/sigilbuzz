//! `sigilbuzz info`: dump font metadata.
//!
//! Output:
//!
//! - `num_glyphs`, `units_per_em`
//! - `sfnt_version` (4-byte tag)
//! - List of OT tables, sorted by tag
//! - GSUB / GPOS feature tags, deduped, sorted

use std::path::PathBuf;

use clap::Args as ClapArgs;

use sigilbuzz::{Blob, Face};

use super::util::{read_font, tag_to_string, CliResult};

/// Arguments for `sigilbuzz info`.
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Path to the font file.
    pub font: PathBuf,
}

/// Runs `sigilbuzz info`.
pub fn run(args: Args) -> CliResult {
    let bytes = read_font(&args.font)?;
    let blob = Blob::from_vec(bytes);
    let face = Face::parse(&blob, 0).map_err(|e| format!("parse face: {e:?}"))?;

    let sfnt = face.sfnt_version();
    let sfnt_tag = sfnt.to_be_bytes();
    println!("path: {}", args.font.display());
    println!("sfnt_version: 0x{sfnt:08X} ({})", tag_to_string(sfnt_tag));

    if let Ok(maxp) = face.maxp() {
        println!("num_glyphs: {}", maxp.num_glyphs);
    }
    if let Ok(head) = face.head() {
        println!("units_per_em: {}", head.units_per_em);
    }

    // OT tables. records() is in directory order, but we sort by
    // tag for stable, diff-friendly output.
    let mut tags: Vec<[u8; 4]> = face.records().iter().map(|r| r.tag).collect();
    tags.sort_unstable();
    println!("tables ({}):", tags.len());
    for tag in &tags {
        println!("  {}", tag_to_string(*tag));
    }

    // GSUB / GPOS feature tags.
    let mut features: Vec<[u8; 4]> = Vec::new();
    if let Ok(Some(gsub)) = face.gsub() {
        for (tag, _f) in gsub.feature_list().iter() {
            features.push(tag);
        }
    }
    if let Ok(Some(gpos)) = face.gpos() {
        for (tag, _f) in gpos.feature_list().iter() {
            features.push(tag);
        }
    }
    features.sort_unstable();
    features.dedup();
    if !features.is_empty() {
        println!("features ({}):", features.len());
        for tag in &features {
            println!("  {}", tag_to_string(*tag));
        }
    }

    Ok(())
}
