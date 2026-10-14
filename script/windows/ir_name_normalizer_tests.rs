use super::*;

#[test]
fn strips_legacy_hashes_and_generic_values() {
    assert_eq!(
        normalized_name("_ZN4demo3run17h0123456789abcdefE"),
        Some("demo::run".to_owned())
    );
    assert_eq!(
        normalized_name("_ZN4demo19work$LT$u8$C$32$GT$17h0123456789abcdefE"),
        Some("demo::work<_>".to_owned())
    );
}

#[test]
fn omits_non_rust_symbols_and_unsafe_names() {
    assert_eq!(normalized_name("PRIVATE_FIXTURE_LITERAL"), None);
    assert_eq!(normalized_name("llvm.memcpy"), None);
    assert_eq!(normalized_name(&"x".repeat(65_537)), None);
}
