//! Optional private-corpus acceptance. No Painter project or resources are bundled.
use std::{io::Read, path::PathBuf};
use subcraft_project::spp_checksum::spp_dataset_checksum;
use subcraft_project::spp_document::{MAX_DATASET_BYTES, SppDocumentRecords};

#[test]
#[ignore = "requires SUBCRAFT_SPP_RESEARCH_DIR with the six owned dataset extracts"]
fn owned_reflective_datasets_reencode_exactly() {
    let root = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR")
            .expect("set SUBCRAFT_SPP_RESEARCH_DIR to the private dataset directory"),
    );
    for name in [
        "baseline-full",
        "same-project-full-copy",
        "renamed-full",
        "opacity-full",
        "hidden-full",
        "visible-incremental",
    ] {
        let mut bytes = Vec::new();
        std::fs::File::open(root.join(format!("{name}.document.bin")))
            .unwrap()
            .take((MAX_DATASET_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .unwrap();
        let checksum_text =
            std::fs::read_to_string(root.join(format!("{name}.checksum.txt"))).unwrap();
        assert!(checksum_text.len() < 128);
        let words: Vec<u64> = checksum_text
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        assert_eq!(words.len(), 2);
        assert_eq!(
            spp_dataset_checksum(&bytes),
            [words[0], words[1]],
            "{name} checksum"
        );
        let records = SppDocumentRecords::decode(&bytes).unwrap();
        assert_eq!(records.encode().unwrap(), bytes, "{name}");
    }
}

#[cfg(all(feature = "spp-hdf5", not(target_arch = "wasm32")))]
#[test]
#[ignore = "requires the owned opacity SPP fixture and writes an edited copy to the private corpus"]
fn owned_native_spp_property_edits_preserve_source() {
    use subcraft_project::{
        spp_container::{read_document, save_document_copy},
        spp_semantics::{
            enumerate_layers, rename_layer, set_base_color_opacity, set_layer_visibility,
        },
    };
    let root = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR").expect("set private corpus directory"),
    );
    let source = root.join("opacity-full.spp");
    let before = std::fs::read(&source).unwrap();
    let mut document = read_document(&source).unwrap();
    let layers = enumerate_layers(&document.records).unwrap();
    assert_eq!(layers.len(), 1);
    let uid = layers[0].uid;
    rename_layer(&mut document.records, uid, "SubCraft").unwrap();
    set_layer_visibility(&mut document.records, uid, false).unwrap();
    set_base_color_opacity(&mut document.records, uid, 0.625).unwrap();
    let target = root.join("subcraft-rust-edited.spp");
    save_document_copy(&source, &target, &document).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let reopened = read_document(&target).unwrap();
    let changed = enumerate_layers(&reopened.records).unwrap();
    assert_eq!(changed[0].label, "SubCraft");
    assert!(!changed[0].enabled);
    assert_eq!(changed[0].base_color_opacity, 0.625);
}

#[cfg(all(feature = "spp-hdf5", not(target_arch = "wasm32")))]
#[test]
#[ignore = "requires the Painter-edited return copy from the private controlled roundtrip"]
fn painter_return_property_edits_are_read_by_rust() {
    use subcraft_project::{spp_container::read_document, spp_semantics::enumerate_layers};
    let root = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR").expect("set private corpus directory"),
    );
    let document = read_document(&root.join("subcraft-painter-return.spp")).unwrap();
    let layers = enumerate_layers(&document.records).unwrap();
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].label, "Painter2");
    assert!(layers[0].enabled);
    assert_eq!(layers[0].base_color_opacity, 0.25);
}

