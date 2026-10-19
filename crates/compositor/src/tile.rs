//! Evaluation of a layer tree for one tile.
//!
//! Every channel keeps a running accumulation of straight `(value, alpha)` texels. The root starts
//! at the channel's default value with alpha 1; isolated folders start transparent; passthrough
//! folders let their children act directly on the accumulation below and then lerp the result
//! by folder opacity × mask.

use std::sync::Arc;

use subcraft_core::ChannelKind;
use subcraft_doc::{BlendMode, ContentEffect, ContentEffectKind, Layer, LayerContent, ProceduralEvaluator, TextureSet};
use subcraft_image::TILE_TEXELS;
use subcraft_materials::eval_fill_tile;

use crate::TileContext;
use crate::blend::{BlendTarget, composite};
use crate::mask::eval_mask;
use crate::util::read_tile_rgba;

/// Running accumulation of one channel over a tile.
#[derive(Clone)]
struct Acc {
    kind: ChannelKind,
    target: BlendTarget,
    px: Vec<[f32; 4]>,
}

impl Acc {
    fn root(kind: ChannelKind) -> Self {
        let d = kind.default_value().0;
        Self { kind, target: BlendTarget::for_channel(kind), px: vec![[d[0], d[1], d[2], 1.0]; TILE_TEXELS] }
    }

    fn transparent(kind: ChannelKind) -> Self {
        Self { kind, target: BlendTarget::for_channel(kind), px: vec![[0.0; 4]; TILE_TEXELS] }
    }
}

/// Composite all layers of `ts` for one tile. Returns, per texture-set channel (in
/// `ts.channels` order), the accumulated `[v0, v1, v2, alpha]` texels.
pub fn evaluate_tile(ts: &TextureSet, tc: &TileContext) -> Vec<(ChannelKind, Vec<[f32; 4]>)> {
    let mut accs: Vec<Acc> = ts.channel_kinds().map(Acc::root).collect();
    let active = vec![true; accs.len()];
    apply_stack(Some(ts), &ts.layers, &mut accs, &active, tc);
    accs.into_iter().map(|a| (a.kind, a.px)).collect()
}

fn apply_stack(ts: Option<&TextureSet>, layers: &[Arc<Layer>], accs: &mut [Acc], active: &[bool], tc: &TileContext) {
    for layer in layers {
        apply_layer(ts, layer, accs, active, tc);
    }
}

/// Isolated authored content for a UV thumbnail, before its own mask/blend/opacity/visibility.
/// Folder children retain their masks and blends. Uses the production evaluator and channel rules.
pub fn preview_content(layer: &Layer, channel: ChannelKind, tc: &TileContext) -> Vec<[f32; 4]> {
    let mut source = layer.clone();
    source.visible = true;
    source.mask = None;
    source.parts = None;
    source.blend.insert(channel, subcraft_doc::ChannelBlend::new(BlendMode::Normal));
    let mut acc = [Acc::transparent(channel)];
    apply_layer(None, &source, &mut acc, &[true], tc);
    std::mem::take(&mut acc[0].px)
}

#[inline]
fn effective_mode(mode: BlendMode) -> BlendMode {
    if mode == BlendMode::Passthrough { BlendMode::Normal } else { mode }
}

