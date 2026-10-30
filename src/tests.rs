#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::{font, io, layout::DonorLayout};
use read_fonts::{
    FontRef, TableProvider,
    types::{GlyphId, Tag},
};
use skrifa::MetadataProvider;
use std::{collections::BTreeMap, fs, path::Path};
use write_fonts::{FontBuilder, from_obj::ToOwnedTable};

const PROPORTIONAL: &[u8] = include_bytes!("../tests/ABeeZee-Regular.ttf");

fn rebuild(font: &FontRef<'_>, changes: &[(Tag, Option<Vec<u8>>)]) -> Vec<u8> {
    let mut builder = FontBuilder::new();
    for r in font.table_directory().table_records() {
        if !changes.iter().any(|(t, _)| *t == r.tag()) {
            builder.add_raw(r.tag(), font.data_for_tag(r.tag()).unwrap());
        }
    }
    for (tag, bytes) in changes {
        if let Some(bytes) = bytes {
            builder.add_raw(*tag, bytes.clone());
        }
    }
    builder.build()
}

fn shape(bytes: &[u8], text: &str, features: &str) -> Vec<(u32, u32, i32, i32, i32, i32)> {
    let face = rustybuzz::Face::from_slice(bytes, 0).unwrap();
    let features: Vec<_> = features
        .split(',')
        .filter(|f| !f.is_empty())
        .map(|f| f.parse::<rustybuzz::Feature>().unwrap())
        .collect();
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    let buffer = rustybuzz::shape(&face, &features, buffer);
    buffer
        .glyph_infos()
        .iter()
        .zip(buffer.glyph_positions())
        .map(|(g, p)| {
            (
                g.glyph_id,
                g.cluster,
                p.x_advance,
                p.y_advance,
                p.x_offset,
                p.y_offset,
            )
        })
        .collect()
}

#[test]
fn transfers_entire_upstream_default_corpus_and_long_arrows() {
    for (_, _, donor_bytes) in font::DONORS {
        let donor = FontRef::new(donor_bytes).unwrap();
        let input = rebuild(&donor, &[(Tag::new(b"GSUB"), None)]);
        let result = font::patch(&input, |_| {}).unwrap();
        assert_eq!(
            shape(&input, "ASCII != -> ===", "calt=0"),
            shape(&result.bytes, "ASCII != -> ===", "calt=0")
        );
        let layout = DonorLayout::extract(&donor.gsub().unwrap().to_owned_table()).unwrap();
        let start = donor.maxp().unwrap().num_glyphs();
        let mapping: BTreeMap<_, _> = layout
            .glyphs
            .iter()
            .enumerate()
            .map(|(i, g)| (g.to_u32(), u32::from(start) + i as u32))
            .collect();
        let encoded: BTreeMap<_, _> = donor
            .charmap()
            .mappings()
            .map(|(_, g)| (g.to_u32(), ()))
            .collect();
        let corpus = include_str!("../assets/fira-code/showcases.txt");
        let long = format!(
            "<{}> <{}> [{}]",
            "-".repeat(160),
            "=".repeat(160),
            "|".repeat(160)
        );
        let edge_cases = [
            "----->>>> <===>=> ==== !=== !=!= *** //// .!? == ==",
            "a->b b!=c x===y",
            &long,
        ];
        for text in corpus.lines().chain(edge_cases) {
            let actual = shape(&result.bytes, text, "kern=0,ccmp=0,locl=0");
            let mut expected = shape(donor_bytes, text, "kern=0,ccmp=0,locl=0");
            for glyph in &mut expected {
                if !encoded.contains_key(&glyph.0) {
                    glyph.0 = *mapping.get(&glyph.0).unwrap_or(&glyph.0);
                }
            }
            assert_eq!(actual, expected, "weight {}, text {text:?}", result.weight);
        }
    }
}

#[test]
fn existing_features_plain_text_and_disabled_calt_survive() {
    for bytes in [font::DONORS[1].2, PROPORTIONAL] {
        let output = font::patch(bytes, |_| {}).unwrap();
        for features in ["calt=0", "calt=0,liga=0", "calt=0,liga=1", "calt=0,smcp=1"] {
            for text in [
                "office affinity fi fj Fl",
                "ASCII != -> ===",
                "Áé Ελληνικά Кириллица",
            ] {
                assert_eq!(
                    shape(bytes, text, features),
                    shape(&output.bytes, text, features),
                    "{text} {features}"
                );
            }
        }
        assert_eq!(
            shape(bytes, "Hello world 0123456789", ""),
            shape(&output.bytes, "Hello world 0123456789", "")
        );
    }
}

