//! A `Font` keeps lookup accelerators, resolved language systems and
//! per-glyph vertical metrics between shaping calls, from its second
//! call on. What they hold must never show in the output: a font shaped
//! with many times, from many threads at once, or rebound to other
//! coordinates gives what a fresh font gives.

use std::sync::Arc;

use sigilbuzz::{shape, Blob, Buffer, Direction, Face, Feature, Font};

/// A font fixture, the user-space axis values to shape it at, and the
/// scripts to draw its texts from.
struct Case {
    name: &'static str,
    data: &'static [u8],
    axes: &'static [([u8; 4], f32)],
}

const CASES: &[Case] = &[
    Case {
        name: "opensans",
        data: include_bytes!("fixtures/opensans_regular.ttf"),
        axes: &[],
    },
    Case {
        name: "amiri",
        data: include_bytes!("fixtures/amiri_regular.ttf"),
        axes: &[],
    },
    Case {
        name: "devanagari",
        data: include_bytes!("fonts/NotoSansDevanagari-Regular.ttf"),
        axes: &[],
    },
    Case {
        name: "source code pro (CFF, no VORG)",
        data: include_bytes!("fonts/SourceCodePro-Latin-Subset.otf"),
        axes: &[],
    },
    Case {
        name: "source sans 3 (CFF2, no VORG)",
        data: include_bytes!("fonts/SourceSans3VF-Latin-Subset.otf"),
        axes: &[(*b"wght", 650.0)],
    },
    Case {
        name: "hahmlet (glyf, gvar, no vmtx)",
        data: include_bytes!("fixtures/hahmlet_gvar_subset.ttf"),
        axes: &[(*b"wght", 700.0)],
    },
    Case {
        name: "rubik (glyf, gvar, no vmtx)",
        data: include_bytes!("fixtures/rubik_vf.ttf"),
        axes: &[(*b"wght", 420.0)],
    },
    Case {
        name: "noto sans kr (CFF2, VORG)",
        data: include_bytes!("fixtures/noto_sans_kr_vf_vertical_subset.otf"),
        axes: &[(*b"wght", 550.0)],
    },
    Case {
        name: "noto sans kr palt",
        data: include_bytes!("fonts/NotoSansKR-Palt-Subset.ttf"),
        axes: &[],
    },
];

/// Normalized coords for `axes`, through `fvar` and `avar`.
fn coords(face: &Face<'_>, axes: &[([u8; 4], f32)]) -> Vec<f32> {
    let Some(fvar) = face.fvar().unwrap() else {
        return Vec::new();
    };
    let user: Vec<f32> = fvar
        .axes()
        .iter()
        .map(|a| {
            axes.iter()
                .find(|(tag, _)| *tag == a.tag)
                .map_or(a.default_value, |&(_, v)| v)
        })
        .collect();
    let normalized = fvar.normalize_coords(&user);
    match face.avar().unwrap() {
        Some(avar) => avar.remap_all(&normalized),
        None => normalized,
    }
}

/// Texts of 1, 3, 8 and 30 characters the font maps, picked with a
/// fixed linear congruential generator.
fn texts(face: &Face<'_>) -> Vec<String> {
    let cmap = face.cmap().unwrap();
    let ranges = [
        0x20..0x7F,
        0x600..0x660,
        0x900..0x970,
        0xAC00..0xAE00,
        0x3000..0x3040,
    ];
    let pool: Vec<char> = ranges
        .into_iter()
        .flatten()
        .filter_map(char::from_u32)
        .filter(|&c| cmap.glyph_id(c).is_some())
        .collect();
    assert!(!pool.is_empty());
    let mut seed = 0x2545_F491u32;
    let mut next = |n: usize| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as usize % n
    };
    let mut out = Vec::new();
    for len in [1, 3, 8, 30] {
        for _ in 0..12 {
            out.push((0..len).map(|_| pool[next(pool.len())]).collect());
        }
    }
    out
}

type Shaped = Vec<(u32, u32, i32, i32, i32, i32)>;

fn run(font: &Font<'_>, text: &str, vertical: bool, features: &[Feature]) -> Shaped {
    let mut buffer = Buffer::new();
    buffer.push_str(text);
    buffer.guess_segment_properties();
    if vertical {
        buffer.set_direction(Direction::Ttb);
    }
    shape(font, &buffer, features)
        .unwrap()
        .glyphs
        .iter()
        .map(|g| {
            (
                g.glyph_id,
                g.cluster,
                g.x_advance,
                g.y_advance,
                g.x_offset,
                g.y_offset,
            )
        })
        .collect()
}

