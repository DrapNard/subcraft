use super::*;
use crate::{read_project, write_project};
use std::io::{Cursor, Write};
use subcraft_image::{Bitmap, BitmapData};
use zip::{ZipWriter, write::SimpleFileOptions};

fn png(width: u32, height: u32, channels: u8, pixels: Vec<u8>) -> Vec<u8> {
    subcraft_image::io::encode_bitmap(&Bitmap {
        width,
        height,
        channels,
        data: BitmapData::U8(pixels),
    })
    .unwrap()
    .0
}

fn fixture() -> Vec<u8> {
    fixture_with_size(128)
}

fn fixture_with_size(size: u32) -> Vec<u8> {
    let manifest = serde_json::json!({
        "format": "Kalium Project",
        "version": 9,
        "project_id": "e7e4c818-8851-4f96-a532-c78fc4fabf75",
        "mesh": {
            "name": "Triangle",
            "vertices": [
                {"position":[0.0,0.0,0.0],"normal":[0.0,0.0,1.0],"uv":[0.0,0.0],"tangent":null},
                {"position":[1.0,0.0,0.0],"normal":[0.0,0.0,1.0],"uv":[1.0,0.0],"tangent":null},
                {"position":[0.0,1.0,0.0],"normal":[0.0,0.0,1.0],"uv":[0.0,1.0],"tangent":null}
            ],
            "indices": [0,1,2], "materials": ["Default"], "triangle_materials": [0], "extra_uv_sets": {}
        },
        "size": size,
        "selected": 0,
        "next_layer_id": 5,
        "layers": [],
        "active_set_id": 1,
        "active_set_name": "Default",
        "next_set_id": 2,
        "material_bindings": [null],
        "layer_tree": {
            "selected_node_id": 2,
            "next_node_id": 5,
            "roots": [{
                "kind":"group", "id":4, "name":"Nested", "visible":true, "opacity":1.0,
                "blend":"Normal", "disabled_channels":[], "children":[
                    {"kind":"paint", "id":2, "name":"Paint", "visible":true, "opacity":1.0,
                     "blend":"Normal", "disabled_channels":[], "channels":["BaseColor","Roughness"], "procedural_graph":null},
                    {"kind":"fill", "id":3, "name":"Fill", "visible":true, "opacity":1.0,
                     "blend":"Normal", "disabled_channels":[], "values":[
                        {"channel":"BaseColor","value":[32,160,220,255]},
                        {"channel":"Metallic","value":[96,96,96,255]}
                     ]}
                ]
            }]
        },
        "texture_sets": []
    });
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("manifest.json", options).unwrap();
    zip.write_all(&serde_json::to_vec(&manifest).unwrap())
        .unwrap();
    let mut mask = vec![255u8; size as usize * size as usize];
    mask[5 * size as usize + 7] = 0;
    zip.start_file("layers/4/mask.png", options).unwrap();
    zip.write_all(&png(size, size, 1, mask)).unwrap();
    let mut mask = vec![255u8; size as usize * size as usize];
    mask[5 * size as usize + 7] = 0;
    zip.start_file("layers/2/mask.png", options).unwrap();
    zip.write_all(&png(size, size, 1, mask)).unwrap();
    zip.start_file("layers/3/mask.png", options).unwrap();
    zip.write_all(&png(
        size,
        size,
        1,
        vec![255u8; size as usize * size as usize],
    ))
    .unwrap();
    let mut base = vec![0u8; size as usize * size as usize * 4];
    for p in base.chunks_exact_mut(4) {
        p.copy_from_slice(&[200, 90, 20, 255]);
    }
    base[(6 * size as usize + 8) * 4..(6 * size as usize + 8) * 4 + 4]
        .copy_from_slice(&[11, 22, 33, 128]);
    zip.start_file("layers/2/BaseColor.png", options).unwrap();
    zip.write_all(&png(size, size, 4, base)).unwrap();
    let mut roughness = vec![0u8; size as usize * size as usize * 4];
    for p in roughness.chunks_exact_mut(4) {
        p.copy_from_slice(&[77, 77, 77, 255]);
    }
    zip.start_file("layers/2/Roughness.png", options).unwrap();
    zip.write_all(&png(size, size, 4, roughness)).unwrap();
    zip.finish().unwrap().into_inner()
}

