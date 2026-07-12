/// Walk an anyhow error's context chain into an ordered array
/// `[outermost-context, …, root-cause]`, for structured `error_chain` fields.
pub fn error_chain_vec(e: &anyhow::Error) -> Vec<String> {
    e.chain().map(|c| c.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn chain_is_outer_to_root() {
        let root = anyhow::anyhow!("connection refused");
        let e = Err::<(), _>(root).context("opening socket").context("creating space").unwrap_err();
        let chain = error_chain_vec(&e);
        assert_eq!(chain.first().map(String::as_str), Some("creating space"));
        assert_eq!(chain.last().map(String::as_str), Some("connection refused"));
        assert_eq!(chain.len(), 3);
    }
}
