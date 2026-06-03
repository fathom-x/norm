//! ZEC <-> zatoshi conversion and formatting. Mirrors
//! `owallet_evm::format_amount` / `parse_amount` (which work in 6-decimal
//! USDC); ZEC has 8 decimals (1 ZEC = 100_000_000 zatoshi).

use crate::error::ZcashError;

/// Number of zatoshi in one ZEC.
pub const COIN: u64 = 100_000_000;

/// Convert a human ZEC amount to integer zatoshi. Rejects non-positive,
/// non-finite, or out-of-range values (mirrors the EVM `parse_amount` guard).
pub fn parse_zec_to_zat(amount_zec: f64) -> Result<u64, ZcashError> {
    if !amount_zec.is_finite() || amount_zec <= 0.0 {
        return Err(ZcashError::NonPositiveAmount);
    }
    // Round to the nearest zatoshi to avoid float truncation surprises
    // (e.g. 0.1 ZEC -> 10_000_000 zat exactly).
    let zat = (amount_zec * COIN as f64).round();
    if zat < 1.0 || zat >= u64::MAX as f64 {
        return Err(ZcashError::AmountOverflow);
    }
    Ok(zat as u64)
}

/// Format integer zatoshi as a decimal ZEC string, trimming trailing
/// fractional zeros (so `150_000_000` -> `"1.5"`, `100_000_000` -> `"1"`).
#[must_use]
pub fn format_zec(zat: u64) -> String {
    let whole = zat / COIN;
    let frac = zat % COIN;
    if frac == 0 {
        return whole.to_string();
    }
    // 8-digit fraction, trailing zeros trimmed.
    let frac_str = format!("{frac:08}");
    let trimmed = frac_str.trim_end_matches('0');
    format!("{whole}.{trimmed}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_simple_values() {
        assert_eq!(parse_zec_to_zat(1.0).unwrap(), 100_000_000);
        assert_eq!(parse_zec_to_zat(0.1).unwrap(), 10_000_000);
        assert_eq!(parse_zec_to_zat(0.00000001).unwrap(), 1);
    }

    #[test]
    fn parse_rejects_bad_amounts() {
        assert!(parse_zec_to_zat(0.0).is_err());
        assert!(parse_zec_to_zat(-1.0).is_err());
        assert!(parse_zec_to_zat(f64::NAN).is_err());
        assert!(parse_zec_to_zat(f64::INFINITY).is_err());
        // Below one zatoshi rounds to zero -> rejected.
        assert!(parse_zec_to_zat(0.000000004).is_err());
    }

    #[test]
    fn format_trims_trailing_zeros() {
        assert_eq!(format_zec(150_000_000), "1.5");
        assert_eq!(format_zec(100_000_000), "1");
        assert_eq!(format_zec(123), "0.00000123");
        assert_eq!(format_zec(0), "0");
        assert_eq!(format_zec(100_000_001), "1.00000001");
    }
}