#[test]
#[ignore = "requires the five owned checksum-verified scene extracts and original OBJ sources"]
fn owned_scene_geometry_deltas_and_exact_reencoding() {
    use subcraft_project::spp_scene::SppScene;
    let root = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR").expect("set private corpus directory"),
    )
    .join("mesh-12.1.5");
    let load = |name: &str| {
        let bytes = std::fs::read(root.join(format!("{name}.prefix.scene.bin"))).unwrap();
        let scene = SppScene::decode(&bytes).unwrap();
        assert_eq!(scene.encode().unwrap(), bytes, "{name} byte identity");
        assert_eq!(scene.meshes.len(), 1);
        let canonical = subcraft_project::spp_scene_mesh::build_reference_mesh(&scene).unwrap();
        assert!(canonical.mesh.validate().is_empty());
        assert_eq!(
            canonical.mesh.triangle_count(),
            scene.meshes[0].triangles.len()
        );
        scene
    };
    let base = load("baseline_quad");
    let mesh = &base.meshes[0];
    assert_eq!(
        mesh.positions,
        [[-1., -1., 0.], [1., -1., 0.], [1., 1., 0.], [-1., 1., 0.]]
    );
    assert_eq!(mesh.normals, [[0., 0., 1.]; 4]);
    assert_eq!(
        mesh.uvs,
        [[0., 0., 0.], [1., 0., 0.], [1., 1., 0.], [0., 1., 0.]]
    );
    assert_eq!(mesh.triangles, [[0, 1, 2], [0, 2, 3]]);
    let uv = load("uv_u");
    let mut expected_uv = mesh.clone();
    expected_uv.uvs[2][0] = 0.875;
    assert_eq!(uv.meshes[0], expected_uv);
    let normal = load("normal_sign");
    assert_eq!(normal.meshes[0].positions, mesh.positions);
    assert_eq!(normal.meshes[0].normals, [[0., 0., -1.]; 4]);
    assert_eq!(normal.meshes[0].bitangents, [[0., -1., 0.]; 4]);
    let triangle = load("triangle_count");
    assert_eq!(triangle.meshes[0].positions, mesh.positions[..3]);
    assert_eq!(triangle.meshes[0].triangles, [[0, 1, 2]]);
    let vertex = load("vertex_x");
    let positions = &vertex.meshes[0].positions;
    // Painter normalizes the source bounds: center (0.0625,0,0), half-width 1.0625.
    let source = [
        [-1_f32, -1_f32, 0_f32],
        [1.125, -1., 0.],
        [1., 1., 0.],
        [-1., 1., 0.],
    ];
    for (actual, original) in positions.iter().zip(source) {
        let expected = [(original[0] - 0.0625) / 1.0625, original[1] / 1.0625, 0.];
        for i in 0..3 {
            assert!((actual[i] - expected[i]).abs() < 1e-6);
        }
    }
    assert_eq!(vertex.meshes[0].uvs, mesh.uvs);
}

#[cfg(all(feature = "spp-hdf5", not(target_arch = "wasm32")))]
#[test]
#[ignore = "requires original owned baseline SPP; writes only an edited copy in private corpus"]
fn owned_native_scene_uv_edit_preserves_source_and_document() {
    use subcraft_project::spp_container::{
        read_document, read_scene, read_scene_bytes, save_scene_copy,
    };
    use subcraft_project::spp_scene::SppScene;
    let root = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR").expect("set private corpus directory"),
    );
    let source = root.join("baseline-full.spp");
    let before = std::fs::read(&source).unwrap();
    let original_document = read_document(&source).unwrap().records;
    let bytes = read_scene_bytes(&source).unwrap();
    let mut scene = read_scene(&source).unwrap();
    assert_eq!(scene.scene.encode().unwrap(), bytes);
    assert_eq!(
        SppScene::decode(&bytes).unwrap().meshes[0].uvs[2],
        [1., 1., 0.]
    );
    scene.scene.meshes[0].uvs[2][0] = 0.875;
    let target = root.join("subcraft-rust-scene-edited.spp");
    save_scene_copy(&source, &target, &scene).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(read_scene(&target).unwrap().scene, scene.scene);
    assert_eq!(read_document(&target).unwrap().records, original_document);
}

