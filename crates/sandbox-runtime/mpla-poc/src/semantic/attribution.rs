use sha2::{Digest, Sha256};

use crate::AttributionInput;

pub fn descriptor_sha256(input: &AttributionInput) -> String {
    let mut digest = Sha256::new();
    digest.update(b"mpla-poc-semantic-v1/attribution-descriptor\0");
    update_bytes(&mut digest, input.actor_id.as_bytes());
    update_bytes(&mut digest, input.semantic_operation_id.as_bytes());
    super::hex_digest(digest.finalize().into())
}

pub fn leaf_digest(record_digest: [u8; 32], input: &AttributionInput) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"mpla-poc-semantic-v1/attribution-leaf\0");
    digest.update(record_digest);
    update_bytes(&mut digest, input.actor_id.as_bytes());
    update_bytes(&mut digest, input.semantic_operation_id.as_bytes());
    digest.finalize().into()
}

fn update_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}