#[test]
fn original_glyph_bytes_cmap_metrics_and_instructions_survive() {
    let input = FontRef::new(PROPORTIONAL).unwrap();
    let patched = font::patch(PROPORTIONAL, |_| {}).unwrap();
    let output = FontRef::new(&patched.bytes).unwrap();
    assert_eq!(
        input.data_for_tag(Tag::new(b"cmap")).unwrap().as_bytes(),
        output.data_for_tag(Tag::new(b"cmap")).unwrap().as_bytes()
    );
    let n = usize::from(input.maxp().unwrap().num_glyphs());
    let old_loca = input.loca(None).unwrap();
    let new_loca = output.loca(None).unwrap();
    let old_glyf = input.data_for_tag(Tag::new(b"glyf")).unwrap();
    let new_glyf = output.data_for_tag(Tag::new(b"glyf")).unwrap();
    for i in 0..n {
        assert_eq!(old_loca.get_raw(i), new_loca.get_raw(i));
        let range =
            old_loca.get_raw(i).unwrap() as usize..old_loca.get_raw(i + 1).unwrap() as usize;
        assert_eq!(
            &old_glyf.as_bytes()[range.clone()],
            &new_glyf.as_bytes()[range]
        );
        let g = GlyphId::new(i as u32);
        assert_eq!(
            input.hmtx().unwrap().advance(g),
            output.hmtx().unwrap().advance(g)
        );
        assert_eq!(
            input.hmtx().unwrap().side_bearing(g),
            output.hmtx().unwrap().side_bearing(g)
        );
    }
    let checksum = patched.bytes.chunks(4).fold(0u32, |s, c| {
        let mut b = [0; 4];
        b[..c.len()].copy_from_slice(c);
        s.wrapping_add(u32::from_be_bytes(b))
    });
    assert_eq!(checksum, 0xb1b0afba);
}

#[test]
fn proportional_ligatures_use_target_cell_width_and_em_height() {
    let input = FontRef::new(PROPORTIONAL).unwrap();
    let patched = font::patch(PROPORTIONAL, |_| {}).unwrap();
    let output = FontRef::new(&patched.bytes).unwrap();
    let donor = FontRef::new(font::DONORS[1].2).unwrap();
    let layout = DonorLayout::extract(&donor.gsub().unwrap().to_owned_table()).unwrap();
    let encoded: BTreeMap<_, _> = donor
        .charmap()
        .mappings()
        .map(|(_, g)| (g.to_u32(), ()))
        .collect();
    let cell = input
        .hmtx()
        .unwrap()
        .advance(input.charmap().map('0').unwrap())
        .unwrap();
    let donor_cell = donor
        .hmtx()
        .unwrap()
        .advance(donor.charmap().map('0').unwrap())
        .unwrap();
    let (sx, sy) = (
        f32::from(cell) / f32::from(donor_cell),
        f32::from(input.head().unwrap().units_per_em())
            / f32::from(donor.head().unwrap().units_per_em()),
    );
    let mut compared = 0;
    for (i, old) in layout.glyphs.iter().enumerate() {
        if encoded.contains_key(&old.to_u32()) {
            continue;
        }
        let new = GlyphId::new(u32::from(input.maxp().unwrap().num_glyphs()) + i as u32);
        let width = donor.hmtx().unwrap().advance((*old).into()).unwrap();
        assert_eq!(
            output.hmtx().unwrap().advance(new),
            Some((f32::from(width) * sx).round() as u16)
        );
        let donor_bbox = donor
            .loca(None)
            .unwrap()
            .get((*old).into(), &donor.glyf().unwrap())
            .unwrap();
        let out_bbox = output
            .loca(None)
            .unwrap()
            .get(new, &output.glyf().unwrap())
            .unwrap();
        if let (Some(d), Some(o)) = (donor_bbox.glyph(), out_bbox.glyph()) {
            assert!(
                (f32::from(o.y_max()) - f32::from(d.y_max()) * sy).abs() <= 2.0,
                "glyph {old}"
            );
            assert!(
                (f32::from(o.x_max()) - f32::from(d.x_max()) * sx).abs() <= 2.0,
                "glyph {old}"
            );
        }
        compared += 1;
    }
    assert!(compared > 100);
}

#[test]
fn missing_punctuation_reduces_coverage_without_notdef_outputs() {
    let input = FontRef::new(PROPORTIONAL).unwrap();
    let chars = [' ', '0', '!', '=', '-', '>']
        .into_iter()
        .map(|c| (c, input.charmap().map(c).unwrap()));
    let cmap = write_fonts::tables::cmap::Cmap::from_mappings(chars).unwrap();
    let input = rebuild(
        &input,
        &[(
            Tag::new(b"cmap"),
            Some(write_fonts::dump_table(&cmap).unwrap()),
        )],
    );
    let output = font::patch(&input, |_| {}).unwrap();
    assert!(output.missing_characters.contains(&'<'));
    assert!(shape(&output.bytes, "!= ->", "").iter().all(|g| g.0 != 0));
}