fn apply_layer(ts: Option<&TextureSet>, layer: &Layer, accs: &mut [Acc], active: &[bool], tc: &TileContext) {
    if !layer.visible {
        return;
    }
    // Channels this layer can affect at all.
    let live: Vec<bool> = accs.iter().zip(active).map(|(a, &on)| on && layer.blend_for(a.kind).is_active()).collect();
    if !live.contains(&true) {
        return;
    }
    let mut mask = eval_mask(layer.mask.as_ref(), tc);
    // Part mask: the layer only shows on its chosen mesh parts.
    if let Some(parts) = layer.parts.as_ref().filter(|p| p.enabled) {
        if parts.parts.is_empty() {
            return; // visible nowhere
        }
        let m = mask.get_or_insert_with(|| vec![1.0; TILE_TEXELS]);
        for (i, v) in m.iter_mut().enumerate() {
            *v *= parts.factor(tc.geo.region(i, parts.kind));
        }
    }
    // Fully masked out in this tile: blending with zero coverage changes nothing, so skip the
    // (possibly expensive) content evaluation entirely. Exact, not an approximation.
    if mask.as_ref().is_some_and(|m| m.iter().all(|&v| v <= 0.0)) {
        return;
    }
    let mask_at = |i: usize| mask.as_ref().map_or(1.0, |m| m[i]);
    let mut content = vec![[0.0f32; 4]; TILE_TEXELS];

    match &layer.content {
        LayerContent::Paint(paint) => {
            for (acc, _) in accs.iter_mut().zip(&live).filter(|(_, l)| **l) {
                let graph = if acc.kind == ChannelKind::Roughness {
                    paint.procedural_graph.as_ref().and_then(|graph| graph.compile().ok().map(|eval| (graph, eval)))
                } else { None };
                let Some(img) = paint.channels.get(&acc.kind) else {
                    // No base raster on this channel: effects still apply to transparent content.
                    if graph.is_none() && layer.content_effects.is_empty() {
                        continue;
                    }
                    if let Some((graph, evaluator)) = graph {
                        eval_paint_graph(graph.occlusion_source_layer_id(), evaluator, ts, tc, &mut content);
                    } else { content.fill([0.0; 4]); }
                    apply_content_effects(&layer.content_effects, acc, &mut content, tc);
                    blend_content(acc, &content, layer, &mask_at);
                    continue;
                };
                if let Some((graph, evaluator)) = graph {
                    eval_paint_graph(graph.occlusion_source_layer_id(), evaluator, ts, tc, &mut content);
                    let mut authored = vec![[0.0f32; 4]; TILE_TEXELS];
                    read_paint(img, acc.kind, tc, &mut authored);
                    for (dst, src) in content.iter_mut().zip(authored) {
                        // Authored Roughness is source-over the generated base. Layer mask,
                        // opacity, blend and content effects apply once below this point.
                        let a = src[3].clamp(0.0, 1.0);
                        let value = dst[0] + (src[0] - dst[0]) * a;
                        *dst = [value, value, value, 1.0];
                    }
                } else { read_paint(img, acc.kind, tc, &mut content); }
                apply_content_effects(&layer.content_effects, acc, &mut content, tc);
                blend_content(acc, &content, layer, &mask_at);
            }
        }
        LayerContent::Fill(fill) => {
            for (acc, _) in accs.iter_mut().zip(&live).filter(|(_, l)| **l) {
                if !eval_fill(fill, acc.kind, tc, &mut content) {
                    if layer.content_effects.is_empty() {
                        continue;
                    }
                    content.fill([0.0; 4]);
                }
                apply_content_effects(&layer.content_effects, acc, &mut content, tc);
                blend_content(acc, &content, layer, &mask_at);
            }
        }
        LayerContent::Group(group) => {
            let mut child: Vec<Acc> = accs
                .iter()
                .zip(&live)
                .map(|(a, &on)| {
                    if on && layer.blend_for(a.kind).mode == BlendMode::Passthrough {
                        a.clone()
                    } else {
                        Acc::transparent(a.kind)
                    }
                })
                .collect();
            apply_stack(ts, &group.children, &mut child, &live, tc);
            for ((acc, inner), _) in accs.iter_mut().zip(child).zip(&live).filter(|(_, l)| **l) {
                let blend = layer.blend_for(acc.kind);
                if blend.mode == BlendMode::Passthrough {
                    for (i, (dst, src)) in acc.px.iter_mut().zip(&inner.px).enumerate() {
                        let f = (blend.opacity * mask_at(i)).clamp(0.0, 1.0);
                        for c in 0..4 {
                            dst[c] += (src[c] - dst[c]) * f;
                        }
                    }
                } else {
                    blend_content(acc, &inner.px, layer, &mask_at);
                }
            }
        }
    }
}

/// Evaluate one graph into a single tile. Only the referenced same-set Paint channel is read;
/// source visibility, mask and blend state do not modify this raw input. The graph layer's own
/// mask and channel blend are applied by the normal layer path.
fn eval_paint_graph(source_id: Option<subcraft_core::LayerId>, evaluator: ProceduralEvaluator,
    ts: Option<&TextureSet>, tc: &TileContext, out: &mut [[f32; 4]]) {
    let ao_image = source_id.and_then(|id| ts.and_then(|set| set.find(id))).and_then(|source| match &source.content {
        LayerContent::Paint(paint) => paint.channels.get(&ChannelKind::AmbientOcclusion),
        _ => None,
    });
    let mut ao = ao_image.map(|_| vec![[0.0f32; 4]; TILE_TEXELS]);
    if let (Some(image), Some(values)) = (ao_image, ao.as_mut()) { read_tile_rgba(image, tc.geo, values); }
    for (i, pixel) in out.iter_mut().enumerate() {
        let occlusion = ao.as_ref().map_or(1.0, |values| {
            let value = values[i][0];
            if value.is_finite() { value.clamp(0.0, 1.0) } else { 1.0 }
        });
        let value = evaluator.evaluate(tc.geo.uv(i), occlusion).unwrap_or(0.0);
        *pixel = [value, value, value, 1.0];
    }
}

/// Read a paint raster (`components` values followed by alpha) as `[v0, v1, v2, alpha]`.
fn read_paint(img: &subcraft_image::TiledImage, kind: ChannelKind, tc: &TileContext, out: &mut [[f32; 4]]) {
    read_tile_rgba(img, tc.geo, out);
    let n = kind.components();
    for px in out.iter_mut() {
        let a = px[n];
        *px = if n == 1 { [px[0], px[0], px[0], a] } else { [px[0], px[1], px[2], a] };
    }
}

