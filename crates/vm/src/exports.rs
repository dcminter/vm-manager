//! `vm export`.

use crate::progress::Bar;
use crate::reports;
use std::path::Path;
use vm_core::Reference;
use vm_core::catalogue::Catalogue;
use vm_core::compression::Compression;
use vm_core::error::{Error, Result};
use vm_core::export;
use vm_core::store::Store;

/// What `vm export` was asked for.
pub struct Request<'a> {
    pub reference: &'a str,
    pub file: &'a Path,
    pub arch: &'a str,
    pub compression: Compression,
    pub force: bool,
}

/// Copies a held image out of the store, drawing its progress.
pub fn run(
    catalogue: &Catalogue,
    store: &Store,
    request: &Request<'_>,
    text: bool,
) -> Result<reports::Exported> {
    let reference: Reference = request.reference.parse()?;
    let (entry, artifact) = catalogue.resolve(&reference, request.arch)?;
    if !store.contains(&artifact.digest) {
        return Err(Error::NotExportable {
            reference: request.reference.to_owned(),
        });
    }
    let (path, suffixed) = export::destination(
        request.file,
        artifact,
        request.compression,
        &entry.name,
        &entry.tag,
    )?;
    let mut bar = Bar::new("  exporting ", text);
    let outcome = export::copy(
        &store.path_for(&artifact.digest),
        artifact,
        &path,
        request.compression,
        request.force,
        &mut |progress| bar.update(progress),
    );
    bar.clear();
    let exported = outcome?;
    Ok(reports::Exported {
        name: entry.name.clone(),
        tag: entry.tag.clone(),
        arch: artifact.arch.clone(),
        format: export::stored_format(artifact).to_owned(),
        compression: request.compression,
        cdrom: artifact.media.is_cdrom(),
        digest: artifact.digest.to_string(),
        path: exported.path.display().to_string(),
        size: exported.size,
        suffixed,
        verified: exported.verified,
    })
}

/// A compression scheme to export with.
pub fn parse_compression(text: &str) -> std::result::Result<Compression, String> {
    Compression::SCHEMES
        .into_iter()
        .find(|scheme| scheme.name() == text)
        .ok_or_else(|| format!("'{text}' is not a compression scheme; use xz, gzip or zstd"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_compressing_schemes_are_accepted_by_name() {
        assert_eq!(parse_compression("xz"), Ok(Compression::Xz));
        assert_eq!(parse_compression("gzip"), Ok(Compression::Gzip));
        assert_eq!(parse_compression("zstd"), Ok(Compression::Zstd));
        for refused in ["none", "gz", "zst", "bzip2", ""] {
            assert!(parse_compression(refused).is_err(), "{refused}");
        }
    }
}
