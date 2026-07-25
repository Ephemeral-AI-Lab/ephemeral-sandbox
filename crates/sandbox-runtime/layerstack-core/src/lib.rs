#![forbid(unsafe_code)]

mod codec;
mod error;
mod identity;
mod path;
mod port;
mod root;
mod tree;
mod v3;

pub use codec::{
    decode_object_record, digest_preimage_header_len, encode_digest_preimage_header,
    encode_object_record, object_id, object_record_payload_len, ObjectRecord, MAX_RECORD_BYTES,
};
pub use error::{Error, ErrorKind, FieldClass};
pub use identity::{
    Capability, CapabilitySet, ChunkProfileId, Digest32, EntryKind, FormatVersion, HardlinkGroupId,
    ObjectId, ObjectKind, PublicationId, PublicationIdentity, RootId, TreeManifestId,
    ROOT_FORMAT_V2, ROOT_FORMAT_V3,
};
pub use path::CanonicalPath;
pub use port::{CanonicalSink, CanonicalSource, DigestDomain, RawDigest, TypedDigest};
pub use root::{
    decode_root_record, encode_root_record, root_id, root_record_payload_len, RootRecordV2,
};
pub use tree::{
    decode_tree_record, encode_tree_record, stage_tree_candidate, tree_entry_record_len,
    tree_record_payload_len, validate_tree_candidate, NodeMetadata, PendingTree, SparseExtent,
    TreeEntry, ValidatedTree, Xattr, XattrRef, MAX_COMPONENT_BYTES, MAX_ENTRY_METADATA_BYTES,
    MAX_PATH_BYTES, MAX_SYMLINK_TARGET_BYTES, MAX_TINY_ENTRIES, MAX_XATTR_KEY_BYTES,
};
pub use v3::{
    attribution_page_id, attribution_root_id, chunk_id, decode_v3_record, decode_v3_record_as,
    encode_v3_record, file_node_id, hardlink_group_id_v3, root_id_v3, segment_page_id,
    tree_page_id, v3_record_id, validate_v3_references, ActorId, AttributionPageId,
    AttributionRootId, BranchId, CanonicalRecordV3, ChunkId, FileNodeId, HardlinkGroupIdV3,
    LeaseId, PinId, RecordKindV3, SegmentPageId, TlvV3, TreePageId, V3ReferenceLookup,
    MAX_V3_RECORD_BYTES,
};
