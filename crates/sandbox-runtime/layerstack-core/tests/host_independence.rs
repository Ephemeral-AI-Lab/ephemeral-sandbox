mod common;

use sandbox_runtime_layerstack_core::{
    decode_root_record, CanonicalPath, Error, ErrorKind, FieldClass, PublicationId,
    PublicationIdentity, RootId, RootRecordV2, TreeEntry, Xattr, ROOT_FORMAT_V2,
};

use common::{
    encode_root, encode_tree, file_segments, identify_root, metadata, sample_root,
    validate_tree_bytes, BytesSource, CaptureDigest,
};

#[derive(Clone, Copy)]
struct PhysicalCarrier<'a> {
    host_path: &'a str,
    inode: u64,
    provider: &'a str,
    materialization: &'a str,
}

fn regular(path: &[u8], object_byte: u8) -> Result<TreeEntry, Error> {
    TreeEntry::regular(
        CanonicalPath::from_bytes(path)?,
        metadata(0o644, Vec::new())?,
        0x0102_0304_0506_0708,
        Vec::new(),
        file_segments(object_byte),
        None,
    )
}

fn identify(record: &RootRecordV2) -> Result<RootId, Error> {
    identify_root(record, &mut CaptureDigest::default())
}

#[test]
fn fixed_width_big_endian_and_raw_bytes_are_host_independent() -> Result<(), Error> {
    let entry = regular(&[b'r', b'a', b'w', b'/', 0xff, b'\\', b'x'], 0x31)?;
    let tree_bytes = encode_tree(std::slice::from_ref(&entry))?;

    assert_eq!(&tree_bytes[..8], b"EOS-LS2\0");
    assert_eq!(tree_bytes[8], 0x11);
    assert_eq!(&tree_bytes[9..11], &2_u16.to_be_bytes());
    assert_eq!(
        &tree_bytes[11..19],
        &(u64::try_from(tree_bytes.len()).unwrap_or(u64::MAX) - 19).to_be_bytes(),
    );
    assert!(tree_bytes
        .windows(entry.path().as_bytes().len())
        .any(|window| window == entry.path().as_bytes()));
    assert!(tree_bytes
        .windows(8)
        .any(|window| window == 0x0102_0304_0506_0708_u64.to_be_bytes()));

    let validated = validate_tree_bytes(&tree_bytes, &mut CaptureDigest::default())?;
    let publication =
        PublicationIdentity::new(0x0102_0304_0506_0708, PublicationId::new([0xa5; 16])?);
    let record = RootRecordV2::new(
        &validated,
        sandbox_runtime_layerstack_core::ChunkProfileId::SEQ_CDC_V1,
        Some(RootId::new(sandbox_runtime_layerstack_core::Digest32::new(
            [0x41; 32],
        ))),
        Some(RootId::new(sandbox_runtime_layerstack_core::Digest32::new(
            [0x42; 32],
        ))),
        publication,
    );
    let root_bytes = encode_root(&record)?;
    assert!(root_bytes
        .windows(8)
        .any(|window| window == publication.generation().to_be_bytes()));
    let mut source = BytesSource::fragmented(&root_bytes, 1);
    assert_eq!(decode_root_record(&mut source, &validated)?, record);
    Ok(())
}

#[test]
fn identity_matrix_includes_only_logical_fields() -> Result<(), Error> {
    let plain_entries = vec![regular(b"file", 0x10)?];
    let xattr_entries = vec![TreeEntry::regular(
        CanonicalPath::from_bytes(b"file")?,
        metadata(
            0o644,
            vec![Xattr::new(b"user.portable".to_vec(), b"yes".to_vec())?],
        )?,
        0x0102_0304_0506_0708,
        Vec::new(),
        file_segments(0x10),
        None,
    )?];
    let changed_entries = vec![regular(b"file", 0x11)?];

    let plain = validate_tree_bytes(&encode_tree(&plain_entries)?, &mut CaptureDigest::default())?;
    let with_capability =
        validate_tree_bytes(&encode_tree(&xattr_entries)?, &mut CaptureDigest::default())?;
    let changed_tree = validate_tree_bytes(
        &encode_tree(&changed_entries)?,
        &mut CaptureDigest::default(),
    )?;

    let baseline = sample_root(&plain, 7, 0x70, None, None)?;
    let baseline_id = identify(&baseline)?;
    let variants = [
        RootRecordV2::new(
            &changed_tree,
            baseline.chunk_profile(),
            baseline.parent(),
            baseline.base(),
            baseline.publication(),
        ),
        RootRecordV2::new(
            &with_capability,
            baseline.chunk_profile(),
            baseline.parent(),
            baseline.base(),
            baseline.publication(),
        ),
        sample_root(
            &plain,
            7,
            0x70,
            Some(RootId::new(sandbox_runtime_layerstack_core::Digest32::new(
                [1; 32],
            ))),
            None,
        )?,
        sample_root(
            &plain,
            7,
            0x70,
            None,
            Some(RootId::new(sandbox_runtime_layerstack_core::Digest32::new(
                [2; 32],
            ))),
        )?,
        sample_root(&plain, 8, 0x70, None, None)?,
        sample_root(&plain, 7, 0x71, None, None)?,
    ];
    for variant in &variants {
        assert_ne!(identify(variant)?, baseline_id);
    }

    let first_carrier = PhysicalCarrier {
        host_path: "/var/lib/docker/overlay2/a",
        inode: 1,
        provider: "docker",
        materialization: "overlayfs",
    };
    let second_carrier = PhysicalCarrier {
        host_path: r"C:\containerd\b",
        inode: u64::MAX,
        provider: "containerd",
        materialization: "fuse",
    };
    assert_ne!(first_carrier.host_path, second_carrier.host_path);
    assert_ne!(first_carrier.inode, second_carrier.inode);
    assert_ne!(first_carrier.provider, second_carrier.provider);
    assert_ne!(
        first_carrier.materialization,
        second_carrier.materialization
    );
    let equivalent = sample_root(&plain, 7, 0x70, None, None)?;
    assert_eq!(encode_root(&baseline)?, encode_root(&equivalent)?);
    assert_eq!(identify(&equivalent)?, baseline_id);

    let mut invalid_profile = encode_root(&baseline)?;
    let profile_marker = [2_u8, 0, 0, 0, 2, 0, 1];
    let offset = invalid_profile
        .windows(profile_marker.len())
        .position(|window| window == profile_marker)
        .ok_or_else(|| Error::new(ErrorKind::Malformed, ROOT_FORMAT_V2, FieldClass::Profile, 0))?;
    invalid_profile[offset + 5..offset + 7].copy_from_slice(&2_u16.to_be_bytes());
    let mut invalid_profile_source = BytesSource::new(&invalid_profile);
    assert!(decode_root_record(&mut invalid_profile_source, &plain).is_err());
    Ok(())
}
