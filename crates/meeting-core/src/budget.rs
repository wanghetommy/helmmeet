//! Independent wire-size and model-token budgets. Token counts are estimates,
//! not a tokenizer claim; the provider's usage remains authoritative.
pub const REQUEST_BYTES: usize = 100 * 1024;
pub const SOFT_BYTES: usize = 80 * 1024;
/// Absolute wire-size guard. This is deliberately independent from the model
/// context window: it protects the HTTP/JSON layer from an accidentally huge
/// request even when a provider advertises a large token window.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
pub const MIN_DYNAMIC_REQUEST_BYTES: usize = 128 * 1024;
pub const ESTIMATED_BYTES_PER_TOKEN: usize = 4;
pub fn input_tokens(context: u32, output: u32) -> Option<usize> {
    context
        .checked_sub(output)?
        .checked_sub(1024.max(context.div_ceil(10)))
        .filter(|n| *n >= 512)
        .map(|n| n as usize)
}
/// Derive a wire budget from the model's usable input tokens. The result is a
/// safety budget, not a promise that every provider accepts this many bytes.
/// Provider usage and HTTP errors remain authoritative.
pub fn request_bytes(context: u32, output: u32) -> Option<usize> {
    let tokens = input_tokens(context, output)?;
    Some(
        tokens
            .saturating_mul(ESTIMATED_BYTES_PER_TOKEN)
            .clamp(MIN_DYNAMIC_REQUEST_BYTES, MAX_REQUEST_BYTES),
    )
}
pub fn soft_request_bytes(hard: usize) -> usize {
    hard.saturating_mul(3) / 4
}
pub fn estimate(text: &str) -> usize {
    // ASCII identifiers/JSON and non-ASCII text have different densities. Use
    // a deliberately cautious generic estimate rather than UTF-8 bytes = tokens.
    let mut quarters = 0usize;
    for c in text.chars() {
        quarters += if c.is_ascii() {
            if c.is_ascii_whitespace() {
                1
            } else {
                2
            }
        } else if matches!(c as u32, 0x3400..=0x9fff | 0xf900..=0xfaff) {
            5
        } else {
            8
        };
    }
    quarters.div_ceil(4) + 32
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn budgets_have_different_units_and_unicode_is_not_three_tokens_per_character() {
        assert_eq!(REQUEST_BYTES, 102400);
        assert_eq!(input_tokens(32768, 4096), Some(25395));
        let text = "会议目标".repeat(5000);
        assert!(text.len() < REQUEST_BYTES);
        assert!(estimate(&text) < text.len());
        assert!(estimate(&text) < 25395);
        assert!(input_tokens(1024, 512).is_none());
        assert!(estimate(&"a".repeat(100000)) > 25395);
        assert_eq!(request_bytes(32768, 4096), Some(128 * 1024));
        assert_eq!(request_bytes(524288, 131072), Some(1_363_148));
        assert_eq!(soft_request_bytes(1_000_000), 750_000);
    }
}
