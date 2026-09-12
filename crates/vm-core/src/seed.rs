//! The cloud-init `NoCloud` seed: a small vfat volume labelled `cidata` holding
//! `user-data` and `meta-data`, attached to the guest as a second drive.
//!
//! This is the only channel by which anything reaches the guest before it
//! boots, so both key injection and volume mounting are written here.

use crate::error::{Error, Result};
use crate::fat;
use crate::value::{Value, to_yaml};
use std::fs;
use std::path::Path;

/// The label cloud-init's `NoCloud` source matches on. Nothing else is looked at.
pub const LABEL: &str = "cidata";

/// A share to mount in the guest, named by its virtiofs tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub tag: String,
    pub target: String,
}

/// Everything the guest is told about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seed {
    /// Identifies this boot to cloud-init. A change re-runs the per-instance
    /// modules, so it must be stable for the life of the instance.
    pub instance_id: String,
    pub hostname: String,
    /// The account the key is installed for, and the one `vm ssh` connects as.
    pub user: String,
    pub authorized_key: String,
    pub mounts: Vec<Mount>,
}

impl Seed {
    pub fn meta_data(&self) -> String {
        to_yaml(&Value::map([
            ("instance-id", Value::string(&self.instance_id)),
            ("local-hostname", Value::string(&self.hostname)),
        ]))
    }

    /// The cloud-config document. Password login is disabled outright: the
    /// generated key is the only way in, so a default password left enabled
    /// would only ever be a way for someone else in.
    pub fn user_data(&self) -> String {
        let mut fields = vec![
            ("hostname", Value::string(&self.hostname)),
            ("preserve_hostname", Value::Bool(false)),
            (
                "users",
                Value::list([Value::map([
                    ("name", Value::string(&self.user)),
                    ("shell", Value::string("/bin/bash")),
                    ("lock_passwd", Value::Bool(true)),
                    ("sudo", Value::string("ALL=(ALL) NOPASSWD:ALL")),
                    (
                        "ssh_authorized_keys",
                        Value::list([Value::string(self.authorized_key.trim())]),
                    ),
                ])]),
            ),
            ("ssh_pwauth", Value::Bool(false)),
            ("disable_root", Value::Bool(true)),
        ];
        if !self.mounts.is_empty() {
            // Mounted per boot rather than written into the guest's fstab. A
            // share belongs to the run, not to the disk: an fstab entry would
            // outlive the share it names, fail at the next boot without it,
            // and take the guest's local-fs.target down with it. cloud-init
            // will not take an entry out again once it has written one.
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

    /// Builds the seed image. Its contents are a function of this struct
    /// alone, so the same instance always seeds the same bytes.
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

    /// Rejects anything that would reach the guest as a broken cloud-config
    /// rather than as an error here. A guest that fails to seed is diagnosed
    /// over a serial console, which is a much worse place to find a typo.
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

fn is_username(text: &str) -> bool {
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

/// A shallow shape check, not a validation of the key material: enough to
/// catch a path pasted where its contents belonged.
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
        assert!(text.contains("lock_passwd: true"), "{text}");
        assert!(text.contains("disable_root: true"), "{text}");
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

    /// Nothing is written to the guest's fstab: an entry there would outlive
    /// the share and fail the next boot that went without it.
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
}
