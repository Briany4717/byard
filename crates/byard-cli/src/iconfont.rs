//! Cutting an icon font down to the glyphs a program uses (RFC-0054).
//!
//! `byard bundle` calls [`subset`] for a font declared `icons = true`. The
//! names come from the program (`byard_compiler::icons::string_literals`) and
//! the ligatures among them from the font itself (`byard_core::text::ligatures`,
//! which shapes each candidate).
//!
//! The cut is the fontations subsetter's, for outlines, `cmap`, `fvar`, `gvar`,
//! `avar` and `HVAR`: it keeps the glyphs of the names and of their letters, and
//! the variation data of exactly those. Its `GSUB` is not used. Closing a
//! `GSUB` over a font of three thousand ligatures keeps every ligature whose
//! letters survive (hundreds of icons nobody asked for) and, for the
//! extension-wrapped lookups Material Symbols uses, writes a table that does
//! not parse. The glyphs are known by name, so the table is written fresh: one
//! `liga` lookup with one rule per kept icon.

use std::collections::BTreeSet;
use std::sync::Arc;

use fontcull_klippa::{Plan, SubsetFlags, subset_font};
use fontcull_skrifa::{GlyphId, MetadataProvider};
use fontcull_write_fonts::{
    read::{FontRef, TableProvider, collections::IntSet},
    types::{NameId, Tag},
};

/// A font cut down, and what it kept.
#[derive(Debug)]
pub struct Cut {
    /// The new font file.
    pub bytes: Vec<u8>,
    /// The icon names it still draws.
    pub names: Vec<String>,
}

/// Keeps the glyphs for `names` (the ligatures of `font` among them) and
/// nothing else. Errors say why the font could not be cut, and the caller
/// ships it whole: a font that came out wrong is worse than a large one.
///
/// # Errors
/// The file is not a font the subsetter reads, the subsetter fails, the glyphs
/// it kept are not the ones expected, or the result no longer draws one of the
/// names (checked by shaping each again).
pub fn subset(font: &[u8], names: &[String]) -> Result<Cut, String> {
    let bytes: Arc<[u8]> = Arc::from(font);
    let found = byard_core::text::ligatures(&bytes, names);
    let face = FontRef::new(font).map_err(|e| format!("not a font the subsetter reads: {e}"))?;

    // The glyphs to keep, as the font numbers them: .notdef, the letters of
    // the names, and the ligature glyphs.
    let charmap = face.charmap();
    let letters: BTreeSet<char> = found.iter().flat_map(|(n, _)| n.chars()).collect();
    let mut old: BTreeSet<u32> = BTreeSet::from([0]);
    for c in &letters {
        let gid = charmap
            .map(*c)
            .ok_or_else(|| format!("the font has no glyph for `{c}`"))?;
        old.insert(gid.to_u32());
    }
    old.extend(found.iter().map(|(_, g)| u32::from(*g)));
    let new_id = |gid: u32| {
        old.iter()
            .position(|g| *g == gid)
            .and_then(|i| u16::try_from(i).ok())
    };

    let mut gids = IntSet::<GlyphId>::empty();
    for g in &old {
        gids.insert(GlyphId::new(*g));
    }
    let mut unicodes = IntSet::<u32>::empty();
    for c in &letters {
        unicodes.insert(u32::from(*c));
    }
    // The names a loader needs to register the face (family, style, full and
    // PostScript names) and the axes' labels, which `fvar` points into.
    let mut name_ids = IntSet::<NameId>::empty();
    for id in (0..=6).chain([16, 17]).chain(256..300) {
        name_ids.insert(NameId::new(id));
    }
    let mut drop = IntSet::<Tag>::empty();
    drop.insert(Tag::new(b"GSUB"));
    let plan = Plan::new(
        &gids,
        &unicodes,
        &face,
        SubsetFlags::default(),
        &drop,
        &IntSet::<Tag>::all(),
        &IntSet::<Tag>::empty(),
        &name_ids,
        &IntSet::<u16>::all(),
    );
    let cut = subset_font(&face, &plan).map_err(|e| format!("the subsetter failed: {e:?}"))?;
    let kept = FontRef::new(&cut)
        .map_err(|e| format!("the cut font does not parse: {e}"))?
        .maxp()
        .map_err(|e| format!("the cut font has no maxp: {e}"))?
        .num_glyphs();
    if usize::from(kept) != old.len() {
        return Err(format!(
            "the subsetter kept {kept} glyphs where {} were asked for",
            old.len()
        ));
    }

    // One rule per icon, keyed by its first letter.
    let mut rules: Vec<(u16, Vec<u16>, u16)> = Vec::new();
    for (name, glyph) in &found {
        let mut parts = Vec::new();
        for c in name.chars() {
            let gid = charmap.map(c).map(fontcull_skrifa::GlyphId::to_u32);
            parts.push(gid.and_then(new_id).ok_or("a letter lost its glyph")?);
        }
        let out = new_id(u32::from(*glyph)).ok_or("a ligature lost its glyph")?;
        rules.push((parts[0], parts[1..].to_vec(), out));
    }
    let gsub = ligature_table(&mut rules);
    let bytes = with_table(&cut, *b"GSUB", &gsub)?;

    let still = byard_core::text::ligatures(
        &Arc::from(bytes.as_slice()),
        &found.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
    );
    if still.len() != found.len() {
        let lost: Vec<&String> = found
            .iter()
            .map(|(n, _)| n)
            .filter(|n| !still.iter().any(|(s, _)| s == *n))
            .collect();
        return Err(format!("the cut font no longer draws {lost:?}"));
    }
    Ok(Cut {
        bytes,
        names: found.into_iter().map(|(n, _)| n).collect(),
    })
}