#[test]
#[ignore = "requires owned structural scene extracts and the Painter saved UV return extract"]
fn owned_structural_scenes_and_painter_uv_return_reencode_exactly() {
    use subcraft_project::spp_scene::SppScene;
    let root = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR").expect("set private corpus directory"),
    )
    .join("mesh-12.1.5");
    for name in [
        "structural_single_object",
        "structural_two_objects_shared_material",
        "structural_two_material_slots",
        "painter-uv-return",
    ] {
        let bytes = std::fs::read(root.join(format!("{name}.prefix.scene.bin"))).unwrap();
        let scene = SppScene::decode(&bytes).unwrap();
        assert_eq!(scene.encode().unwrap(), bytes, "{name}");
        let hierarchy = scene.hierarchy().unwrap();
        let materials = scene.materials().unwrap();
        let canonical = subcraft_project::spp_scene_mesh::build_reference_mesh(&scene).unwrap();
        assert!(canonical.mesh.validate().is_empty());
        assert_eq!(
            canonical.mesh.triangle_count(),
            scene.meshes.iter().map(|m| m.triangles.len()).sum::<usize>()
        );
        for (source_index, target_slot) in
            canonical.material_slot_by_source_index.iter().enumerate()
        {
            if let Some(slot) = target_slot {
                assert_eq!(
                    Some(canonical.mesh.materials[*slot as usize].name.as_str()),
                    materials.materials[source_index].name().unwrap()
                );
            }
        }

        let mut restored = scene.clone();
        restored.set_hierarchy(&hierarchy).unwrap();
        restored.set_materials(&materials).unwrap();
        assert_eq!(
            restored.encode().unwrap(),
            bytes,
            "{name} typed hierarchy/material identity"
        );
        if name == "structural_single_object" {
            assert_eq!(hierarchy.children.len(), 1);
            assert_eq!(hierarchy.children[0].name, "SingleObject");
            assert_eq!(hierarchy.children[0].mesh_indices, [0]);
            assert_eq!(
                materials.materials[0].name().unwrap(),
                Some("SharedMaterial")
            );
        }
        if name == "painter-uv-return" {
            assert_eq!(scene.meshes.len(), 1);
            assert_eq!(scene.meshes[0].uvs[2], [0.875, 1., 0.]);
        }
        if name == "structural_two_material_slots" {
            assert_eq!(scene.meshes.len(), 2);
            assert_ne!(
                scene.meshes[0].material_index,
                scene.meshes[1].material_index
            );
            assert_eq!(hierarchy.children[0].mesh_indices, [0, 1]);
            assert_eq!(materials.materials[0].name().unwrap(), Some("RedSlot"));
            assert_eq!(materials.materials[1].name().unwrap(), Some("BlueSlot"));
            for mesh in &scene.meshes {
                let material = materials.materials[mesh.material_index as usize]
                    .name()
                    .unwrap()
                    .unwrap();
                let points = mesh.triangles[0].map(|i| mesh.positions[i as usize]);
                let expected = match material {
                    "RedSlot" => [[-1., -1., 0.], [1., -1., 0.], [1., 1., 0.]],
                    "BlueSlot" => [[-1., -1., 0.], [1., 1., 0.], [-1., 1., 0.]],
                    _ => panic!("unexpected original fixture material"),
                };
                assert_eq!(points, expected);
            }
        }
        if name == "structural_two_objects_shared_material" {
            assert_eq!(scene.meshes.len(), 2);
            assert_eq!(
                scene.meshes[0].material_index,
                scene.meshes[1].material_index
            );
            assert_eq!(hierarchy.children.len(), 2);
            assert_eq!(hierarchy.children[0].name, "ObjectA");
            assert_eq!(hierarchy.children[0].mesh_indices, [0]);
            assert_eq!(hierarchy.children[1].name, "ObjectB");
            assert_eq!(hierarchy.children[1].mesh_indices, [1]);
        }
    }
}

#[test]
#[ignore = "requires the four owned checksum-verified uniform fill document extracts"]
fn owned_uniform_fill_records_reencode_and_decode_linear_colors() {
    use subcraft_project::{
        spp_document::SppDocumentRecords, spp_fill::enumerate_uniform_base_color_sources,
    };
    let root = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR").expect("set private corpus directory"),
    )
    .join("layers-12.1.5");
    for (name, expected) in [
        ("blank_baseline", vec![]),
        (
            "single_fill_basecolor_0125_0375_0625",
            vec![[0.125, 0.375, 0.625]],
        ),
        (
            "single_fill_basecolor_025_0375_0625",
            vec![[0.25, 0.375, 0.625]],
        ),
        (
            "two_named_fill_layers_order",
            vec![[0.1, 0.2, 0.3], [0.7, 0.6, 0.5]],
        ),
        ("painter-fill-return", vec![[0.25, 0.375, 0.625]]),
    ] {
        let bytes = std::fs::read(root.join(format!("{name}.prefix.document.bin"))).unwrap();
        let records = SppDocumentRecords::decode(&bytes).unwrap();
        assert_eq!(records.encode().unwrap(), bytes, "{name}");
        let sources = enumerate_uniform_base_color_sources(&records).unwrap();
        assert_eq!(sources.len(), expected.len(), "{name}");
        for (source, expected) in sources.iter().zip(expected) {
            let linear = subcraft_core::color::srgb3_to_linear(expected);
            for i in 0..3 {
                assert!(
                    (source.color[i] - linear[i]).abs() < 1e-6,
                    "{name} component {i}"
                );
            }
        }
    }
}