#[test]
fn malformed_unsupported_and_repeated_fonts_fail_cleanly() {
    assert!(font::patch(b"not a font", |_| {}).is_err());
    let input = FontRef::new(PROPORTIONAL).unwrap();
    for tag in [b"fvar", b"morx", b"vmtx", b"COLR"] {
        let bytes = rebuild(&input, &[(Tag::new(tag), Some(vec![0; 16]))]);
        assert!(
            format!("{:#}", font::patch(&bytes, |_| {}).err().unwrap()).contains("unsupported")
        );
    }
    let patched = font::patch(PROPORTIONAL, |_| {}).unwrap();
    assert!(
        font::patch(&patched.bytes, |_| {})
            .err()
            .unwrap()
            .to_string()
            .contains("already")
    );
    let broken = rebuild(&input, &[(Tag::new(b"loca"), Some(vec![0; 4]))]);
    assert!(font::patch(&broken, |_| {}).is_err());
}

#[test]
fn writes_are_atomic_and_never_overwrite_inputs_or_colliding_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("Input.ttf");
    let output = dir.path().join("Input-Liga.ttf");
    fs::write(&source, b"original").unwrap();
    let inputs = vec![source.clone()];
    io::save_font(&output, b"first", &inputs, false).unwrap();
    assert!(io::save_font(&output, b"second", &inputs, false).is_err());
    assert_eq!(fs::read(&output).unwrap(), b"first");
    io::save_font(&output, b"second", &inputs, true).unwrap();
    assert_eq!(fs::read(&output).unwrap(), b"second");
    assert!(io::save_font(&source, b"destroyed", &inputs, true).is_err());
    assert!(io::jobs(&[source.clone(), output.clone()], None).is_err());
    assert_eq!(fs::read(&source).unwrap(), b"original");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    let other = dir.path().join("other");
    fs::create_dir(&other).unwrap();
    let duplicate = other.join("Input.ttf");
    fs::write(&duplicate, b"other").unwrap();
    assert!(io::jobs(&[source.clone(), duplicate], Some(dir.path())).is_err());
    fs::remove_file(&output).unwrap();
    fs::hard_link(&source, &output).unwrap();
    assert!(io::save_font(&output, b"destroyed", &inputs, true).is_err());
    #[cfg(unix)]
    {
        fs::remove_file(&output).unwrap();
        std::os::unix::fs::symlink(&source, &output).unwrap();
        assert!(io::save_font(&output, b"destroyed", &inputs, true).is_err());
    }
}

#[test]
fn read_limits_and_invalid_paths_fail_without_large_allocations() {
    let dir = tempfile::tempdir().unwrap();
    assert!(io::read_font(dir.path()).is_err());
    assert!(io::read_font(Path::new("/nonexistent/ligaturize-font.ttf")).is_err());
    let large = dir.path().join("large.ttf");
    fs::File::create(&large)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    assert!(io::read_font(&large).is_err());
}

#[test]
fn script_specific_calt_is_not_enabled_for_other_scripts() {
    use write_fonts::tables::{
        gsub::{Gsub, SubstitutionLookupList},
        layout::{Feature, FeatureList, FeatureRecord, LangSys, Script, ScriptList, ScriptRecord},
    };
    let source = FontRef::new(PROPORTIONAL).unwrap();
    let a = source.charmap().map('a').unwrap().try_into().unwrap();
    let b = source.charmap().map('b').unwrap().try_into().unwrap();
    let gsub = Gsub::new(
        ScriptList::new(vec![ScriptRecord::new(
            Tag::new(b"arab"),
            Script {
                default_lang_sys: Some(LangSys::new(vec![0])).into(),
                ..Default::default()
            },
        )]),
        FeatureList::new(vec![FeatureRecord::new(
            Tag::new(b"calt"),
            Feature::new(None, vec![0]),
        )]),
        SubstitutionLookupList::new(vec![crate::layout::single_lookup([(a, b)])]),
    );
    let input = rebuild(
        &source,
        &[(
            Tag::new(b"GSUB"),
            Some(write_fonts::dump_table(&gsub).unwrap()),
        )],
    );
    let patched = font::patch(&input, |_| {}).unwrap();
    assert_eq!(shape(&input, "a", ""), shape(&patched.bytes, "a", ""));
}