/// A `GSUB` table with one `liga` lookup: `rules` are `(first, rest, ligature)`
/// glyph ids. Longer rules go first within a first glyph, as shaping needs.
fn ligature_table(rules: &mut [(u16, Vec<u16>, u16)]) -> Vec<u8> {
    fn u16s(out: &mut Vec<u8>, values: &[u16]) {
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
    rules.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.len().cmp(&a.1.len())));
    let firsts: Vec<u16> = {
        let mut f: Vec<u16> = rules.iter().map(|r| r.0).collect();
        f.dedup();
        f
    };
    // The substitution subtable: coverage, then one ligature set per first.
    let mut sub = Vec::new();
    let set_count = firsts.len();
    let coverage_at = 6 + 2 * set_count;
    u16s(&mut sub, &[1, u16::try_from(coverage_at).unwrap_or(0)]);
    u16s(&mut sub, &[u16::try_from(set_count).unwrap_or(0)]);
    let mut coverage = vec![0u8; 0];
    u16s(&mut coverage, &[1, u16::try_from(set_count).unwrap_or(0)]);
    u16s(&mut coverage, &firsts);
    let mut sets: Vec<Vec<u8>> = Vec::new();
    for first in &firsts {
        let mine: Vec<&(u16, Vec<u16>, u16)> = rules.iter().filter(|r| r.0 == *first).collect();
        let mut set = Vec::new();
        u16s(&mut set, &[u16::try_from(mine.len()).unwrap_or(0)]);
        let mut ligs: Vec<u8> = Vec::new();
        let mut at = 2 + 2 * mine.len();
        for r in &mine {
            u16s(&mut set, &[u16::try_from(at).unwrap_or(0)]);
            let mut lig = Vec::new();
            u16s(&mut lig, &[r.2, u16::try_from(r.1.len() + 1).unwrap_or(0)]);
            u16s(&mut lig, &r.1);
            at += lig.len();
            ligs.extend(lig);
        }
        set.extend(ligs);
        sets.push(set);
    }
    let mut offset = coverage_at + coverage.len();
    for set in &sets {
        u16s(&mut sub, &[u16::try_from(offset).unwrap_or(0)]);
        offset += set.len();
    }
    sub.extend(coverage);
    for set in sets {
        sub.extend(set);
    }

    // Script, feature and lookup lists around it.
    let mut t = Vec::new();
    // Header: version 1.0, then the three list offsets (10, 36, 50).
    u16s(&mut t, &[1, 0, 10, 36, 50]);
    // ScriptList at 10: DFLT and latn share one Script table.
    u16s(&mut t, &[2]);
    t.extend_from_slice(b"DFLT");
    u16s(&mut t, &[14]);
    t.extend_from_slice(b"latn");
    u16s(&mut t, &[14]);
    // That makes the ScriptList 14 bytes so far; the Script table follows at 10 + 14.
    // Script at 14 inside the list: default LangSys offset 4, no others.
    u16s(&mut t, &[4, 0]);
    // LangSys: no lookup order, no required feature, one feature (0).
    u16s(&mut t, &[0, 0xFFFF, 1, 0]);
    // FeatureList at 36: one record 'liga' -> Feature at 8.
    debug_assert_eq!(t.len(), 36);
    u16s(&mut t, &[1]);
    t.extend_from_slice(b"liga");
    u16s(&mut t, &[8]);
    // Feature: no params, one lookup (0).
    u16s(&mut t, &[0, 1, 0]);
    // LookupList at 50: one lookup at offset 4.
    debug_assert_eq!(t.len(), 50);
    u16s(&mut t, &[1, 4]);
    // Lookup: type 4, no flags, one subtable at offset 8.
    u16s(&mut t, &[4, 0, 1, 8]);
    t.extend(sub);
    t
}

