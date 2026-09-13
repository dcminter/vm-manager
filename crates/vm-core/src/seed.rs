//! The cloud-init `NoCloud` seed: a vfat volume labelled `cidata`.

use crate::error::{Error, Result};
use crate::fat;
use crate::value::{Value, to_yaml};
use std::fs;
use std::path::Path;

/// The volume label cloud-init's `NoCloud` source looks for.
pub const LABEL: &str = "cidata";

/// The preferred shell, corrected on images without it.
const SHELL: &str = "/bin/bash";

/// A share to mount in the guest, named by its virtiofs tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub tag: String,
    pub target: String,
}

/// Everything the guest is told about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    /// cloud-init re-runs per-instance modules when this changes.
    pub instance_id: String,
    pub hostname: String,
    /// The account the key is installed for, and the one `vm ssh` connects as.
    pub user: String,
    pub authorized_key: String,
    pub mounts: Vec<Mount>,
    /// A `$6$` hash, `*` to remove the password, or `None` for key-only access.
    pub password: Option<String>,
}

impl Seed {
    pub fn meta_data(&self) -> String {
        to_yaml(&Value::map([
            ("instance-id", Value::string(&self.instance_id)),
            ("local-hostname", Value::string(&self.hostname)),
        ]))
    }

    /// Guest fixes as shell lines, since FreeBSD's nuageinit takes no argument lists.
    fn corrections(&self) -> Value {
        let user = &self.user;
        Value::list([
            // ssh cannot exec a missing shell.
            Value::string(format!(
                "test -x {SHELL} || usermod -s /bin/sh {user} || \
                 pw usermod {user} -s /bin/sh || chsh -s /bin/sh {user} || true"
            )),
            // Images with doas instead of sudo need their own rule.
            Value::string(format!(
                "command -v sudo >/dev/null || {{ command -v doas >/dev/null && \
                 echo permit nopass {user} >> /etc/doas.conf; }} || true"
            )),
        ])
    }

    /// The cloud-config document.
    pub fn user_data(&self) -> String {
        let mut fields = vec![
            ("hostname", Value::string(&self.hostname)),
            ("preserve_hostname", Value::Bool(false)),
            (
                "users",
                Value::list([Value::map([
                    ("name", Value::string(&self.user)),
                    ("shell", Value::string(SHELL)),
                    // `*` rather than a lock, which OpenSSH without PAM treats as refusing keys too.
                    (
                        "passwd",
                        Value::string(self.password.as_deref().unwrap_or("*")),
                    ),
                    ("lock_passwd", Value::Bool(false)),
                    ("sudo", Value::string("ALL=(ALL) NOPASSWD:ALL")),
                    (
                        "ssh_authorized_keys",
                        Value::list([Value::string(self.authorized_key.trim())]),
                    ),
                ])]),
            ),
            ("ssh_pwauth", Value::Bool(false)),
            ("disable_root", Value::Bool(true)),
            ("runcmd", self.corrections()),
        ];
        // The users module leaves an existing password alone, so chpasswd sets it.
        if let Some(password) = &self.password {
            fields.push((
                "chpasswd",
                Value::map([
                    ("expire", Value::Bool(false)),
                    (
                        "users",
                        Value::list([Value::map([
                            ("name", Value::string(&self.user)),
                            ("password", Value::string(password)),
                            ("type", Value::string("hash")),
                        ])]),
                    ),
                ]),
            ));
        }
        if !self.mounts.is_empty() {
            // Mounted each boot, since an fstab entry would outlive the share.
            fields.push((
                "bootcmd",
                Value::list(self.mounts.iter().flat_map(|mount| {
                    [
                        Value::list([
                            Value::string("mkdir"),
                            Value::string("-p"),
                            Value::string(&mount.target),
                        ]),
                        Value::list([
                            Value::string("mount"),
                            Value::string("-t"),
                            Value::string("virtiofs"),
                            Value::string(&mount.tag),
                            Value::string(&mount.target),
                        ]),
                    ]
                })),
            ));
        }
        format!("#cloud-config\n{}", to_yaml(&Value::map(fields)))
    }

    /// Builds the seed image deterministically.
    pub fn image(&self) -> Result<Vec<u8>> {
        self.check()?;
        let user_data = self.user_data();
        let meta_data = self.meta_data();
        fat::image(
            LABEL,
            &[
                fat::File {
                    name: "user-data",
                    contents: user_data.as_bytes(),
                },
                fat::File {
                    name: "meta-data",
                    contents: meta_data.as_bytes(),
                },
            ],
        )
        .map_err(|refused| Error::SeedRefused {
            reason: refused.to_string(),
        })
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let image = self.image()?;
        fs::write(path, image).map_err(|source| Error::SeedWrite {
            path: path.to_owned(),
            source,
        })
    }

