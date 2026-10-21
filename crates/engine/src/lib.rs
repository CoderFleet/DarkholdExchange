//! Pure matching engine: no I/O, no async, no clock, no randomness.
pub fn crate_name() -> &'static str {
    "dx-engine"
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() {
        assert_eq!(super::crate_name(), "dx-engine");
    }
}
