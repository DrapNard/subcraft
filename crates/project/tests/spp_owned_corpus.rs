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
