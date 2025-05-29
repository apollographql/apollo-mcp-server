use std::convert::TryFrom;
use std::env;
use std::str;

pub use anyhow;
use camino::Utf8PathBuf;
use once_cell::sync::Lazy;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

pub static PKG_PROJECT_ROOT: Lazy<Utf8PathBuf> = Lazy::new(|| {
    let manifest_dir =
        Utf8PathBuf::try_from(MANIFEST_DIR).expect("could not get the root directory.");
    let root_dir = manifest_dir
        .ancestors()
        .nth(1)
        .expect("could not find project root");

    root_dir.to_path_buf()
});
