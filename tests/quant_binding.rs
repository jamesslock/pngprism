//! Binding-requirement and pipeline tests, ported from the oracle suite
//! `lab/reference/test_prism_quant.py` (T-0068): (1) occupancy-weighted
//! centroids with pixel-exact output at or below the distinct-color cap,
//! (2) alpha extremes exact (0 stays 0, 255 stays 255), (3) channel
//! extremes 0 and 255 reachable as palette values — plus the hidden-RGB
//! policy hooks, degenerate-cap one-directional degradation, the
//! convergence bound, and the declared distance properties.

use pngprism::png::{self, DecodedImage, Properties};
use pngprism::{
    DEFAULT_HIDDEN_RGB_POLICY, HIDDEN_RGB_POLICIES, Rgba, premultiplied_distance_sq, quantize_image,
};
use std::path::Path;

fn mk_source(width: u32, height: u32, pixels: Vec<Rgba>) -> DecodedImage {
    assert_eq!(width as usize * height as usize, pixels.len());
    DecodedImage {
        width,
        height,
        pixels,
        properties: Properties {
            color_type: 6,
            bit_depth: 8,
            interlaced: false,
            plte: None,
            trns: None,
            gama: None,
            iccp: None,
            conversions: vec![],
        },
    }
}

#[path = "common/smoke.rs"]
mod smoke;

fn decode_source(path: &Path) -> DecodedImage {
    let raw = std::fs::read(path)
        .unwrap_or_else(|err| panic!("read smoke image {}: {err}", path.display()));
    png::decode_png(&raw).expect("decode smoke image")
}

/// The image behind one manifest id, or `None` when it is lab-only and we are
/// outside the research tree.
fn source_by_id(id: &str) -> Option<DecodedImage> {
    smoke::resolve_id(id).as_deref().map(decode_source)
}

fn decoded_pixels(png_bytes: &[u8]) -> Vec<Rgba> {
    png::decode_png(png_bytes)
        .expect("decode emitted png")
        .pixels
}

