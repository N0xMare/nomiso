//! Tact v1 capacity constants (global corpus).

use serde::{Deserialize, Serialize};

/// Production limits matching Tact memory.md (calibratable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TactLimits {
    /// Max UTF-8 content bytes per record.
    pub max_content_bytes: usize,
    /// Max live rows.
    pub max_rows: usize,
    /// Max total content bytes across rows.
    pub max_total_content_bytes: usize,
    /// Max scan candidates.
    pub max_scan_results: u32,
    /// Max scan query bytes.
    pub max_query_bytes: usize,
    /// Preview UTF-8 byte cap.
    pub max_preview_bytes: usize,
    /// Probation duration (seconds).
    pub probation_secs: i64,
}

impl Default for TactLimits {
    fn default() -> Self {
        Self {
            max_content_bytes: 1024,
            max_rows: 512,
            max_total_content_bytes: 256 * 1024,
            max_scan_results: 5,
            max_query_bytes: 512,
            max_preview_bytes: 64,
            probation_secs: 7 * 24 * 3600,
        }
    }
}

impl TactLimits {
    /// Reject zero/negative knobs that would panic (`clamp(1, 0)`) or invert policy.
    pub fn validate(&self) -> Result<(), String> {
        if self.max_content_bytes < 1 {
            return Err("max_content_bytes must be >= 1".into());
        }
        if self.max_rows < 1 {
            return Err("max_rows must be >= 1".into());
        }
        if self.max_total_content_bytes < 1 {
            return Err("max_total_content_bytes must be >= 1".into());
        }
        if self.max_scan_results < 1 {
            return Err("max_scan_results must be >= 1".into());
        }
        if self.max_query_bytes < 1 {
            return Err("max_query_bytes must be >= 1".into());
        }
        if self.max_preview_bytes < 1 {
            return Err("max_preview_bytes must be >= 1".into());
        }
        if self.probation_secs < 0 {
            return Err("probation_secs must be >= 0".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limits_ok() {
        assert!(TactLimits::default().validate().is_ok());
    }

    #[test]
    fn zeros_and_negative_rejected() {
        assert!(TactLimits {
            max_content_bytes: 0,
            ..TactLimits::default()
        }
        .validate()
        .is_err());
        assert!(TactLimits {
            max_rows: 0,
            ..TactLimits::default()
        }
        .validate()
        .is_err());
        assert!(TactLimits {
            max_total_content_bytes: 0,
            ..TactLimits::default()
        }
        .validate()
        .is_err());
        assert!(TactLimits {
            max_scan_results: 0,
            ..TactLimits::default()
        }
        .validate()
        .is_err());
        assert!(TactLimits {
            max_query_bytes: 0,
            ..TactLimits::default()
        }
        .validate()
        .is_err());
        assert!(TactLimits {
            max_preview_bytes: 0,
            ..TactLimits::default()
        }
        .validate()
        .is_err());
        assert!(TactLimits {
            probation_secs: -1,
            ..TactLimits::default()
        }
        .validate()
        .is_err());
    }
}