/// The sfnt `font` with the table `tag` set to `data`, directory and
/// checksums rebuilt.
fn with_table(font: &[u8], tag: [u8; 4], data: &[u8]) -> Result<Vec<u8>, String> {
    let be16 = |at: usize| {
        font.get(at..at + 2)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
    };
    let be32 = |at: usize| {
        font.get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    };
    let bad = || "the cut font's table directory is malformed".to_string();
    let count = be16(4).ok_or_else(bad)?;
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(count + 1);
    for i in 0..count {
        let rec = 12 + 16 * i;
        let name: [u8; 4] = font
            .get(rec..rec + 4)
            .ok_or_else(bad)?
            .try_into()
            .map_err(|_| bad())?;
        let (off, len) = (
            be32(rec + 8).ok_or_else(bad)?,
            be32(rec + 12).ok_or_else(bad)?,
        );
        if name != tag {
            tables.push((name, font.get(off..off + len).ok_or_else(bad)?.to_vec()));
        }
    }
    tables.push((tag, data.to_vec()));
    tables.sort_by_key(|(n, _)| *n);

    let n = tables.len();
    let mut entry_selector = 0u16;
    while (1usize << (entry_selector + 1)) <= n {
        entry_selector += 1;
    }
    let search_range = 16u16 * (1u16 << entry_selector);
    let mut out = Vec::new();
    out.extend_from_slice(font.get(..4).ok_or_else(bad)?);
    for v in [
        u16::try_from(n).map_err(|_| bad())?,
        search_range,
        entry_selector,
        u16::try_from(n).map_err(|_| bad())? * 16 - search_range,
    ] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    let sum = |bytes: &[u8]| {
        bytes.chunks(4).fold(0u32, |acc, c| {
            let mut w = [0u8; 4];
            w[..c.len()].copy_from_slice(c);
            acc.wrapping_add(u32::from_be_bytes(w))
        })
    };
    let offset = 12 + 16 * n;
    let mut body = Vec::new();
    let mut head_at = None;
    for (name, bytes) in &mut tables {
        if name == b"head" && bytes.len() >= 12 {
            bytes[8..12].copy_from_slice(&[0; 4]);
            head_at = Some(body.len());
        }
        out.extend_from_slice(name);
        out.extend_from_slice(&sum(bytes).to_be_bytes());
        out.extend_from_slice(
            &u32::try_from(offset + body.len())
                .map_err(|_| bad())?
                .to_be_bytes(),
        );
        out.extend_from_slice(&u32::try_from(bytes.len()).map_err(|_| bad())?.to_be_bytes());
        body.extend_from_slice(bytes);
        while body.len() % 4 != 0 {
            body.push(0);
        }
    }
    out.extend(body);
    if let Some(at) = head_at {
        let adjust = 0xB1B0_AFBAu32.wrapping_sub(sum(&out));
        let pos = offset + at + 8;
        out[pos..pos + 4].copy_from_slice(&adjust.to_be_bytes());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICONS: &[u8] = include_bytes!("../examples/assets/fonts/ByardIcons.ttf");

    fn names(of: &[&str]) -> Vec<String> {
        of.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_cut_font_draws_the_names_and_is_smaller() {
        let cut = subset(ICONS, &names(&["favorite", "home", "nonsense"])).expect("cuts");
        assert_eq!(cut.names, names(&["favorite", "home"]));
        assert!(
            cut.bytes.len() < ICONS.len(),
            "{} vs {}",
            cut.bytes.len(),
            ICONS.len()
        );
        let kept: Vec<String> = byard_core::text::ligatures(
            &Arc::from(cut.bytes.as_slice()),
            &names(&[
                "favorite", "home", "star", "search", "settings", "mail", "menu", "close",
            ]),
        )
        .into_iter()
        .map(|(n, _)| n)
        .collect();
        assert!(kept.contains(&"favorite".to_string()) && kept.contains(&"home".to_string()));
        assert!(
            !kept.contains(&"settings".to_string()),
            "an unused icon came along: {kept:?}"
        );
    }

    #[test]
    fn the_axes_survive_the_cut() {
        let cut = subset(ICONS, &names(&["favorite"])).expect("cuts");
        let face = ttf_axes(&cut.bytes);
        assert_eq!(face, ["FILL", "GRAD", "opsz", "wght"]);
        assert!(cut.bytes.windows(4).any(|w| w == b"gvar"));
    }

    /// The tags of the font's `fvar` axes, read from the table directory.
    fn ttf_axes(bytes: &[u8]) -> Vec<String> {
        let n = usize::from(u16::from_be_bytes([bytes[4], bytes[5]]));
        let fvar = (0..n)
            .map(|i| &bytes[12 + 16 * i..28 + 16 * i])
            .find(|r| &r[..4] == b"fvar")
            .expect("fvar is kept");
        let off = u32::from_be_bytes([fvar[8], fvar[9], fvar[10], fvar[11]]) as usize;
        let axes_at = off + usize::from(u16::from_be_bytes([bytes[off + 4], bytes[off + 5]]));
        let count = usize::from(u16::from_be_bytes([bytes[off + 8], bytes[off + 9]]));
        let size = usize::from(u16::from_be_bytes([bytes[off + 10], bytes[off + 11]]));
        (0..count)
            .map(|i| {
                String::from_utf8_lossy(&bytes[axes_at + i * size..axes_at + i * size + 4])
                    .into_owned()
            })
            .collect()
    }
}

/// The real thing, when it is at hand: Material Symbols Rounded, 15 MB, which
/// is not in this repository. Set `BYARD_MATERIAL_SYMBOLS` to the file.
#[cfg(test)]
mod material_symbols {
    use super::*;

    #[test]
    fn four_icons_cut_from_material_symbols() {
        let Some(path) = std::env::var_os("BYARD_MATERIAL_SYMBOLS") else {
            eprintln!("BYARD_MATERIAL_SYMBOLS is not set; skipping the 15 MB font test");
            return;
        };
        let font = std::fs::read(path).expect("the font is readable");
        let names: Vec<String> = ["favorite", "home", "search", "settings", "not_an_icon"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let cut = subset(&font, &names).expect("cuts");
        assert_eq!(cut.names.len(), 4, "{:?}", cut.names);
        eprintln!("{} -> {} bytes", font.len(), cut.bytes.len());
        assert!(cut.bytes.len() < font.len() / 20);
        for table in [b"fvar", b"gvar", b"avar", b"GSUB"] {
            assert!(
                cut.bytes.windows(4).any(|w| w == table),
                "{} is kept",
                String::from_utf8_lossy(table)
            );
        }
    }
}
