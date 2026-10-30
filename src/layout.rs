use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail, ensure};
use read_fonts::types::{GlyphId16, Tag};
use write_fonts::tables::{
    gsub::{
        ExtensionSubtable, Gsub, LigatureSubst, MultipleSubst, SingleSubst, SingleSubstFormat2,
        SubstitutionLookup, SubstitutionLookupList,
    },
    layout::{
        ChainedSequenceContext, ClassDef, CoverageTable, Feature, FeatureRecord, LangSys, Lookup,
        LookupFlag, Script, ScriptRecord, SequenceContext, SequenceLookupRecord,
    },
};

const CALT: Tag = Tag::new(b"calt");

pub struct DonorLayout {
    pub lookups: Vec<SubstitutionLookup>,
    pub roots: Vec<u16>,
    pub glyphs: BTreeSet<GlyphId16>,
}

impl DonorLayout {
    pub fn extract(gsub: &Gsub) -> Result<Self> {
        let features = gsub
            .feature_list
            .as_ref()
            .context("donor has no features")?;
        let all = &gsub
            .lookup_list
            .as_ref()
            .context("donor has no lookups")?
            .lookups;
        let roots: Vec<_> = features
            .feature_records
            .iter()
            .filter(|f| f.feature_tag == CALT)
            .flat_map(|f| f.feature.lookup_list_indices.iter().copied())
            .collect();
        ensure!(!roots.is_empty(), "donor has no contextual ligatures");
        let mut needed = BTreeSet::new();
        let mut pending = roots.clone();
        while let Some(index) = pending.pop() {
            if !needed.insert(index) {
                continue;
            }
            let mut lookup = all
                .get(usize::from(index))
                .context("invalid donor lookup reference")?
                .as_ref()
                .clone();
            visit_records(&mut lookup, &mut |r| {
                pending.push(r.lookup_list_index);
                Ok(())
            })?;
        }
        let indices: BTreeMap<_, _> = needed
            .iter()
            .enumerate()
            .map(|(new, old)| Ok((*old, u16::try_from(new + 1)?)))
            .collect::<Result<_>>()?;
        let mut glyphs = BTreeSet::new();
        let mut lookups = Vec::new();
        for old in needed {
            let mut lookup = all[usize::from(old)].as_ref().clone();
            visit_records(&mut lookup, &mut |r| {
                r.lookup_list_index = *indices
                    .get(&r.lookup_list_index)
                    .context("missing donor lookup")?;
                Ok(())
            })?;
            visit_donor_glyphs(&mut lookup, &mut |g| {
                glyphs.insert(g);
                Ok(g)
            })
            .with_context(|| format!("donor lookup {old}"))?;
            lookups.push(lookup);
        }
        let roots = roots.into_iter().map(|i| indices[&i]).collect();
        Ok(Self {
            lookups,
            roots,
            glyphs,
        })
    }

    pub fn remap(&mut self, glyph_map: &BTreeMap<GlyphId16, GlyphId16>) -> Result<()> {
        for lookup in &mut self.lookups {
            visit_donor_glyphs(lookup, &mut |g| {
                glyph_map
                    .get(&g)
                    .copied()
                    .context("missing donor glyph mapping")
            })?;
        }
        Ok(())
    }
}

pub fn single_lookup(
    pairs: impl IntoIterator<Item = (GlyphId16, GlyphId16)>,
) -> SubstitutionLookup {
    let pairs: BTreeMap<_, _> = pairs.into_iter().collect();
    let coverage = pairs.keys().copied().collect();
    let replacements = pairs.into_values().collect();
    SubstitutionLookup::Single(Lookup::new(
        LookupFlag::empty(),
        vec![SingleSubst::Format2(SingleSubstFormat2::new(
            coverage,
            replacements,
        ))],
    ))
}

