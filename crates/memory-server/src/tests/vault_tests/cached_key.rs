use super::*;

#[test]
fn cached_vault_key_copies_source_buffer() {
    let mut source = [7u8; 32];
    let cached = CachedVaultKey::copy_from(&source);
    vault_crypto::zero_key(&mut source);

    assert_eq!(source, [0u8; 32]);
    assert_eq!(cached.bytes(), &[7u8; 32]);
}
