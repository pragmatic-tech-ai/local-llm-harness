//! Logprob computation from raw logits (no FFI; vocab lookup is a closure).

use super::types::TokenInfo;

/// How many top alternatives to report per token (parity with the HTTP engine).
pub const TOP_LOGPROBS: usize = 5;

/// Builds the `TokenInfo` for `sampled_id` from raw `logits`. An out-of-range
/// `sampled_id` yields the `f32::MIN` ("n/a") sentinel logprob.
pub fn token_info(
    logits: &[f32],
    sampled_id: usize,
    piece_of: impl Fn(usize) -> String,
    top_n: usize,
) -> TokenInfo {
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let log_sum = logits.iter().map(|&l| (l - max).exp()).sum::<f32>().ln();
    let logprob_at = |id: usize| logits[id] - max - log_sum;

    let logprob = if sampled_id < logits.len() {
        logprob_at(sampled_id)
    } else {
        f32::MIN
    };

    let mut ids: Vec<usize> = (0..logits.len()).collect();
    ids.sort_by(|&a, &b| logits[b].total_cmp(&logits[a]));
    ids.truncate(top_n);
    let top = ids
        .into_iter()
        .map(|id| (piece_of(id), logprob_at(id)))
        .collect();

    TokenInfo {
        token: piece_of(sampled_id),
        logprob,
        top,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(id: usize) -> String {
        format!("t{id}")
    }

    #[test]
    fn log_softmax_of_known_logits() {
        let info = token_info(
            &[0.0, std::f32::consts::LN_2, 2.0 * std::f32::consts::LN_2],
            2,
            piece,
            3,
        );
        assert!((info.logprob - (-0.5596158)).abs() < 1e-5);
    }

    #[test]
    fn top_n_sorted_descending_and_capped() {
        let logits: Vec<f32> = (0..10).map(|i| ((i * 7) % 10) as f32 * 0.3).collect();
        let info = token_info(&logits, 0, piece, 5);
        assert_eq!(info.top.len(), 5);
        assert!(info.top.windows(2).all(|w| w[0].1 > w[1].1));
        assert!(info.top.iter().all(|(_, lp)| *lp <= 0.0));
    }

    #[test]
    fn sampled_token_piece_is_returned() {
        let info = token_info(&[1.0, 2.0, 3.0], 1, piece, 2);
        assert_eq!(info.token, "t1");
    }

    #[test]
    fn top_n_bounds_are_safe() {
        let logits = [1.0, 2.0, 3.0];
        assert_eq!(token_info(&logits, 0, piece, 100).top.len(), 3);
        assert!(token_info(&logits, 0, piece, 0).top.is_empty());
    }
}