pub fn merge(
    mut target: Gsub,
    donor: DonorLayout,
    enter: SubstitutionLookup,
    exit: SubstitutionLookup,
) -> Result<Gsub> {
    ensure!(
        target.feature_variations.is_none()
            && target.lookup_list2.is_none()
            && target.feature_list2.is_none()
            && target.script_list2.is_none(),
        "GSUB variations and extended layout headers are not supported yet"
    );
    let added = u16::try_from(donor.lookups.len() + 2)?;
    let mut original = target
        .lookup_list
        .as_mut()
        .context("missing GSUB lookup list")?
        .lookups
        .clone();
    ensure!(
        original.len() + usize::from(added) <= usize::from(u16::MAX),
        "too many substitution lookups"
    );
    for lookup in &mut original {
        visit_records(lookup, &mut |r| {
            ensure!(
                usize::from(r.lookup_list_index)
                    < target.lookup_list.as_ref().map_or(0, |l| l.lookups.len()),
                "invalid input lookup reference"
            );
            r.lookup_list_index = r
                .lookup_list_index
                .checked_add(added)
                .context("lookup index overflow")?;
            Ok(())
        })?;
    }
    let mut patch_indices = vec![0];
    patch_indices.extend(donor.roots);
    patch_indices.push(added - 1);
    let features = target
        .feature_list
        .as_mut()
        .context("missing GSUB feature list")?;
    for record in &mut features.feature_records {
        for index in &mut record.feature.lookup_list_indices {
            ensure!(
                usize::from(*index) < original.len(),
                "invalid input feature lookup reference"
            );
            *index = index.checked_add(added).context("lookup index overflow")?;
        }
        if record.feature_tag == CALT {
            record
                .feature
                .lookup_list_indices
                .splice(0..0, patch_indices.iter().copied());
        }
    }
    let scripts = target
        .script_list
        .as_mut()
        .context("missing GSUB script list")?;
    let old_feature_count = features.feature_records.len();
    ensure!(
        old_feature_count < usize::from(u16::MAX),
        "too many substitution features"
    );
    let at = features
        .feature_records
        .partition_point(|f| f.feature_tag <= CALT);
    // Languages without calt get only the patch, never another script's original rules.
    features.feature_records.insert(
        at,
        FeatureRecord::new(CALT, Feature::new(None, patch_indices)),
    );
    let calt_index = u16::try_from(at)?;
    for record in &mut scripts.script_records {
        for lang in languages(&mut record.script) {
            for index in &mut lang.feature_indices {
                ensure!(
                    usize::from(*index) < old_feature_count,
                    "invalid input language feature reference"
                );
                if *index >= calt_index {
                    *index = index.checked_add(1).context("feature index overflow")?;
                }
            }
            if lang.required_feature_index != u16::MAX {
                ensure!(
                    usize::from(lang.required_feature_index) < old_feature_count,
                    "invalid required feature reference"
                );
                if lang.required_feature_index >= calt_index {
                    lang.required_feature_index = lang
                        .required_feature_index
                        .checked_add(1)
                        .context("feature index overflow")?;
                }
            }
        }
    }
    if !scripts
        .script_records
        .iter()
        .any(|s| s.script_tag == Tag::new(b"DFLT"))
    {
        let script = Script {
            default_lang_sys: Some(LangSys::new(vec![calt_index])).into(),
            ..Default::default()
        };
        scripts
            .script_records
            .push(ScriptRecord::new(Tag::new(b"DFLT"), script));
        scripts.script_records.sort_by_key(|s| s.script_tag);
    }
    for record in &mut scripts.script_records {
        if record.script.default_lang_sys.is_none() {
            record.script.default_lang_sys = Some(LangSys::new(vec![])).into();
        }
        for lang in languages(&mut record.script) {
            let has_calt = lang
                .feature_indices
                .iter()
                .copied()
                .chain(
                    (lang.required_feature_index != u16::MAX)
                        .then_some(lang.required_feature_index),
                )
                .any(|i| {
                    features
                        .feature_records
                        .get(usize::from(i))
                        .is_some_and(|f| f.feature_tag == CALT)
                });
            if !has_calt {
                lang.feature_indices.push(calt_index);
                lang.feature_indices.sort_unstable();
            }
        }
    }
    let lookups = std::iter::once(enter)
        .chain(donor.lookups)
        .chain(std::iter::once(exit))
        .map(Into::into)
        .chain(original)
        .collect();
    target.lookup_list = Some(SubstitutionLookupList { lookups }).into();
    Ok(target)
}

