//! Bytes written as hexadecimal, such as a digest in a name or file.

/// `bytes` as lowercase hexadecimal, two digits a byte.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_each_byte_as_two_lowercase_digits() {
        assert_eq!(super::encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