#[cfg(all(feature = "spp-hdf5", not(target_arch = "wasm32")))]
#[test]
#[ignore = "requires full owned single-fill SPP; writes only an edited copy in private corpus"]
fn owned_native_uniform_fill_color_edit_preserves_source() {
    use subcraft_project::{
        spp_container::{read_document, save_document_copy},
        spp_document::SppDocumentRecords,
        spp_fill::{enumerate_uniform_base_color_sources, set_uniform_base_color_source},
    };
    let root = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR").expect("set private corpus directory"),
    );
    let source = root.join("single-fill-original-full.spp");
    let before = std::fs::read(&source).unwrap();
    let target_records = SppDocumentRecords::decode(
        &std::fs::read(
            root.join("layers-12.1.5/single_fill_basecolor_025_0375_0625.prefix.document.bin"),
        )
        .unwrap(),
    )
    .unwrap();
    let target_color = enumerate_uniform_base_color_sources(&target_records).unwrap()[0].color;
    let mut document = read_document(&source).unwrap();
    let uid = enumerate_uniform_base_color_sources(&document.records).unwrap()[0].uid;
    set_uniform_base_color_source(&mut document.records, uid, target_color).unwrap();
    let target = root.join("subcraft-rust-fill-edited.spp");
    save_document_copy(&source, &target, &document).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(read_document(&target).unwrap().records, document.records);
    assert_eq!(
        enumerate_uniform_base_color_sources(&read_document(&target).unwrap().records).unwrap()[0]
            .color,
        target_color
    );
}

#[cfg(all(feature = "spp-worker", target_os = "linux"))]
#[test]
#[ignore = "requires SUBCRAFT_SPP_WORKER_EXE and the owned full uniform-fill SPP fixture"]
fn owned_isolated_worker_reads_and_saves_combined_edits() {
    use subcraft_project::{spp_container, spp_fill, spp_worker};
    let root =
        PathBuf::from(std::env::var_os("SUBCRAFT_SPP_RESEARCH_DIR").expect("private corpus"));
    let executable = PathBuf::from(
        std::env::var_os("SUBCRAFT_SPP_WORKER_EXE").expect("worker-enabled SubCraft executable"),
    );
    let original = root.join("single-fill-original-full.spp");
    let original_bytes = std::fs::read(&original).unwrap();
    let directory = std::env::temp_dir().join(format!(
        "subcraft-spp-worker-corpus-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&directory).unwrap();
    let source = directory.join("source.spp");
    let target = directory.join("edited.spp");
    std::fs::copy(&original, &source).unwrap();
    let mut project = spp_worker::read_project(&executable, &source).unwrap();
    let direct = spp_container::read_project(&source).unwrap();
    assert_eq!(project.records, direct.records);
    assert_eq!(
        project.scene.encode().unwrap(),
        direct.scene.encode().unwrap()
    );
    assert_eq!(project.source_digest(), direct.source_digest());
    spp_worker::save_project_copy(&executable, &source, &target, &project).unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), original_bytes);
    let uniform = spp_fill::enumerate_uniform_base_color_sources(&project.records).unwrap();
    assert_eq!(uniform.len(), 1);
    spp_fill::set_uniform_base_color_source(
        &mut project.records,
        uniform[0].uid,
        [0.050876088, 0.116016142, 0.348510206],
    )
    .unwrap();
    project.scene.meshes[0].uvs[0][0] = 0.875;
    spp_worker::save_project_copy(&executable, &source, &target, &project).unwrap();
    let saved = spp_worker::read_project(&executable, &target).unwrap();
    assert_eq!(saved.records, project.records);
    assert_eq!(
        saved.scene.encode().unwrap(),
        project.scene.encode().unwrap()
    );
    assert_eq!(std::fs::read(&source).unwrap(), original_bytes);
    let target_bytes = std::fs::read(&target).unwrap();
    let file = hdf5::File::open_rw(&source).unwrap();
    file.new_attr::<u32>()
        .create("subcraft-test-conflict")
        .unwrap()
        .write_scalar(&1)
        .unwrap();
    file.close().unwrap();
    assert!(
        spp_worker::save_project_copy(&executable, &source, &target, &project)
            .unwrap_err()
            .contains("revision changed")
    );
    assert_eq!(std::fs::read(&target).unwrap(), target_bytes);
    assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
    std::fs::remove_dir_all(directory).unwrap();
}