/// Evaluate one channel of a fill as `[v0, v1, v2, alpha]`; false if the channel is off/absent.
fn eval_fill(fill: &subcraft_doc::FillContent, kind: ChannelKind, tc: &TileContext, out: &mut [[f32; 4]]) -> bool {
    let Some(ch) = fill.channels.get(&kind).filter(|c| c.enabled) else { return false };
    eval_fill_tile(&ch.source, kind.is_color(), &fill.projection, tc.geo, &tc.sample, out);
    crate::strength::apply_strength(kind, ch.strength, out);
    if kind.components() == 1 {
        for px in out.iter_mut() {
            *px = [px[0], px[0], px[0], px[3]];
        }
    }
    true
}

/// Run a layer's content effect stack (bottom to top) on one channel of its content.
///
/// Sources (Paint, Fill) composite their `(value, alpha)` with the effect's channel blend.
/// Adjustments (Levels) blend the transformed value over the content and keep its coverage,
/// so they never make transparent content visible.
fn apply_content_effects(effects: &[Arc<ContentEffect>], acc: &Acc, content: &mut [[f32; 4]], tc: &TileContext) {
    let kind = acc.kind;
    let mut e = vec![[0.0f32; 4]; TILE_TEXELS];
    for effect in effects.iter().filter(|e| e.visible) {
        let blend = effect.blend_for(kind);
        if !blend.is_active() {
            continue;
        }
        let mode = effective_mode(blend.mode);
        match &effect.kind {
            ContentEffectKind::Paint { channels } => {
                let Some(img) = channels.get(&kind) else { continue };
                read_paint(img, kind, tc, &mut e);
            }
            ContentEffectKind::Fill(fill) => {
                if !eval_fill(fill, kind, tc, &mut e) {
                    continue;
                }
            }
            ContentEffectKind::Levels { channel, levels } => {
                if *channel != kind || kind == ChannelKind::Normal {
                    continue;
                }
                for px in content.iter_mut() {
                    let mut dst = [px[0], px[1], px[2], 1.0];
                    let v = [levels.apply(px[0]), levels.apply(px[1]), levels.apply(px[2])];
                    composite(&mut dst, v, blend.opacity, mode, acc.target);
                    px[..3].copy_from_slice(&dst[..3]);
                }
                continue;
            }
        }
        let ignore_alpha = mode == BlendMode::Replace;
        for (dst, src) in content.iter_mut().zip(&e) {
            let a = if ignore_alpha { 1.0 } else { src[3] } * blend.opacity;
            composite(dst, [src[0], src[1], src[2]], a, mode, acc.target);
        }
    }
}

/// Composite `(value, alpha)` content onto a channel with the layer's mode, opacity and mask.
fn blend_content(acc: &mut Acc, content: &[[f32; 4]], layer: &Layer, mask_at: &dyn Fn(usize) -> f32) {
    let blend = layer.blend_for(acc.kind);
    let mode = effective_mode(blend.mode);
    let ignore_alpha = mode == BlendMode::Replace;
    for (i, (dst, src)) in acc.px.iter_mut().zip(content).enumerate() {
        let a = if ignore_alpha { 1.0 } else { src[3] } * blend.opacity * mask_at(i);
        composite(dst, [src[0], src[1], src[2]], a, mode, acc.target);
    }
}

#[cfg(test)]
mod procedural_graph_tests {
    use std::sync::Arc;
    use super::BlendMode;
    use subcraft_core::{ChannelKind, LayerId};
    use subcraft_doc::{ChannelDef, ChannelFormat, Layer, LayerContent, Mask, ProceduralGraph,
        ResourceStore, TextureSet};
    use subcraft_image::TiledImage;
    use subcraft_materials::{MeshMaps, TextureSetGeometry};
    use subcraft_mesh::primitives;
    use crate::{CompositeCache, EvalContext, composite_full};

    const RES: u32 = 128;
    const R: ChannelKind = ChannelKind::Roughness;
    const AO: ChannelKind = ChannelKind::AmbientOcclusion;

