use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use read_fonts::{
    FontRef, TableProvider,
    types::{GlyphId, GlyphId16, NameId, Tag},
};
use skrifa::{
    MetadataProvider,
    instance::{LocationRef, Size},
    outline::{DrawSettings, OutlinePen},
};
use write_fonts::{
    FontBuilder,
    from_obj::ToOwnedTable,
    tables::{
        gdef::Gdef,
        glyf::{Bbox, Contour, Glyph, SimpleGlyph},
        gsub::Gsub,
        hmtx::{Hmtx, LongMetric},
        loca::Loca,
        name::{Name, NameRecord},
    },
};

use crate::layout::{self, DonorLayout};

pub const MARKER: &str = "Patched with ligaturize";
const DONOR_LICENSE: &str = include_str!("../assets/fira-code/LICENSE");
pub const DONORS: &[(u16, &str, &[u8])] = &[
    (
        300,
        "Light",
        include_bytes!("../assets/fira-code/FiraCode-Light.ttf"),
    ),
    (
        400,
        "Regular",
        include_bytes!("../assets/fira-code/FiraCode-Regular.ttf"),
    ),
    (
        450,
        "Retina",
        include_bytes!("../assets/fira-code/FiraCode-Retina.ttf"),
    ),
    (
        500,
        "Medium",
        include_bytes!("../assets/fira-code/FiraCode-Medium.ttf"),
    ),
    (
        600,
        "SemiBold",
        include_bytes!("../assets/fira-code/FiraCode-SemiBold.ttf"),
    ),
    (
        700,
        "Bold",
        include_bytes!("../assets/fira-code/FiraCode-Bold.ttf"),
    ),
];

pub struct PatchedFont {
    pub bytes: Vec<u8>,
    pub family: String,
    pub weight: &'static str,
    pub added_glyphs: usize,
    pub missing_characters: Vec<char>,
}

