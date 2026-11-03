//! Device tokens' shape (`flotilla_core::devices` makes and checks them).

/// What a device token starts with (so `util::redact` can find one anywhere).
pub const TOKEN_PREFIX: &str = "fdt_";
const ID_LEN: usize = 16;
const SECRET_LEN: usize = 43;

/// The device id in a token, if it has the shape of one.
pub fn token_device(token: &str) -> Option<&str> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    let id_ok = id.len() == ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    let secret_ok = secret.len() == SECRET_LEN
        && secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    (id_ok && secret_ok).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_names_its_device_only_when_its_shape_is_right() {
        let secret = "A".repeat(43);
        assert_eq!(
            token_device(&format!("fdt_0123456789abcdef_{secret}")),
            Some("0123456789abcdef")
        );
        assert_eq!(
            token_device(&format!("fdt_0123456789ABCDEF_{secret}")),
            None
        );
        assert_eq!(token_device(&format!("fdt_0123456789abcde_{secret}")), None);
        assert_eq!(
            token_device(&format!("fdt_0123456789abcdef_{}", &secret[1..])),
            None
        );
        assert_eq!(
            token_device(&format!("fdt_0123456789abcdef_{}!", &secret[1..])),
            None
        );
        assert_eq!(
            token_device(&format!("boat_0123456789abcdef_{secret}")),
            None
        );
    }
}
