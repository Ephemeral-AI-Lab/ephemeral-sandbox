#![forbid(unsafe_code)]

mod codec;
mod error;
mod identity;
mod path;
mod port;
mod root;
mod tree;

pub use codec::{
    decode_object_record, digest_preimage_header_len, encode_digest_preimage_header,
    encode_object_record, object_id, object_record_payload_len, ObjectRecord, MAX_RECORD_BYTES,
};
pub use error::{Error, ErrorKind, FieldClass};
pub use identity::{
    Capability, CapabilitySet, ChunkProfileId, Digest32, EntryKind, FormatVersion, HardlinkGroupId,
    ObjectId, ObjectKind, PublicationId, PublicationIdentity, RootId, TreeManifestId,
    ROOT_FORMAT_V2,
};
pub use path::CanonicalPath;
pub use port::{CanonicalSink, CanonicalSource, DigestDomain, TypedDigest};
pub use root::{
    decode_root_record, encode_root_record, root_id, root_record_payload_len, RootRecordV2,
};
pub use tree::{
    decode_tree_record, encode_tree_record, stage_tree_candidate, tree_entry_record_len,
    tree_record_payload_len, validate_tree_candidate, NodeMetadata, PendingTree, SparseExtent,
    TreeEntry, ValidatedTree, Xattr, XattrRef, MAX_COMPONENT_BYTES, MAX_ENTRY_METADATA_BYTES,
    MAX_PATH_BYTES, MAX_SYMLINK_TARGET_BYTES, MAX_TINY_ENTRIES, MAX_XATTR_KEY_BYTES,
};