fn languages(script: &mut Script) -> impl Iterator<Item = &mut LangSys> {
    script.default_lang_sys.as_mut().into_iter().chain(
        script
            .lang_sys_records
            .iter_mut()
            .map(|r| r.lang_sys.as_mut()),
    )
}

fn coverage(
    table: &mut CoverageTable,
    f: &mut impl FnMut(GlyphId16) -> Result<GlyphId16>,
) -> Result<()> {
    let glyphs = table
        .iter()
        .map(|g| f(GlyphId16::try_from(g).context("24-bit glyph IDs are not supported")?))
        .collect::<Result<Vec<_>>>()?;
    // Donor glyph IDs are assigned monotonically, keeping coverage-to-rule associations intact.
    ensure!(
        glyphs.windows(2).all(|w| w[0] < w[1]),
        "donor mapping changed coverage order"
    );
    *table = glyphs.into();
    Ok(())
}

fn classes(table: &mut ClassDef, f: &mut impl FnMut(GlyphId16) -> Result<GlyphId16>) -> Result<()> {
    let pairs = table
        .iter()
        .map(|(g, c)| Ok((f(GlyphId16::try_from(g)?)?, u16::try_from(c)?)))
        .collect::<Result<Vec<_>>>()?;
    *table = pairs.into_iter().collect();
    Ok(())
}

fn visit_donor_glyphs(
    lookup: &mut SubstitutionLookup,
    f: &mut impl FnMut(GlyphId16) -> Result<GlyphId16>,
) -> Result<()> {
    match lookup {
        SubstitutionLookup::Single(l) => {
            ensure!(
                l.lookup_flag == LookupFlag::empty(),
                "unexpected donor lookup flags"
            );
            for sub in &mut l.subtables {
                let (cov, replacements) = match sub.as_ref() {
                    SingleSubst::Format1(s) => (
                        s.coverage.as_ref().clone(),
                        s.coverage
                            .iter()
                            .map(|g| {
                                GlyphId16::new(
                                    (g.to_u32() as u16).wrapping_add(s.delta_glyph_id as u16),
                                )
                            })
                            .collect(),
                    ),
                    SingleSubst::Format2(s) => {
                        (s.coverage.as_ref().clone(), s.substitute_glyph_ids.clone())
                    }
                    _ => bail!("unsupported donor single substitution format"),
                };
                let mut cov = cov;
                coverage(&mut cov, f)?;
                let replacements = replacements
                    .into_iter()
                    .map(&mut *f)
                    .collect::<Result<_>>()?;
                **sub = SingleSubst::Format2(SingleSubstFormat2::new(cov, replacements));
            }
        }
        SubstitutionLookup::Multiple(l) => {
            ensure!(
                l.lookup_flag == LookupFlag::empty(),
                "unexpected donor lookup flags"
            );
            for sub in &mut l.subtables {
                let MultipleSubst::Format1(s) = sub.as_mut() else {
                    bail!("unsupported donor multiple substitution format");
                };
                coverage(&mut s.coverage, f)?;
                for sequence in &mut s.sequences {
                    for g in &mut sequence.substitute_glyph_ids {
                        *g = f(*g)?;
                    }
                }
            }
        }
        SubstitutionLookup::Ligature(l) => {
            ensure!(
                l.lookup_flag == LookupFlag::empty(),
                "unexpected donor lookup flags"
            );
            for sub in &mut l.subtables {
                let LigatureSubst::Format1(s) = sub.as_mut() else {
                    bail!("unsupported donor ligature substitution format");
                };
                coverage(&mut s.coverage, f)?;
                for set in &mut s.ligature_sets {
                    for ligature in &mut set.ligatures {
                        ligature.ligature_glyph = f(ligature.ligature_glyph)?;
                        for g in &mut ligature.component_glyph_ids {
                            *g = f(*g)?;
                        }
                    }
                }
            }
        }
        SubstitutionLookup::ChainContextual(l) => {
            ensure!(
                l.lookup_flag == LookupFlag::empty(),
                "unexpected donor lookup flags"
            );
            for sub in &mut l.subtables {
                match &mut ***sub {
                    ChainedSequenceContext::Format1(s) => {
                        coverage(&mut s.coverage, f)?;
                        for set in &mut s.chained_seq_rule_sets {
                            if let Some(set) = set.as_mut() {
                                for rule in &mut set.chained_seq_rules {
                                    let rule = rule.as_mut();
                                    for g in rule
                                        .backtrack_sequence
                                        .iter_mut()
                                        .chain(&mut rule.input_sequence)
                                        .chain(&mut rule.lookahead_sequence)
                                    {
                                        *g = f(*g)?;
                                    }
                                }
                            }
                        }
                    }
                    ChainedSequenceContext::Format2(s) => {
                        coverage(&mut s.coverage, f)?;
                        classes(&mut s.backtrack_class_def, f)?;
                        classes(&mut s.input_class_def, f)?;
                        classes(&mut s.lookahead_class_def, f)?;
                    }
                    ChainedSequenceContext::Format3(s) => {
                        for cov in s
                            .backtrack_coverages
                            .iter_mut()
                            .chain(&mut s.input_coverages)
                            .chain(&mut s.lookahead_coverages)
                        {
                            coverage(cov, f)?;
                        }
                    }
                    _ => bail!("unsupported donor context format"),
                }
            }
        }
        _ => bail!("unexpected lookup type in the pinned donor's calt graph"),
    }
    Ok(())
}

