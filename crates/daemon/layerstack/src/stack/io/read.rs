use crate::error::LayerStackError;
use crate::stack::LayerStack;

impl LayerStack {
    pub fn read_bytes(&self, path: &str) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        self.read_bytes_limited(path, usize::MAX)
    }

    pub fn read_bytes_limited(
        &self,
        path: &str,
        max_bytes: usize,
    ) -> Result<(Option<Vec<u8>>, bool), LayerStackError> {
        let _guard = self.writer_lock.shared()?;
        let manifest = self.read_active_manifest_unlocked()?;
        self.view.read_bytes_limited(path, &manifest, max_bytes)
    }

    pub fn read_text(&self, path: &str) -> Result<(String, bool), LayerStackError> {
        let (bytes, exists) = self.read_bytes(path)?;
        if !exists {
            return Ok((String::new(), false));
        }
        let bytes = bytes.unwrap_or_default();
        let text =
            String::from_utf8(bytes).map_err(|err| LayerStackError::Storage(err.to_string()))?;
        Ok((text, true))
    }
}