#[test]
fn stage_notes_and_palette_contract() {
    let source = decode_source(&smoke::available()[0].1);
    let (output, palette, notes) = quantize_image(&source, 256, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
    assert_eq!(notes.sampled_pixels, source.pixels.len());
    assert!(palette.len() <= 256);
    let check = png::decode_png(&output).unwrap();
    assert_eq!((check.width, check.height), (source.width, source.height));
    assert_eq!(check.properties.color_type, 3);
}

#[test]
fn alpha_preserved_via_trns() {
    let source = source_by_id("syn-aa-circle-subpixel").expect("vendored smoke image");
    let (output, _palette, notes) =
        quantize_image(&source, 256, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
    assert_eq!(
        notes.alpha_note,
        "alpha preserved via tRNS (extremes exact; interior quantized)"
    );
    let check = png::decode_png(&output).unwrap();
    assert!(check.properties.trns.is_some());
    assert!(check.pixels.iter().any(|p| p.3 < 255));
}

#[test]
fn opaque_source_stays_opaque() {
    // Kodak in-tree (unchanged); outside the tree it has no redistribution
    // grant, so fall back to a vendored CC0 image that is also fully opaque.
    // The assertion is about opaque sources, not about Kodak specifically —
    // substituting keeps it exercised rather than skipping the property.
    let source = source_by_id("kodak-kodim01")
        .or_else(|| source_by_id("kenney-retro-texture-opaque"))
        .expect("an opaque smoke source");
    let (output, palette, notes) = quantize_image(&source, 256, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
    assert_eq!(notes.alpha_note, "source fully opaque; no tRNS emitted");
    let check = png::decode_png(&output).unwrap();
    assert!(!check.pixels.iter().any(|p| p.3 < 255));
    assert!(palette.iter().all(|p| p.3 == 255));
}

#[test]
fn determinism() {
    let source = decode_source(&smoke::available()[1].1);
    let first = quantize_image(&source, 64, DEFAULT_HIDDEN_RGB_POLICY)
        .unwrap()
        .0;
    let second = quantize_image(&source, 64, DEFAULT_HIDDEN_RGB_POLICY)
        .unwrap()
        .0;
    assert_eq!(first, second);
}

#[test]
fn full_smoke_set_produces_valid_indexed_pngs() {
    let available = smoke::available();
    for (_row, path) in &available {
        let source = decode_source(path);
        let (output, palette, _notes) =
            quantize_image(&source, 256, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
        let check = png::decode_png(&output).unwrap();
        assert_eq!(check.properties.color_type, 3);
        assert!(palette.len() <= 256);
        assert_eq!((check.width, check.height), (source.width, source.height));
    }
    smoke::report_coverage(
        "full_smoke_set_produces_valid_indexed_pngs",
        available.len(),
    );
}

/// BINDING 2 on real corpus content: at --colors 256 AND 16, every a==0
/// source pixel keeps a==0 and every a==255 pixel keeps a==255.
#[test]
fn full_smoke_set_alpha_extremes_exact() {
    let available = smoke::available();
    for (row, path) in &available {
        let id = row.id.as_str();
        let source = decode_source(path);
        for colors in [256, 16] {
            let (output, _palette, _notes) =
                quantize_image(&source, colors, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
            let decoded = decoded_pixels(&output);
            assert_eq!(decoded.len(), source.pixels.len(), "{id} colors={colors}");
            for (original, quantized) in source.pixels.iter().zip(decoded.iter()) {
                if original.3 == 0 {
                    assert_eq!(
                        quantized.3, 0,
                        "fully-transparent pixel gained visibility: {id} colors={colors} {original:?} -> {quantized:?}"
                    );
                }
                if original.3 == 255 {
                    assert_eq!(
                        quantized.3, 255,
                        "opaque pixel lost opacity: {id} colors={colors} {original:?} -> {quantized:?}"
                    );
                }
            }
        }
    }
    smoke::report_coverage("full_smoke_set_alpha_extremes_exact", available.len());
}

/// BINDING 1 on real corpus content: any smoke item with <= 256 distinct
/// colors comes out pixel-exact at --colors 256.
#[test]
fn smoke_palette_sources_pixel_exact() {
    let available = smoke::available();
    let mut exact_count = 0;
    for (row, path) in &available {
        let id = row.id.as_str();
        let source = decode_source(path);
        let distinct: std::collections::HashSet<Rgba> = source.pixels.iter().copied().collect();
        if distinct.len() > 256 {
            continue;
        }
        exact_count += 1;
        let (output, _palette, notes) =
            quantize_image(&source, 256, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
        assert!(notes.exact_path, "{id}");
        assert_eq!(decoded_pixels(&output), source.pixels, "{id}");
    }
    smoke::report_coverage("smoke_palette_sources_pixel_exact", available.len());
    assert!(
        exact_count >= 5,
        "the palette-class smoke items: only {exact_count} of {} available images \
         came out exact",
        available.len()
    );
}

/// The coordination note's measured case (T-0068): the flag colors must
/// reproduce byte-exactly (a zero-diff case).
#[test]
fn pixel_exact_gb_flag_colors() {
    let mut pixels = vec![(236, 32, 55, 255); 10];
    pixels.extend(vec![(238, 238, 247, 255); 5]);
    pixels.extend(vec![(52, 57, 203, 255); 7]);
    pixels.push((0, 0, 0, 255));
    pixels.push((255, 255, 255, 255));
    pixels.push((9, 9, 9, 255));
    let source = mk_source(5, 5, pixels.clone());
    for colors in [6, 256] {
        let (output, palette, notes) =
            quantize_image(&source, colors, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
        assert_eq!(decoded_pixels(&output), pixels, "colors={colors}");
        assert!(notes.exact_path, "colors={colors}");
        assert_eq!(palette.len(), 6, "colors={colors}");
    }
}

/// The wrench regression: 3000 fully-transparent pixels (with hidden RGB)
/// + a full alpha ramp + opaque mass, quantized to 16 colors.
#[test]
fn alpha_extremes_exact_with_heavy_transparent_mass() {
    let mut pixels: Vec<Rgba> = (0..=255).map(|a| (200, 30, 40, a)).collect();
    pixels.extend(vec![(123, 45, 67, 0); 3000]);
    pixels.extend(vec![(10, 200, 30, 255); 2000]);
    let source = mk_source(1314, 4, pixels.clone());
    let (output, palette, _notes) = quantize_image(&source, 16, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
    let decoded = decoded_pixels(&output);
    for (original, quantized) in pixels.iter().zip(decoded.iter()) {
        if original.3 == 0 {
            assert_eq!(quantized.3, 0);
        }
        if original.3 == 255 {
            assert_eq!(quantized.3, 255);
        }
    }
    assert!(palette.iter().any(|entry| entry.3 == 0));
    assert_eq!(
        decoded.iter().filter(|p| p.3 == 0).count(),
        pixels.iter().filter(|p| p.3 == 0).count(),
    );
}

/// BINDING 3: 0 and 255 are reachable palette values (uniform-extreme
/// clusters have the extreme as their occupancy-weighted mean).
#[test]
fn channel_extremes_reachable() {
    let mut pixels = vec![(0, 0, 0, 255); 1000];
    pixels.extend(vec![(255, 255, 255, 255); 1000]);
    pixels.extend(vec![(17, 19, 21, 255); 3]);
    let source = mk_source(2003, 1, pixels);
    let (_output, palette, _notes) = quantize_image(&source, 3, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
    assert!(palette.contains(&(0, 0, 0, 255)));
    assert!(palette.contains(&(255, 255, 255, 255)));
}

/// BINDING 1 mechanism: a forced merge yields the count-weighted mean of
/// actual members, never a uniform-grid center like 16/48/.../240.
#[test]
fn occupancy_weighted_centroids_never_grid_centers() {
    let mut pixels = vec![(10, 10, 10, 255); 1000];
    pixels.extend(vec![(30, 30, 30, 255); 1000]);
    pixels.push((20, 20, 20, 255));
    let source = mk_source(2001, 1, pixels);
    let (_output, palette, _notes) = quantize_image(&source, 2, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
    let set: std::collections::HashSet<Rgba> = palette.iter().copied().collect();
    assert_eq!(
        set,
        [(10, 10, 10, 255), (30, 30, 30, 255)].into_iter().collect()
    );
    let grid_centers = [16, 48, 80, 112, 144, 176, 208, 240];
    for entry in &palette {
        assert!(
            !(grid_centers.contains(&entry.0)
                && grid_centers.contains(&entry.1)
                && grid_centers.contains(&entry.2)),
            "grid-center entry survived: {entry:?}"
        );
    }
}

/// Policy hooks (clustered path): canonicalize-black stores (0,0,0,0);
/// preserve-mean stores the count-weighted hidden mean; alpha stays
/// exactly 0.
#[test]
fn hidden_rgb_policies() {
    let mut pixels: Vec<Rgba> = (0..40)
        .flat_map(|i| vec![(100 + i, 0, 200 - i, 0); 10])
        .collect();
    pixels.extend((0..10).flat_map(|i| vec![(i, i, i, 255); 5]));
    let source = mk_source(45, 10, pixels);
    let (_o, palette, notes) = quantize_image(&source, 8, "canonicalize-black").unwrap();
    assert!(!notes.exact_path);
    let transparent: Vec<Rgba> = palette.iter().copied().filter(|e| e.3 == 0).collect();
    assert_eq!(transparent, vec![(0, 0, 0, 0)]);
    let (_o, palette, _notes) = quantize_image(&source, 8, "preserve-mean").unwrap();
    let transparent: Vec<Rgba> = palette.iter().copied().filter(|e| e.3 == 0).collect();
    // mean r = mean(100..139) = 119.5 -> 120; mean b = mean(200..161) = 180.5 -> 181
    assert_eq!(transparent, vec![(120, 0, 181, 0)]);
}

#[test]
fn hidden_rgb_default_is_declared() {
    assert_eq!(DEFAULT_HIDDEN_RGB_POLICY, "canonicalize-black");
    assert!(HIDDEN_RGB_POLICIES.contains(&DEFAULT_HIDDEN_RGB_POLICY));
}

/// cap below the zone count: degradation may lose visibility but a
/// fully-transparent pixel NEVER gains it (one-directional rule).
#[test]
fn degenerate_caps_never_reveal_transparency() {
    let pixels = vec![
        (1, 2, 3, 0),
        (4, 5, 6, 255),
        (7, 8, 9, 128),
        (10, 11, 12, 13),
    ];
    let source = mk_source(2, 2, pixels);
    for colors in [1, 2, 3] {
        let (output, palette, _notes) =
            quantize_image(&source, colors, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
        assert!(palette.len() <= colors as usize);
        let decoded = decoded_pixels(&output);
        assert_eq!(decoded[0].3, 0, "colors={colors}");
    }
}

#[test]
fn convergence_bound() {
    let pixels: Vec<Rgba> = (0..8192)
        .map(|i| {
            (
                (i & 255) as u8,
                ((i >> 8) & 255) as u8,
                ((i * 31) & 255) as u8,
                (128 + (i & 127)) as u8,
            )
        })
        .collect();
    let source = mk_source(64, 128, pixels);
    let (_o, _p, notes) = quantize_image(&source, 32, DEFAULT_HIDDEN_RGB_POLICY).unwrap();
    assert!(notes.refinement_iterations <= 8);
}

/// The declared distance: hidden RGB collapses at alpha zero; alpha is a
/// first-class channel with exact 255-scale weight.
#[test]
fn distance_properties() {
    let d = premultiplied_distance_sq;
    assert_eq!(d((1, 2, 3, 0), (250, 251, 252, 0)), 0); // collapse
    assert_eq!(d((10, 20, 30, 40), (10, 20, 30, 40)), 0);
    assert_eq!(
        d((10, 20, 30, 40), (50, 60, 70, 80)),
        d((50, 60, 70, 80), (10, 20, 30, 40))
    );
    assert_eq!(d((0, 0, 0, 0), (0, 0, 0, 1)), 255 * 255);
    assert_eq!(
        d((10, 20, 30, 255), (40, 50, 60, 255)),
        255 * 255 * (30 * 30 + 30 * 30 + 30 * 30)
    );
}
