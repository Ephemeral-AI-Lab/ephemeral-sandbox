use super::super::generation::{GenerationSelection, MaterializationKey};
use super::super::materialization::MaterializationError;
use super::super::materialization_publication::publication_subject;

/// Validate the immutable result of the private streamed reconstruction.
///
/// The native backend performs the actual bounded flatten/reconstruction and
/// content verification. This producer boundary additionally proves that
/// squash changed only the physical generation: both logical identities remain
/// the exact typed identities selected by the caller.
pub(super) fn validate_identity_preservation(
    key: &MaterializationKey,
    prior: &GenerationSelection,
    ready: &GenerationSelection,
) -> Result<(), MaterializationError> {
    let identities_preserved = prior.manifest.root_id == ready.manifest.root_id
        && prior.manifest.attribution_root_id == ready.manifest.attribution_root_id
        && ready.manifest.root_id == format!("sha256:{}", hex(key.root.digest().as_bytes()))
        && ready.manifest.attribution_root_id
            == format!("sha256:{}", hex(key.attribution_root.digest().as_bytes()));
    if !identities_preserved
        || prior.manifest.generation >= ready.manifest.generation
        || publication_subject(prior) == publication_subject(ready)
    {
        return Err(MaterializationError::Generation(
            "private squash changed RootId/AttributionRootId or reused its generation".to_owned(),
        ));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}