pub fn patch(bytes: &[u8], mut stage: impl FnMut(&str)) -> Result<PatchedFont> {
    stage("Reading font");
    let target =
        FontRef::new(bytes).context("expected a standalone static TrueType font (.ttf)")?;
    validate(&target)?;
    let mut name: Name = target.name()?.to_owned_table();
    ensure!(
        !name.name_record.iter().any(|r| r.string.contains(MARKER)),
        "this font is already ligaturized"
    );
    let family = rename(&mut name)?;
    let target_os2 = target.os2()?;
    let (_, weight, donor_bytes) = DONORS
        .iter()
        .min_by_key(|(w, _, _)| w.abs_diff(target_os2.us_weight_class()))
        .context("no bundled donor weights")?;
    let donor = FontRef::new(donor_bytes)?;
    let mut patch_layout = DonorLayout::extract(&donor.gsub()?.to_owned_table())?;
    let mut head: write_fonts::tables::head::Head = target.head()?.to_owned_table();
    let mut hhea: write_fonts::tables::hhea::Hhea = target.hhea()?.to_owned_table();
    let mut maxp: write_fonts::tables::maxp::Maxp = target.maxp()?.to_owned_table();
    let old_count = maxp.num_glyphs;
    ensure!(
        usize::from(old_count) + patch_layout.glyphs.len() <= usize::from(u16::MAX),
        "adding ligatures would exceed TrueType's glyph limit"
    );
    let glyph_map: BTreeMap<_, _> = patch_layout
        .glyphs
        .iter()
        .enumerate()
        .map(|(i, g)| {
            Ok((
                *g,
                GlyphId16::new(u16::try_from(usize::from(old_count) + i)?),
            ))
        })
        .collect::<Result<_>>()?;

    let target_chars = target.charmap();
    let donor_chars = donor.charmap();
    let mut encoded = BTreeSet::new();
    let mut base_map = BTreeMap::new();
    let mut used_targets = BTreeSet::new();
    let mut missing_characters = Vec::new();
    for (cp, donor_glyph) in donor_chars.mappings() {
        let donor_glyph = GlyphId16::try_from(donor_glyph)?;
        if !glyph_map.contains_key(&donor_glyph) {
            continue;
        }
        encoded.insert(donor_glyph);
        if base_map.contains_key(&donor_glyph) {
            continue;
        }
        if let Some(target_glyph) = target_chars.map(cp).filter(|g| g.to_u32() != 0) {
            let target_glyph = GlyphId16::try_from(target_glyph)?;
            // Shared Unicode aliases must never collapse two original glyphs during the round trip.
            if used_targets.insert(target_glyph) {
                base_map.insert(donor_glyph, target_glyph);
            }
        } else if let Some(c) = char::from_u32(cp).filter(|c| c.is_ascii_punctuation()) {
            missing_characters.push(c);
        }
    }
    ensure!(
        base_map.len() >= 2,
        "font has too few characters used by Fira Code's programming ligatures"
    );
    let target_metrics = target.hmtx()?;
    let donor_metrics = donor.hmtx()?;
    let cell_width = ['0', ' ']
        .into_iter()
        .filter_map(|c| target_chars.map(c))
        .filter_map(|g| target_metrics.advance(g))
        .find(|w| *w > 0)
        .unwrap_or(head.units_per_em / 2);
    let donor_cell = donor_metrics
        .advance(donor_chars.map('0').context("donor lacks zero")?)
        .context("missing donor metrics")?;
    ensure!(
        cell_width > 0 && donor_cell > 0,
        "font has invalid advance widths"
    );
    let sx = f32::from(cell_width) / f32::from(donor_cell);
    let sy = f32::from(head.units_per_em) / f32::from(donor.head()?.units_per_em());

    stage("Transferring Fira Code outlines");
    let original_loca = target.loca(None)?;
    let mut offsets = (0..=usize::from(old_count))
        .map(|i| {
            original_loca
                .get_raw(i)
                .context("truncated glyph locations")
        })
        .collect::<Result<Vec<_>>>()?;
    let original_glyf = target
        .data_for_tag(Tag::new(b"glyf"))
        .context("missing glyph outlines")?;
    let end = usize::try_from(*offsets.last().context("empty glyph locations")?)?;
    let mut glyf = original_glyf
        .as_bytes()
        .get(..end)
        .context("glyph locations extend beyond outlines")?
        .to_vec();
    let mut metrics = (0..old_count)
        .map(|g| {
            let id = GlyphId::new(u32::from(g));
            Ok(LongMetric::new(
                target_metrics
                    .advance(id)
                    .context("missing advance width")?,
                target_metrics
                    .side_bearing(id)
                    .context("missing side bearing")?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let outlines = donor.outline_glyphs();
    for old in &patch_layout.glyphs {
        let (glyph, advance) = if encoded.contains(old) {
            let advance = base_map
                .get(old)
                .and_then(|g| target_metrics.advance((*g).into()))
                .unwrap_or(cell_width);
            (Glyph::Empty, advance)
        } else {
            let outline = outlines
                .get((*old).into())
                .context("missing donor outline")?;
            let mut pen = Pen::new(sx, sy);
            outline.draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                &mut pen,
            )?;
            let (glyph, points, contours) = pen.finish()?;
            maxp.max_points = Some(maxp.max_points.unwrap_or(0).max(u16::try_from(points)?));
            maxp.max_contours = Some(maxp.max_contours.unwrap_or(0).max(u16::try_from(contours)?));
            let width = donor_metrics
                .advance((*old).into())
                .context("missing donor advance")?;
            let advance = (f32::from(width) * sx).round();
            ensure!(
                (0.0..=f32::from(u16::MAX)).contains(&advance),
                "scaled advance exceeds TrueType limits"
            );
            (glyph, advance as u16)
        };
        let bbox = glyph.bbox().unwrap_or_default();
        if glyph.bbox().is_some() {
            head.x_min = head.x_min.min(bbox.x_min);
            head.x_max = head.x_max.max(bbox.x_max);
            head.y_min = head.y_min.min(bbox.y_min);
            head.y_max = head.y_max.max(bbox.y_max);
        }
        hhea.advance_width_max = hhea.advance_width_max.max(advance.into());
        hhea.min_left_side_bearing = hhea.min_left_side_bearing.min(bbox.x_min.into());
        let rsb = i32::from(advance) - i32::from(bbox.x_max);
        hhea.min_right_side_bearing = hhea.min_right_side_bearing.min(
            i16::try_from(rsb)
                .context("side bearing exceeds TrueType limits")?
                .into(),
        );
        hhea.x_max_extent = hhea.x_max_extent.max(bbox.x_max.into());
        glyf.extend(write_fonts::dump_table(&glyph)?);
        if glyf.len() % 2 != 0 {
            glyf.push(0);
        }
        offsets.push(u32::try_from(glyf.len())?);
        metrics.push(LongMetric::new(advance, bbox.x_min));
    }
    let loca = Loca::new(offsets);
    head.index_to_loc_format = loca.format() as i16;
    maxp.num_glyphs = u16::try_from(metrics.len())?;
    hhea.number_of_h_metrics = maxp.num_glyphs;

    stage("Merging contextual ligatures");
    patch_layout.remap(&glyph_map)?;
    // Temporarily enter the donor's glyph namespace, then restore every unchanged base glyph.
    // This keeps coverage ordering and class-zero guards exact without renumbering target glyphs.
    let enter = layout::single_lookup(base_map.iter().map(|(d, t)| (*t, glyph_map[d])));
    let exit = layout::single_lookup(base_map.iter().map(|(d, t)| (glyph_map[d], *t)));
    let target_gsub = if target.data_for_tag(Tag::new(b"GSUB")).is_some() {
        target.gsub()?.to_owned_table()
    } else {
        Gsub::default()
    };
    let gsub = layout::merge(target_gsub, patch_layout, enter, exit)?;

    let mut builder = FontBuilder::new();
    // Device caches and signatures no longer describe the modified font.
    for record in target.table_directory().table_records() {
        let tag = record.tag();
        if ![b"DSIG", b"LTSH", b"hdmx", b"VDMX"].contains(&&tag.to_be_bytes()) {
            builder.add_raw(
                tag,
                target
                    .data_for_tag(tag)
                    .context("invalid table directory")?,
            );
        }
    }
    let mut post: write_fonts::tables::post::Post = target.post()?.to_owned_table();
    post.version = read_fonts::types::Version16Dot16::VERSION_3_0;
    post.num_glyphs = None;
    post.glyph_name_index = None;
    post.string_data = None;
    let mut os2: write_fonts::tables::os2::Os2 = target_os2.to_owned_table();
    os2.us_win_ascent = os2.us_win_ascent.max(head.y_max.max(0) as u16);
    os2.us_win_descent = os2.us_win_descent.max(head.y_min.min(0).unsigned_abs());
    let mut gdef: Gdef = if target.data_for_tag(Tag::new(b"GDEF")).is_some() {
        target.gdef()?.to_owned_table()
    } else {
        Gdef::default()
    };
    ensure!(
        gdef.glyph_class_def2.is_none() && gdef.item_var_store.is_none(),
        "extended or variable glyph classifications are not supported yet"
    );
    let mut classes: BTreeMap<_, _> = gdef
        .glyph_class_def
        .as_ref()
        .into_iter()
        .flat_map(|c| c.iter())
        .map(|(g, c)| Ok((GlyphId16::try_from(g)?, u16::try_from(c)?)))
        .collect::<Result<_>>()?;
    let donor_gdef: Gdef = donor.gdef()?.to_owned_table();
    for (old, new) in &glyph_map {
        let class = donor_gdef
            .glyph_class_def
            .as_ref()
            .map_or(1, |c| c.get(*old));
        classes.insert(*new, u16::try_from(class)?);
    }
    gdef.glyph_class_def = Some(classes.into_iter().collect()).into();
    builder.add_raw(Tag::new(b"glyf"), glyf);
    builder
        .add_table(&loca)?
        .add_table(&head)?
        .add_table(&hhea)?
        .add_table(&maxp)?
        .add_table(&Hmtx::new(metrics, vec![]))?
        .add_table(&name)?
        .add_table(&post)?
        .add_table(&os2)?
        .add_table(&gsub)?
        .add_table(&gdef)?;
    stage("Validating patched font");
    let bytes = builder.build();
    let output = FontRef::new(&bytes)?;
    validate(&output)?;
    ensure!(
        output.maxp()?.num_glyphs() == maxp.num_glyphs,
        "output glyph count mismatch"
    );
    let face = rustybuzz::Face::from_slice(&bytes, 0).context("patched font cannot be shaped")?;
    let works = include_str!("../assets/fira-code/showcases.txt")
        .lines()
        .take_while(|l| !l.starts_with("# Powerline"))
        .any(|line| {
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(line);
            rustybuzz::shape(&face, &[], buffer)
                .glyph_infos()
                .iter()
                .any(|g| g.glyph_id >= u32::from(old_count))
        });
    ensure!(
        works,
        "font lacks the characters needed to form any default programming ligatures"
    );
    Ok(PatchedFont {
        bytes,
        family,
        weight,
        added_glyphs: glyph_map.len(),
        missing_characters,
    })
}

fn validate(font: &FontRef<'_>) -> Result<()> {
    ensure!(
        font.data_for_tag(Tag::new(b"glyf")).is_some(),
        "only TrueType glyf outlines are supported; CFF/OTF fonts are not"
    );
    for tag in [
        b"fvar", b"gvar", b"CFF2", b"VARC", b"HVAR", b"VVAR", b"MVAR", b"morx", b"mort", b"COLR",
        b"sbix", b"CBDT", b"EBDT", b"SVG ", b"vhea", b"vmtx",
    ] {
        ensure!(
            font.data_for_tag(Tag::new(tag)).is_none(),
            "unsupported {} table; use a static, horizontal TrueType font",
            String::from_utf8_lossy(tag)
        );
    }
    let head = font.head()?;
    ensure!(
        (16..=16384).contains(&head.units_per_em()),
        "invalid units per em"
    );
    let count = font.maxp()?.num_glyphs();
    ensure!(count > 0, "font has no glyphs");
    let loca = font.loca(None)?;
    let glyf = font.glyf()?;
    let metrics = font.hmtx()?;
    let mut previous = 0;
    for i in 0..=count {
        let offset = loca
            .get_raw(usize::from(i))
            .context("truncated glyph locations")?;
        ensure!(
            offset >= previous
                && offset as usize <= font.data_for_tag(Tag::new(b"glyf")).map_or(0, |d| d.len()),
            "invalid glyph locations"
        );
        if i < count {
            let id = GlyphId::new(u32::from(i));
            ensure!(
                metrics.side_bearing(id).is_some(),
                "truncated glyph metrics"
            );
            let glyph = loca.get(id, &glyf).context("malformed glyph outline")?;
            if let Some(read_fonts::tables::glyf::Glyph::Composite(g)) = glyph.glyph() {
                ensure!(
                    g.component_glyphs_and_flags()
                        .all(|(g, _)| g.to_u32() < u32::from(count)),
                    "invalid composite glyph reference"
                );
            }
        }
        previous = offset;
    }
    font.cmap()?;
    font.name()?;
    font.os2()?;
    font.post()?;
    Ok(())
}

fn rename(name: &mut Name) -> Result<String> {
    let original = [16, 1]
        .into_iter()
        .find_map(|id| {
            name.name_record
                .iter()
                .find(|r| r.name_id.to_u16() == id && r.platform_id == 3)
                .or_else(|| name.name_record.iter().find(|r| r.name_id.to_u16() == id))
                .map(|r| r.string.to_string())
        })
        .context("font has no family name")?;
    let family = format!("Liga {original}");
    for record in &mut name.name_record {
        match record.name_id.to_u16() {
            1 | 4 | 16 | 18 | 21 => {
                record.string = format!("Liga {}", record.string.as_str()).into()
            }
            3 => {
                record.string = format!(
                    "ligaturize-{};{}",
                    env!("CARGO_PKG_VERSION"),
                    record.string.as_str()
                )
                .into()
            }
            6 => {
                record.string = format!("Liga-{}", record.string.as_str())
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .take(63)
                    .collect::<String>()
                    .into()
            }
            0 => {
                record.string = format!(
                    "{}\nFira Code portions: Copyright (c) 2014, The Fira Code Project Authors.",
                    record.string.as_str()
                )
                .into()
            }
            _ => (),
        }
    }
    for (id, text) in [
        (
            10,
            format!(
                "{MARKER} {}; Fira Code 6.2 programming ligatures.",
                env!("CARGO_PKG_VERSION")
            ),
        ),
        (13, format!("Fira Code portions:\n{DONOR_LICENSE}")),
    ] {
        if let Some(record) = name
            .name_record
            .iter_mut()
            .find(|r| r.name_id.to_u16() == id && r.platform_id == 3 && r.language_id == 0x409)
        {
            record.string = format!("{}\n\n{text}", record.string.as_str()).into();
        } else {
            name.name_record
                .push(NameRecord::new(3, 1, 0x409, NameId::new(id), text.into()));
        }
    }
    name.name_record.sort();
    Ok(family)
}

struct Pen {
    sx: f32,
    sy: f32,
    contours: Vec<Contour>,
    current: Vec<read_fonts::tables::glyf::CurvePoint>,
    bbox: Option<Bbox>,
    points: usize,
    invalid: bool,
}

impl Pen {
    fn new(sx: f32, sy: f32) -> Self {
        Self {
            sx,
            sy,
            contours: vec![],
            current: vec![],
            bbox: None,
            points: 0,
            invalid: false,
        }
    }
    fn point(&mut self, x: f32, y: f32, on: bool) {
        let (x, y) = ((x * self.sx).round(), (y * self.sy).round());
        if !x.is_finite()
            || !y.is_finite()
            || x < f32::from(i16::MIN)
            || x > f32::from(i16::MAX)
            || y < f32::from(i16::MIN)
            || y > f32::from(i16::MAX)
        {
            self.invalid = true;
            return;
        }
        let (x, y) = (x as i16, y as i16);
        let b = Bbox {
            x_min: x,
            y_min: y,
            x_max: x,
            y_max: y,
        };
        self.bbox = Some(self.bbox.map_or(b, |old| old.union(b)));
        self.current
            .push(read_fonts::tables::glyf::CurvePoint::new(x, y, on));
    }
    fn finish(mut self) -> Result<(Glyph, usize, usize)> {
        self.close();
        ensure!(
            !self.invalid,
            "scaled donor outline exceeds TrueType coordinate limits"
        );
        let contours = self.contours.len();
        let glyph = if contours == 0 {
            Glyph::Empty
        } else {
            Glyph::Simple(SimpleGlyph {
                bbox: self.bbox.unwrap_or_default(),
                contours: self.contours,
                instructions: vec![],
                overlaps: true,
            })
        };
        Ok((glyph, self.points, contours))
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.point(x, y, true);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.point(x, y, true);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.point(cx, cy, false);
        self.point(x, y, true);
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        self.invalid = true;
    }
    fn close(&mut self) {
        if self.current.len() > 1 && self.current.first() == self.current.last() {
            self.current.pop();
        }
        if !self.current.is_empty() {
            self.points += self.current.len();
            self.contours.push(std::mem::take(&mut self.current).into());
        }
    }
}
