mod common;

use sandbox_runtime_layerstack_core::{decode_root_record, decode_tree_record, Error};

use common::{
    complete_sample_tree, encode_root, encode_tree, identify_root, sample_root,
    validate_tree_bytes_with_peak, BytesSource, CaptureDigest, LogicalOwnerCounter,
};

#[test]
#[ignore = "PRC-R08 std-only lifecycle oracle; run explicitly"]
fn portable_root_tiny_loop() -> Result<(), Error> {
    let entries = complete_sample_tree()?;
    let expected_tree = encode_tree(&entries)?;
    let (validated, _) =
        validate_tree_bytes_with_peak(&expected_tree, &mut CaptureDigest::default())?;
    let expected_root = sample_root(&validated, 23_042, 0x52, None, None)?;
    let expected_root_bytes = encode_root(&expected_root)?;
    let expected_root_id = identify_root(&expected_root, &mut CaptureDigest::default())?;

    let mut completed = 0_u64;
    let mut encoded_bytes = 0_u64;
    let mut peak_codec_scratch_bytes = 0_u64;
    let mut retained_owners = 0_u64;
    for fragment in 1..=256 {
        let owners = LogicalOwnerCounter::default();
        let tree_bytes = encode_tree(&entries)?;
        assert_eq!(tree_bytes, expected_tree);

        let mut decoded_entry_count = 0_usize;
        let mut tree_source = BytesSource::fragmented(&tree_bytes, fragment);
        decode_tree_record(&mut tree_source, &mut |entry| {
            let _entry_owner = owners.enter();
            assert_eq!(entries.get(decoded_entry_count), Some(entry));
            decoded_entry_count += 1;
            Ok(())
        })?;
        assert_eq!(decoded_entry_count, entries.len());
        assert_eq!(owners.live(), 0);
        peak_codec_scratch_bytes = peak_codec_scratch_bytes
            .max(u64::try_from(tree_source.peak_read_bytes()).unwrap_or(u64::MAX));

        let (tree_value, validation_peak) =
            validate_tree_bytes_with_peak(&tree_bytes, &mut CaptureDigest::default())?;
        let tree = owners.track(tree_value);
        peak_codec_scratch_bytes = peak_codec_scratch_bytes.max(validation_peak);
        let root = owners.track(sample_root(&tree, 23_042, 0x52, None, None)?);
        let root_bytes = encode_root(&root)?;
        assert_eq!(root_bytes, expected_root_bytes);
        assert_eq!(
            identify_root(&root, &mut CaptureDigest::default())?,
            expected_root_id,
        );
        let mut root_source = BytesSource::fragmented(&root_bytes, fragment);
        let decoded_root = owners.track(decode_root_record(&mut root_source, &tree)?);
        assert_eq!(&*decoded_root, &*root);
        peak_codec_scratch_bytes = peak_codec_scratch_bytes
            .max(u64::try_from(root_source.peak_read_bytes()).unwrap_or(u64::MAX));

        encoded_bytes = encoded_bytes
            .saturating_add(u64::try_from(tree_bytes.len() + root_bytes.len()).unwrap_or(u64::MAX));
        completed += 1;
        drop(decoded_root);
        drop(root);
        drop(tree);
        drop(root_bytes);
        drop(tree_bytes);
        retained_owners = retained_owners.max(owners.live());
        assert_eq!(owners.live(), 0);
        assert!(owners.peak() > 0);
    }
    assert!(peak_codec_scratch_bytes <= 262_144);

    println!(
        "PRC_CORE_TINY_JSON:{{\"schema_version\":1,\"case_id\":\"PRC-R08-core\",\"iterations\":{completed},\"encoded_bytes\":{encoded_bytes},\"peak_codec_scratch_bytes\":{peak_codec_scratch_bytes},\"retained_owners\":{retained_owners}}}"
    );
    Ok(())
}
