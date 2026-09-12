//! SHA-512 crypt, the `$6$` password hash every guest's shadow file understands.

use crate::error::{Error, Result};
use sha2::{Digest as _, Sha512};
use std::io::Read as _;

const ROUNDS_DEFAULT: u32 = 5000;
const ROUNDS_MIN: u32 = 1000;
const ROUNDS_MAX: u32 = 999_999_999;
const SALT_MAX: usize = 16;
const ALPHABET: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// The order in which the final digest's bytes are packed, as the specification lays it out.
const TRIPLES: [(usize, usize, usize); 21] = [
    (0, 21, 42),
    (22, 43, 1),
    (44, 2, 23),
    (3, 24, 45),
    (25, 46, 4),
    (47, 5, 26),
    (6, 27, 48),
    (28, 49, 7),
    (50, 8, 29),
    (9, 30, 51),
    (31, 52, 10),
    (53, 11, 32),
    (12, 33, 54),
    (34, 55, 13),
    (56, 14, 35),
    (15, 36, 57),
    (37, 58, 16),
    (59, 17, 38),
    (18, 39, 60),
    (40, 61, 19),
    (62, 20, 41),
];

/// Hashes a password under a fresh random salt.
pub fn hash(password: &str) -> Result<String> {
    check(password)?;
    Ok(sha512_crypt(password.as_bytes(), &salt()?, None))
}

/// Refuses what a person could not type at a login prompt.
pub fn check(password: &str) -> Result<()> {
    let refuse = |reason: &'static str| Err(Error::Password { reason });
    if password.is_empty() {
        return refuse("a password cannot be empty");
    }
    if password.len() > 1024 {
        return refuse("a password is at most 1024 bytes");
    }
    if password.chars().any(char::is_control) {
        return refuse("a password cannot hold control characters");
    }
    Ok(())
}

/// Whether text has the shape of a hash this module wrote, or of the disabled marker.
pub fn is_hash(text: &str) -> bool {
    text == "*"
        || text.strip_prefix("$6$").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|byte| ALPHABET.contains(&byte) || byte == b'$' || byte == b'=')
        })
}

/// Sixteen salt characters from the kernel's random source, letters and digits only.
fn salt() -> Result<Vec<u8>> {
    let mut source = std::fs::File::open("/dev/urandom").map_err(random_error)?;
    let mut salt = Vec::with_capacity(SALT_MAX);
    let mut buffer = [0u8; 64];
    while salt.len() < SALT_MAX {
        source.read_exact(&mut buffer).map_err(random_error)?;
        salt.extend(
            buffer
                .iter()
                .map(|byte| byte & 0x3f)
                .filter(|index| *index >= 2)
                .map(|index| ALPHABET[usize::from(index)])
                .take(SALT_MAX - salt.len()),
        );
    }
    Ok(salt)
}

fn random_error(source: std::io::Error) -> Error {
    Error::State {
        path: "/dev/urandom".into(),
        action: "read random bytes for a password salt",
        source,
    }
}