fn fixture_with_procedural_graph(source_layer_id: u32) -> Vec<u8> {
    let bytes = fixture();
    let mut archive = ZipArchive::new(Cursor::new(bytes.clone())).unwrap();
    let mut text = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let mut manifest: JsonValue = serde_json::from_str(&text).unwrap();
    manifest["layer_tree"]["roots"][0]["children"][0]["procedural_graph"] =
        serde_json::json!({
            "nodes": [
                {"id":1,"kind":{"type":"uv"}},
                {"id":2,"kind":{"type":"value_noise","seed":71,"scale":4.0}},
                {"id":3,"kind":{"type":"occlusion_input","source_layer_id":source_layer_id}},
                {"id":4,"kind":{"type":"multiply"}},
                {"id":5,"kind":{"type":"roughness_output"}}
            ],
            "edges": [
                {"from":1,"to":2}, {"from":2,"to":4},
                {"from":3,"to":4}, {"from":4,"to":5}
            ]
        });
    rewrite_manifest(bytes, manifest)
}

#[test]
fn imports_v9_nested_paint_fill_masks_and_survives_subcraft_save_reopen() {
    let bytes = fixture();
    let imported = read_kalium_project(Cursor::new(bytes.clone())).unwrap();
    assert_eq!(imported.mesh.triangle_count(), 1);
    assert_eq!(imported.mesh.materials[0].name, "Default");
    assert_eq!(imported.document.texture_sets.len(), 1);
    let set = &imported.document.texture_sets[0];
    assert_eq!(set.resolution, 128);
    assert_eq!(set.channels.len(), 8);
    assert_eq!(set.layers.len(), 2); // explicit Kalium background + source Group
    let group = &set.layers[1];
    let LayerContent::Group(group_content) = &group.content else {
        panic!("group was flattened");
    };
    assert_eq!(group_content.children.len(), 2);
    let paint = &group_content.children[0];
    assert_eq!(paint.name, "Paint");
    assert_eq!(paint.mask.as_ref().unwrap().base.get(7, 5)[0], 0.0);
    let LayerContent::Paint(paint_content) = &paint.content else {
        panic!("paint data was dropped");
    };
    let base = paint_content.channels.get(&ChannelKind::BaseColor).unwrap();
    assert_eq!(base.get(7, 5)[3], 1.0);
    assert!((base.get(8, 6)[3] - 128.0 / 255.0).abs() < 0.001);
    assert!((base.get(8, 6)[0] - subcraft_core::color::srgb_to_linear(11.0 / 255.0)).abs() < 0.001);
    let roughness = paint_content.channels.get(&ChannelKind::Roughness).unwrap();
    assert_eq!(roughness.comps(), 2); // scalar value plus independent alpha/coverage
    assert!((roughness.get(0, 0)[0] - 77.0 / 255.0).abs() < 0.001);
    assert_eq!(roughness.get(0, 0)[1], 1.0);
    let fill = &group_content.children[1];
    assert_eq!(fill.name, "Fill");
    let LayerContent::Fill(fill_content) = &fill.content else {
        panic!("fill was not retained as editable content");
    };
    let FillSource::Uniform { value } = &fill_content.channels[&ChannelKind::Metallic].source
    else {
        panic!("fill source was not retained");
    };
    assert_eq!(value.0, [96.0 / 255.0; 3]);

    let saved = write_project(
        Cursor::new(Vec::new()),
        &imported.mesh,
        &imported.document,
        &imported.resources,
        &imported.mesh_maps,
    )
    .unwrap()
    .into_inner();
    let reopened = read_project(Cursor::new(saved)).unwrap();
    let set = &reopened.document.texture_sets[0];
    let LayerContent::Group(group) = &set.layers[1].content else {
        panic!("saved group disappeared");
    };
    let LayerContent::Paint(paint) = &group.children[0].content else {
        panic!("saved paint disappeared");
    };
    assert!((paint.channels[&ChannelKind::Roughness].get(0, 0)[0] - 77.0 / 255.0).abs() < 0.001);
    assert_eq!(paint.channels[&ChannelKind::Roughness].get(0, 0)[1], 1.0);
    assert_eq!(
        group.children[0].mask.as_ref().unwrap().base.get(7, 5)[0],
        0.0
    );
    let LayerContent::Fill(fill) = &group.children[1].content else {
        panic!("saved fill disappeared");
    };
    let FillSource::Uniform { value } = &fill.channels[&ChannelKind::Metallic].source else {
        panic!("saved fill source disappeared");
    };
    assert_eq!(value.0, [96.0 / 255.0; 3]);
}