    struct Fixture { geometry: TextureSetGeometry, resources: ResourceStore, maps: MeshMaps }
    impl Fixture {
        fn new() -> Self { Self {
            geometry: TextureSetGeometry::new(Arc::new(primitives::plane(2.0, 2)), 0, RES),
            resources: ResourceStore::default(), maps: MeshMaps::new(),
        } }
        fn context(&self) -> EvalContext<'_> { EvalContext {
            geometry: &self.geometry, resources: &self.resources, mesh_maps: &self.maps,
        } }
    }

    fn texture_set() -> TextureSet {
        let mut ts = TextureSet::new("Graph", 0, RES);
        ts.channels.push(ChannelDef { kind: AO, format: ChannelFormat::Bit8 });
        ts
    }

    fn source_layer(ts: &TextureSet, value: f32) -> (LayerId, Layer) {
        let mut source = Layer::new_paint("Raw AO", ts);
        let LayerContent::Paint(paint) = &mut source.content else { unreachable!() };
        paint.channels.insert(AO, TiledImage::new(RES, RES, 2, [value, 0.0, 0.0, 0.0]));
        (source.id, source)
    }

    fn graph_layer(ts: &TextureSet, source: LayerId, seed: u64, scale: f32) -> Layer {
        let mut layer = Layer::new_paint("Generated Roughness", ts);
        let LayerContent::Paint(paint) = &mut layer.content else { unreachable!() };
        paint.procedural_graph = Some(ProceduralGraph::value_noise_times_occlusion(seed, scale, source));
        layer
    }

    fn close(a: f32, b: f32) -> bool { (a - b).abs() < 2e-3 }

    #[test]
    fn roughness_graph_uses_raw_same_set_ao_and_preserves_layer_semantics() {
        let fx = Fixture::new();
        let mut ts = texture_set();
        let template = ts.clone();
        let (source_id, mut source) = source_layer(&template, 0.5);
        // Kalium semantics read the raw referenced channel despite source layer visibility,
        // mask and blend settings. Only the generated layer's own mask/blend is applied.
        source.visible = false;
        source.mask = Some(Mask::new(RES, false));
        source.blend.get_mut(&AO).unwrap().mode = BlendMode::Disable;
        ts.insert_above(None, source);
        let mut target = graph_layer(&template, source_id, 71, 4.0);
        let LayerContent::Paint(paint) = &mut target.content else { unreachable!() };
        let x = 44;
        let y = 39;
        let mut authored = TiledImage::new(RES, RES, 2, [0.0; 4]);
        authored.set(x, y, [0.9, 0.5, 0.0, 0.0]);
        paint.channels.insert(R, authored);
        let graph = paint.procedural_graph.as_ref().unwrap().compile().unwrap();
        let uv = [(x as f32 + 0.5) / RES as f32, (y as f32 + 0.5) / RES as f32];
        let generated = graph.evaluate(uv, 0.5).unwrap();
        let target_id = ts.insert_above(None, target);
        let composite = composite_full(&ts, &fx.context());
        assert!(close(composite.channel(R).unwrap().get(x, y)[0], generated + (0.9 - generated) * 0.5));
        let neighbor_uv = [(x as f32 + 1.5) / RES as f32, uv[1]];
        assert!(close(composite.channel(R).unwrap().get(x + 1, y)[0], graph.evaluate(neighbor_uv, 0.5).unwrap()));

        // The graph target's own mask and disabled-channel state retain normal layer behavior.
        ts.find_mut(target_id).unwrap().mask = Some(Mask::new(RES, false));
        assert!(close(composite_full(&ts, &fx.context()).channel(R).unwrap().get(x, y)[0], R.default_value().0[0]));
        ts.find_mut(target_id).unwrap().mask = None;
        ts.find_mut(target_id).unwrap().blend.get_mut(&R).unwrap().mode = BlendMode::Disable;
        assert!(close(composite_full(&ts, &fx.context()).channel(R).unwrap().get(x, y)[0], R.default_value().0[0]));
    }

    #[test]
    fn source_ao_edit_dirties_its_graph_tile_and_graph_edit_dirties_all_tiles() {
        let fx = Fixture::new();
        let mut ts = texture_set();
        let template = ts.clone();
        let (source_id, source) = source_layer(&template, 0.25);
        let source_node = ts.insert_above(None, source);
        let graph_node = ts.insert_above(None, graph_layer(&template, source_id, 23, 5.0));
        let context = fx.context();
        let mut cache = CompositeCache::new();
        assert_eq!(cache.update(&ts, &context), vec![0, 1, 2, 3]);

        let LayerContent::Paint(paint) = &mut ts.find_mut(source_node).unwrap().content else { unreachable!() };
        paint.channels.get_mut(&AO).unwrap().set(70, 70, [0.9, 0.0, 0.0, 0.0]);
        assert_eq!(cache.update(&ts, &context), vec![3]);
        assert_eq!(cache.composite().unwrap(), &composite_full(&ts, &context));

        let LayerContent::Paint(paint) = &mut ts.find_mut(graph_node).unwrap().content else { unreachable!() };
        paint.procedural_graph = Some(ProceduralGraph::value_noise_times_occlusion(23, 10.0, source_id));
        assert_eq!(cache.update(&ts, &context), vec![0, 1, 2, 3]);
        assert_eq!(cache.composite().unwrap(), &composite_full(&ts, &context));
    }
}