fn digest(parts: &[&[u8]]) -> [u8; 64] {
    let mut hasher = Sha512::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// A digest repeated to fill `length` bytes.
fn stretch(block: &[u8; 64], length: usize) -> Vec<u8> {
    block.iter().copied().cycle().take(length).collect()
}

/// The algorithm as specified by Ulrich Drepper, with `rounds` absent meaning the default.
fn sha512_crypt(password: &[u8], salt: &[u8], rounds: Option<u32>) -> String {
    let salt = &salt[..salt.len().min(SALT_MAX)];
    let count = rounds.map_or(ROUNDS_DEFAULT, |held| held.clamp(ROUNDS_MIN, ROUNDS_MAX));

    let alternate = digest(&[password, salt, password]);
    let mut hasher = Sha512::new();
    hasher.update(password);
    hasher.update(salt);
    hasher.update(stretch(&alternate, password.len()));
    let mut length = password.len();
    while length > 0 {
        if length & 1 == 1 {
            hasher.update(alternate);
        } else {
            hasher.update(password);
        }
        length >>= 1;
    }
    let mut current: [u8; 64] = hasher.finalize().into();

    let mut hasher = Sha512::new();
    for _ in 0..password.len() {
        hasher.update(password);
    }
    let password_block = stretch(&hasher.finalize().into(), password.len());

    let mut hasher = Sha512::new();
    for _ in 0..16 + usize::from(current[0]) {
        hasher.update(salt);
    }
    let salt_block = stretch(&hasher.finalize().into(), salt.len());

    for round in 0..count {
        let mut hasher = Sha512::new();
        if round & 1 == 1 {
            hasher.update(&password_block);
        } else {
            hasher.update(current);
        }
        if round % 3 != 0 {
            hasher.update(&salt_block);
        }
        if round % 7 != 0 {
            hasher.update(&password_block);
        }
        if round & 1 == 1 {
            hasher.update(current);
        } else {
            hasher.update(&password_block);
        }
        current = hasher.finalize().into();
    }

    let mut out = rounds.map_or_else(
        || String::from("$6$"),
        |held| format!("$6$rounds={}$", held.clamp(ROUNDS_MIN, ROUNDS_MAX)),
    );
    out.push_str(&String::from_utf8_lossy(salt));
    out.push('$');
    for (high, middle, low) in TRIPLES {
        encode(&mut out, current[high], current[middle], current[low], 4);
    }
    encode(&mut out, 0, 0, current[63], 2);
    out
}

fn encode(out: &mut String, high: u8, middle: u8, low: u8, characters: usize) {
    let mut word = (u32::from(high) << 16) | (u32::from(middle) << 8) | u32::from(low);
    for _ in 0..characters {
        out.push(char::from(ALPHABET[(word & 0x3f) as usize]));
        word >>= 6;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Values `openssl passwd -6` produces for the specification's inputs.
    #[test]
    fn the_specification_examples_are_reproduced() {
        assert_eq!(
            sha512_crypt(b"Hello world!", b"saltstring", None),
            "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1"
        );
        assert_eq!(
            sha512_crypt(b"Hello world!", b"saltstringsaltstring", Some(10000)),
            "$6$rounds=10000$saltstringsaltst$OW1/O6BYHV6BcXZu8QVeXbDWra3Oeqh0sbHbbMCVNSnCM/UrjmM0Dp8vOuZeHBy/YTBmSK6H9qs/y3RnOaw5v."
        );
        assert_eq!(
            sha512_crypt(
                b"we have a short salt string but not a short password",
                b"short",
                Some(77777)
            ),
            "$6$rounds=77777$short$WuQyW2YR.hBNpjjRhpYD/ifIw05xdfeEyQoMxIXbkvr0gge1a1x3yRULJ5CCaUeOxFmtlcGZelFl5CxtgfiAc0"
        );
        assert_eq!(
            sha512_crypt(b"a very much longer text to encrypt.  This one even stretches over morethan one line.", b"asaltof16chars..", Some(1400)),
            "$6$rounds=1400$asaltof16chars..$A04YeHZ50HBLMQH3ql.6UqosFpgQyaqQtUr5vzoZ0AaVe5VfbmQNEPphPGb/K49lN2OJ/0SlclayQg0bhzN0/."
        );
    }

    /// A rounds count below the minimum is raised to it.
    #[test]
    fn rounds_below_the_minimum_are_raised_to_it() {
        assert_eq!(
            sha512_crypt(
                b"the minimum number is still observed",
                b"roundstoolow",
                Some(10)
            ),
            "$6$rounds=1000$roundstoolow$kUMsbe306n21p9R.FRkW3IGn.S9NPN0x50YhH1xhLsPuWGsUSklZt58jaTfF4ZEQpyUNGc0dqbpBYYBaHHrsX."
        );
    }

    /// Checked against `openssl passwd -6`, which is how the guest's own tools would hash it.
    #[test]
    fn a_hash_agrees_with_the_system_tool_where_it_is_installed() {
        let Ok(output) = std::process::Command::new("openssl")
            .args(["passwd", "-6", "-salt", "AbCdEfGh12345678", "correct horse"])
            .output()
        else {
            return;
        };
        let expected = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            sha512_crypt(b"correct horse", b"AbCdEfGh12345678", None),
            expected.trim()
        );
    }

    #[test]
    fn a_fresh_hash_is_salted_differently_each_time() {
        let first = hash("secret").unwrap();
        let second = hash("secret").unwrap();
        assert_ne!(first, second);
        assert!(is_hash(&first), "{first}");
    }

    #[test]
    fn a_fresh_salt_is_sixteen_letters_and_digits() {
        let salt = salt().unwrap();
        assert_eq!(salt.len(), 16);
        assert!(salt.iter().all(u8::is_ascii_alphanumeric), "{salt:?}");
    }

    #[test]
    fn passwords_nobody_could_type_are_refused() {
        for password in ["", "line\nbreak", "tab\there", &"x".repeat(1025)] {
            let error = hash(password).unwrap_err();
            assert_eq!(error.kind(), "unusable-password", "{password:?}");
        }
    }

    #[test]
    fn only_hashes_and_the_disabled_marker_pass_as_hashes() {
        assert!(is_hash("*"));
        assert!(is_hash("$6$saltstring$svn8UoSVapNt"));
        for text in [
            "",
            "plain",
            "$6$",
            "$6$salt$has space",
            "$6$a'b",
            "$1$md5$hash",
        ] {
            assert!(!is_hash(text), "{text:?}");
        }
    }
}