    /// Rejects values that would produce a broken cloud-config.
    fn check(&self) -> Result<()> {
        let refuse = |reason: String| Err(Error::SeedRefused { reason });
        if !is_hostname(&self.hostname) {
            return refuse(format!("'{}' is not a usable hostname", self.hostname));
        }
        if !is_username(&self.user) {
            return refuse(format!("'{}' is not a usable user name", self.user));
        }
        if self.instance_id.is_empty() || !is_plain(&self.instance_id) {
            return refuse(format!(
                "'{}' is not a usable instance id",
                self.instance_id
            ));
        }
        if self
            .password
            .as_deref()
            .is_some_and(|password| !crate::crypt::is_hash(password))
        {
            return refuse("the password is not a SHA-512 crypt hash".to_owned());
        }
        if !is_public_key(&self.authorized_key) {
            return refuse("the authorized key is not an OpenSSH public key".to_owned());
        }
        for mount in &self.mounts {
            if mount.tag.is_empty() || !is_plain(&mount.tag) {
                return refuse(format!("'{}' is not a usable virtiofs tag", mount.tag));
            }
            if !mount.target.starts_with('/') || mount.target.contains('\n') {
                return refuse(format!("'{}' is not an absolute mount point", mount.target));
            }
        }
        Ok(())
    }
}

fn is_hostname(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 63
        && !text.starts_with('-')
        && !text.ends_with('-')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

pub fn is_username(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text.starts_with(|character: char| character.is_ascii_lowercase() || character == '_')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte))
}

