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