#[test]
fn imports_64px_kalium_texture_sets() {
    let imported = read_kalium_project(Cursor::new(fixture_with_size(64))).unwrap();
    assert_eq!(imported.document.texture_sets[0].resolution, 64);
    let LayerContent::Group(group) = &imported.document.texture_sets[0].layers[1].content else {
        panic!("group was flattened");
    };
    let LayerContent::Paint(paint) = &group.children[0].content else {
        panic!("paint data was dropped");
    };
    assert!((paint.channels[&ChannelKind::Roughness].get(0, 0)[0] - 77.0 / 255.0).abs() < 0.001);
}

#[test]
fn migrates_kalium_procedural_graph_references_and_roundtrips_them() {
    let project_id = Uuid::parse_str("e7e4c818-8851-4f96-a532-c78fc4fabf75").unwrap();
    let source_layer_id = 2;
    let expected_source = LayerId(stable_layer_id(project_id, 1, source_layer_id));
    let imported = read_kalium_project(Cursor::new(fixture_with_procedural_graph(source_layer_id)))
        .unwrap();
    let set = &imported.document.texture_sets[0];
    let LayerContent::Group(group) = &set.layers[1].content else {
        panic!("group was flattened");
    };
    let LayerContent::Paint(paint) = &group.children[0].content else {
        panic!("paint data was dropped");
    };
    let graph = paint.procedural_graph.as_ref().expect("graph should be imported");
    graph.validate().unwrap();
    assert_eq!(graph.occlusion_source_layer_id(), Some(expected_source));

    let saved = write_project(
        Cursor::new(Vec::new()),
        &imported.mesh,
        &imported.document,
        &imported.resources,
        &imported.mesh_maps,
    )
    .unwrap()
    .into_inner();
    let reopened = read_project(Cursor::new(saved)).unwrap();
    let LayerContent::Group(group) = &reopened.document.texture_sets[0].layers[1].content else {
        panic!("saved group disappeared");
    };
    let LayerContent::Paint(paint) = &group.children[0].content else {
        panic!("saved paint disappeared");
    };
    let reopened_graph = paint
        .procedural_graph
        .as_ref()
        .expect("saved procedural graph disappeared");
    assert_eq!(reopened_graph, graph);
    assert_eq!(reopened_graph.occlusion_source_layer_id(), Some(expected_source));
}

#[test]
fn rejects_zero_procedural_occlusion_source_id() {
    let error = read_kalium_project(Cursor::new(fixture_with_procedural_graph(0)))
        .unwrap_err()
        .to_string();
    assert!(error.contains("references layer ID zero"), "{error}");
}