fn context_records(
    context: &mut SequenceContext,
    f: &mut impl FnMut(&mut SequenceLookupRecord) -> Result<()>,
) -> Result<()> {
    match context {
        SequenceContext::Format1(s) => {
            for set in &mut s.seq_rule_sets {
                if let Some(set) = set.as_mut() {
                    for rule in &mut set.seq_rules {
                        for r in &mut rule.seq_lookup_records {
                            f(r)?;
                        }
                    }
                }
            }
        }
        SequenceContext::Format2(s) => {
            for set in &mut s.class_seq_rule_sets {
                if let Some(set) = set.as_mut() {
                    for rule in &mut set.class_seq_rules {
                        for r in &mut rule.seq_lookup_records {
                            f(r)?;
                        }
                    }
                }
            }
        }
        SequenceContext::Format3(s) => {
            for r in &mut s.seq_lookup_records {
                f(r)?;
            }
        }
        _ => bail!("extended contextual substitutions are not supported yet"),
    }
    Ok(())
}

fn chain_records(
    context: &mut ChainedSequenceContext,
    f: &mut impl FnMut(&mut SequenceLookupRecord) -> Result<()>,
) -> Result<()> {
    match context {
        ChainedSequenceContext::Format1(s) => {
            for set in &mut s.chained_seq_rule_sets {
                if let Some(set) = set.as_mut() {
                    for rule in &mut set.chained_seq_rules {
                        for r in &mut rule.seq_lookup_records {
                            f(r)?;
                        }
                    }
                }
            }
        }
        ChainedSequenceContext::Format2(s) => {
            for set in &mut s.chained_class_seq_rule_sets {
                if let Some(set) = set.as_mut() {
                    for rule in &mut set.chained_class_seq_rules {
                        for r in &mut rule.seq_lookup_records {
                            f(r)?;
                        }
                    }
                }
            }
        }
        ChainedSequenceContext::Format3(s) => {
            for r in &mut s.seq_lookup_records {
                f(r)?;
            }
        }
        _ => bail!("extended chained substitutions are not supported yet"),
    }
    Ok(())
}

fn visit_records(
    lookup: &mut SubstitutionLookup,
    f: &mut impl FnMut(&mut SequenceLookupRecord) -> Result<()>,
) -> Result<()> {
    match lookup {
        SubstitutionLookup::Contextual(l) => {
            for sub in &mut l.subtables {
                context_records(sub, f)?;
            }
        }
        SubstitutionLookup::ChainContextual(l) => {
            for sub in &mut l.subtables {
                chain_records(sub, f)?;
            }
        }
        SubstitutionLookup::Extension(l) => {
            for sub in &mut l.subtables {
                match sub.as_mut() {
                    ExtensionSubtable::Contextual(s) => context_records(&mut s.extension, f)?,
                    ExtensionSubtable::ChainContextual(s) => chain_records(&mut s.extension, f)?,
                    _ => (),
                }
            }
        }
        _ => (),
    }
    Ok(())
}