/// Each text shaped by a font of its own, which builds no cache.
fn fresh(
    face: &Face<'_>,
    coords: &[f32],
    texts: &[String],
    vertical: bool,
    features: &[Feature],
) -> Vec<Shaped> {
    texts
        .iter()
        .map(|t| {
            let font = Font::new(face.clone(), 1000.0).with_coords(coords);
            run(&font, t, vertical, features)
        })
        .collect()
}

const FEATURE_SETS: [&[Feature]; 3] = [
    &[],
    &[
        Feature {
            tag: *b"liga",
            value: 0,
        },
        Feature {
            tag: *b"kern",
            value: 0,
        },
    ],
    &[Feature {
        tag: *b"smcp",
        value: 1,
    }],
];

#[test]
fn a_font_shaped_with_again_shapes_like_a_fresh_one() {
    for case in CASES {
        let face = Face::parse_bytes(case.data, 0).unwrap();
        let coords = coords(&face, case.axes);
        let texts = texts(&face);
        let font = Font::new(face.clone(), 1000.0).with_coords(&coords);
        for vertical in [false, true] {
            for features in FEATURE_SETS {
                let expected = fresh(&face, &coords, &texts, vertical, features);
                // Twice over: the first round fills the caches, the
                // second reads them.
                for round in 0..2 {
                    for (text, want) in texts.iter().zip(&expected) {
                        let got = run(&font, text, vertical, features);
                        assert_eq!(
                            &got, want,
                            "{} {text:?} vertical={vertical} round {round}",
                            case.name
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn threads_sharing_one_font_agree() {
    for case in CASES {
        let face = Face::parse_bytes(case.data, 0).unwrap();
        let coords: Arc<[f32]> = coords(&face, case.axes).into();
        let texts = texts(&face);
        let expected: Vec<Vec<Shaped>> = [false, true]
            .iter()
            .map(|&v| fresh(&face, &coords, &texts, v, &[]))
            .collect();
        let font = Font::new(face, 1000.0).with_coords(&coords);
        std::thread::scope(|scope| {
            for t in 0..8 {
                let (font, texts, expected) = (&font, &texts, &expected);
                scope.spawn(move || {
                    for round in 0..4 {
                        // Each thread walks the texts in its own order.
                        for k in 0..texts.len() {
                            let i = (k * (2 * t + 1) + round) % texts.len();
                            let vertical = (i + t) % 2 == 1;
                            let got = run(font, &texts[i], vertical, &[]);
                            assert_eq!(
                                got,
                                expected[usize::from(vertical)][i],
                                "{} thread {t} text {i}",
                                case.name
                            );
                        }
                    }
                });
            }
        });
    }
}

#[test]
fn rebinding_coords_does_not_reuse_the_old_instance() {
    let case = CASES
        .iter()
        .find(|c| c.name.starts_with("hahmlet"))
        .unwrap();
    let face = Face::parse_bytes(case.data, 0).unwrap();
    let light = coords(&face, &[(*b"wght", 100.0)]);
    let heavy = coords(&face, &[(*b"wght", 900.0)]);
    let texts = texts(&face);
    let font = Font::new(face.clone(), 1000.0).with_coords(&light);
    for vertical in [true, false] {
        for text in &texts {
            run(&font, text, vertical, &[]);
            run(&font, text, vertical, &[]);
        }
    }
    let rebound = font.clone().with_coords(&heavy);
    let resized = font.with_size(12.0);
    for vertical in [true, false] {
        let want_heavy = fresh(&face, &heavy, &texts, vertical, &[]);
        let want_light = fresh(&face, &light, &texts, vertical, &[]);
        for (i, text) in texts.iter().enumerate() {
            assert_eq!(
                run(&rebound, text, vertical, &[]),
                want_heavy[i],
                "{text:?}"
            );
            assert_eq!(
                run(&resized, text, vertical, &[]),
                want_light[i],
                "{text:?}"
            );
        }
    }
}

#[test]
fn fonts_and_faces_stay_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<Font<'static>>();
    check::<Face<'static>>();
    check::<Blob<'static>>();
}