#[test]
fn retains_missing_procedural_source_as_stable_neutral_fallback_reference() {
    let project_id = Uuid::parse_str("e7e4c818-8851-4f96-a532-c78fc4fabf75").unwrap();
    let missing_source = 999;
    let imported = read_kalium_project(Cursor::new(fixture_with_procedural_graph(missing_source)))
        .unwrap();
    let LayerContent::Group(group) = &imported.document.texture_sets[0].layers[1].content else {
        panic!("group was flattened");
    };
    let LayerContent::Paint(paint) = &group.children[0].content else {
        panic!("paint data was dropped");
    };
    let graph = paint.procedural_graph.as_ref().expect("graph should be imported");
    assert_eq!(
        graph.occlusion_source_layer_id(),
        Some(LayerId(stable_layer_id(project_id, 1, missing_source)))
    );
}

#[test]
fn rejects_unknown_metadata_instead_of_dropping_it() {
    let mut unknown = fixture();
    let mut archive = ZipArchive::new(Cursor::new(unknown.clone())).unwrap();
    let mut text = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let mut manifest: JsonValue = serde_json::from_str(&text).unwrap();
    manifest["futureProjectSetting"] = serde_json::json!({"preserve":"me"});
    unknown = rewrite_manifest(unknown, manifest);
    assert!(
        read_kalium_project(Cursor::new(unknown))
            .unwrap_err()
            .to_string()
            .contains("unknown Kalium project metadata")
    );
}

#[test]
fn rejects_unknown_fill_value_metadata_instead_of_dropping_it() {
    let unknown = fixture();
    let mut archive = ZipArchive::new(Cursor::new(unknown.clone())).unwrap();
    let mut text = String::new();
    archive
        .by_name("manifest.json")
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let mut manifest: JsonValue = serde_json::from_str(&text).unwrap();
    manifest["layer_tree"]["roots"][0]["children"][1]["values"][0]["future_setting"] =
        serde_json::json!({"preserve": true});
    let unknown = rewrite_manifest(unknown, manifest);
    let error = read_kalium_project(Cursor::new(unknown))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unknown Kalium fill value metadata field 'future_setting'"),
        "{error}"
    );
}

#[test]
fn rejects_non_grayscale_scalar_source_instead_of_losing_channels() {
    let size = 128;
    let pixels = vec![10, 20, 30, 255].repeat(size as usize * size as usize);
    let bytes = rewrite_entry(
        fixture(),
        "layers/2/Roughness.png",
        png(size, size, 4, pixels),
    );
    let error = read_kalium_project(Cursor::new(bytes))
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-grayscale RGB data"), "{error}");
}

fn rewrite_manifest(bytes: Vec<u8>, manifest: JsonValue) -> Vec<u8> {
    rewrite_archive(bytes, "manifest.json", Some(manifest), None)
}

fn rewrite_entry(bytes: Vec<u8>, replaced_name: &str, data: Vec<u8>) -> Vec<u8> {
    rewrite_archive(bytes, replaced_name, None, Some(data))
}

fn rewrite_archive(
    bytes: Vec<u8>,
    replaced_name: &str,
    manifest: Option<JsonValue>,
    replacement: Option<Vec<u8>>,
) -> Vec<u8> {
    let mut input = ZipArchive::new(Cursor::new(bytes)).unwrap();
    let mut output = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for index in 0..input.len() {
        let mut entry = input.by_index(index).unwrap();
        let name = entry.name().unwrap().to_owned();
        output.start_file(&name, options).unwrap();
        if name == replaced_name {
            if name == "manifest.json" {
                output
                    .write_all(&serde_json::to_vec(manifest.as_ref().unwrap()).unwrap())
                    .unwrap();
            } else {
                output.write_all(replacement.as_ref().unwrap()).unwrap();
            }
        } else {
            let mut data = Vec::new();
            entry.read_to_end(&mut data).unwrap();
            output.write_all(&data).unwrap();
        }
    }
    output.finish().unwrap().into_inner()
}