fn is_plain(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// A shape check that catches a path given in place of a key.
fn is_public_key(text: &str) -> bool {
    let text = text.trim();
    let mut parts = text.split_whitespace();
    let (Some(algorithm), Some(material)) = (parts.next(), parts.next()) else {
        return false;
    };
    !text.contains('\n')
        && (algorithm.starts_with("ssh-") || algorithm.starts_with("ecdsa-"))
        && material.len() > 16
        && material
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQFakeKeyMaterialForTestsOnly vm";

    fn seed() -> Seed {
        Seed {
            instance_id: "vm-0001".to_owned(),
            hostname: "demo".to_owned(),
            user: "vm".to_owned(),
            authorized_key: KEY.to_owned(),
            mounts: Vec::new(),
            password: None,
        }
    }

    #[test]
    fn the_meta_data_names_the_instance_and_the_host() {
        let text = seed().meta_data();
        assert_eq!(text, "instance-id: vm-0001\nlocal-hostname: demo\n");
    }

    #[test]
    fn the_user_data_opens_with_the_cloud_config_marker() {
        let text = seed().user_data();
        assert!(
            text.starts_with("#cloud-config\n"),
            "cloud-init ignores anything else: {text}"
        );
    }

    #[test]
    fn the_key_reaches_the_named_user() {
        let text = seed().user_data();
        assert!(text.contains("- name: vm"), "{text}");
        assert!(text.contains(KEY), "{text}");
    }

    #[test]
    fn password_login_is_disabled() {
        let text = seed().user_data();
        assert!(text.contains("ssh_pwauth: false"), "{text}");
        assert!(text.contains("disable_root: true"), "{text}");
        assert!(text.contains("passwd:"), "{text}");
        assert!(text.contains('*'), "{text}");
    }

    #[test]
    fn the_account_is_not_locked() {
        assert!(
            !seed().user_data().contains("lock_passwd: true"),
            "a locked account cannot be reached on a guest without PAM"
        );
    }

    #[test]
    fn a_shell_the_image_lacks_is_corrected() {
        let text = seed().user_data();
        assert!(text.contains("runcmd:"), "{text}");
        assert!(text.contains("test -x /bin/bash"), "{text}");
        assert!(text.contains("usermod -s /bin/sh vm"), "{text}");
        // FreeBSD has no usermod.
        assert!(text.contains("pw usermod vm -s /bin/sh"), "{text}");
    }

    #[test]
    fn no_mounts_means_no_mounts_key() {
        assert!(!seed().user_data().contains("mounts"));
    }

    #[test]
    fn a_mount_becomes_a_virtiofs_mount() {
        let mut seed = seed();
        seed.mounts.push(Mount {
            tag: "work".to_owned(),
            target: "/mnt/work".to_owned(),
        });
        let text = seed.user_data();
        assert!(text.contains("bootcmd:"), "{text}");
        assert!(text.contains("- work"), "{text}");
        assert!(text.contains("- /mnt/work"), "{text}");
        assert!(text.contains("- virtiofs"), "{text}");
        assert!(text.contains("- mkdir"), "{text}");
    }

    #[test]
    fn a_mount_leaves_nothing_behind_in_the_guest() {
        let mut seed = seed();
        seed.mounts.push(Mount {
            tag: "work".to_owned(),
            target: "/mnt/work".to_owned(),
        });
        assert!(
            !seed.user_data().contains("mounts:"),
            "{}",
            seed.user_data()
        );
    }

    #[test]
    fn the_image_is_a_fat_volume_labelled_cidata() {
        let image = seed().image().unwrap();
        assert_eq!(image.len(), fat::image_size());
        assert_eq!(&image[43..54], b"CIDATA     ");
    }

    #[test]
    fn the_image_carries_both_documents() {
        let seed = seed();
        let image = seed.image().unwrap();
        let haystack = String::from_utf8_lossy(&image);
        assert!(haystack.contains("#cloud-config"), "user-data is missing");
        assert!(
            haystack.contains("instance-id: vm-0001"),
            "meta-data is missing"
        );
    }

    #[test]
    fn the_same_seed_always_produces_the_same_image() {
        assert_eq!(seed().image().unwrap(), seed().image().unwrap());
    }

    #[test]
    fn a_written_seed_is_the_image() {
        let mut path = std::env::temp_dir();
        path.push(format!("vm-seed-{}.img", std::process::id()));
        seed().write(&path).unwrap();
        let written = fs::read(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(written, seed().image().unwrap());
    }

    #[test]
    fn an_unwritable_path_is_reported_as_such() {
        let error = seed()
            .write(Path::new("/nonexistent/directory/seed.img"))
            .unwrap_err();
        assert_eq!(error.kind(), "seed-unwritable");
    }

    #[test]
    fn a_bad_hostname_is_refused() {
        for hostname in [
            "",
            "-leading",
            "trailing-",
            "has space",
            "has.dot",
            &"x".repeat(64),
        ] {
            let mut seed = seed();
            seed.hostname = hostname.to_owned();
            let error = seed.image().unwrap_err();
            assert_eq!(error.kind(), "seed-invalid", "{hostname} should be refused");
        }
    }

    #[test]
    fn a_bad_user_name_is_refused() {
        for user in ["", "Capitals", "1leading", "has space", &"x".repeat(33)] {
            let mut seed = seed();
            seed.user = user.to_owned();
            assert!(seed.image().is_err(), "{user} should be refused");
        }
    }

    #[test]
    fn an_instance_id_with_yaml_in_it_is_refused() {
        let mut seed = seed();
        seed.instance_id = "one\nlocal-hostname: elsewhere".to_owned();
        assert_eq!(seed.image().unwrap_err().kind(), "seed-invalid");
    }

    #[test]
    fn something_that_is_not_a_public_key_is_refused() {
        for key in [
            "",
            "/home/someone/.ssh/id_ed25519.pub",
            "ssh-ed25519",
            "ssh-ed25519 short",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
        ] {
            let mut seed = seed();
            seed.authorized_key = key.to_owned();
            assert!(seed.image().is_err(), "{key} should be refused");
        }
    }

    #[test]
    fn ordinary_key_types_are_accepted() {
        for algorithm in ["ssh-ed25519", "ssh-rsa", "ecdsa-sha2-nistp256"] {
            let mut seed = seed();
            seed.authorized_key = format!("{algorithm} AAAAC3NzaC1lZDI1NTE5AAAAIKq7YQ== vm");
            assert!(seed.image().is_ok(), "{algorithm} should be accepted");
        }
    }

    #[test]
    fn a_relative_mount_point_is_refused() {
        let mut seed = seed();
        seed.mounts.push(Mount {
            tag: "work".to_owned(),
            target: "mnt/work".to_owned(),
        });
        assert_eq!(seed.image().unwrap_err().kind(), "seed-invalid");
    }

    #[test]
    fn a_tag_that_would_break_the_document_is_refused() {
        let mut seed = seed();
        seed.mounts.push(Mount {
            tag: "work: elsewhere".to_owned(),
            target: "/mnt/work".to_owned(),
        });
        assert!(seed.image().is_err());
    }

    const HASH: &str = "$6$saltstring$svn8UoSVapNtMuq1ukKS4tPQd8iKwSMHWjl/O817G3uBnIFNjnQJuesI68u4OTLiBFdcbYEdFCoEOfaS35inz1";

    #[test]
    fn without_a_password_the_account_has_none_and_nothing_is_changed() {
        let text = seed().user_data();
        assert!(text.contains("passwd: \"*\""), "{text}");
        assert!(!text.contains("chpasswd"), "{text}");
    }

    #[test]
    fn a_password_reaches_both_the_new_account_and_an_existing_one() {
        let mut held = seed();
        held.password = Some(HASH.to_owned());
        let text = held.user_data();
        assert!(text.contains(&format!("passwd: {HASH}")), "{text}");
        assert!(text.contains("chpasswd:"), "{text}");
        assert!(text.contains("expire: false"), "{text}");
        assert!(text.contains("type: hash"), "{text}");
        assert!(text.contains("ssh_pwauth: false"), "{text}");
        held.image().unwrap();
    }

    #[test]
    fn a_password_that_is_not_a_hash_is_refused() {
        let mut held = seed();
        held.password = Some("hunter2".to_owned());
        assert_eq!(held.image().unwrap_err().kind(), "seed-invalid");
    }
}
