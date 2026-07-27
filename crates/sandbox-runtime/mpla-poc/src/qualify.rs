use crate::{PocConfig, PocError, PocResult, QualificationReceipt, QualificationRequest};

pub fn qualify(
    _config: &PocConfig,
    _request: &QualificationRequest,
) -> PocResult<QualificationReceipt> {
    Err(PocError::Unsupported(
        "real qualification implementation is assigned to M0 Worker A".to_owned(),
    ))
}
