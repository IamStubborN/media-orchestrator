use media_core::OperationKey;

#[test]
fn operation_key_is_nominal_round_trips_bytes_and_redacts_debug_output() {
    let raw = [0xa5; 32];
    let key = OperationKey::from_bytes(raw);

    assert_eq!(key.as_bytes(), &raw);
    assert_eq!(key, OperationKey::from_bytes(raw));

    let debug = format!("{key:?}");
    assert_eq!(debug, "OperationKey([REDACTED])");
    assert!(!debug.contains("165"));
}
