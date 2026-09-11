//! Speech models: naming them, fetching them, and verifying what arrived.
//!
//! A model is named three ways - a built-in, a local file, or any URL with a
//! digest - so a model this crate has never heard of needs no code change.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// A downloadable model. `sha256` is what the file must hash to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSpec {
    pub name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size_bytes: u64,
}

/// Models this crate knows by name. Digests are from the publisher's index.
pub const BUILTIN: &[ModelSpec] = &[
    ModelSpec {
        name: "tiny.en",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin",
        sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
        size_bytes: 77_691_713,
    },
    ModelSpec {
        name: "base.en",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin",
        sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
        size_bytes: 147_964_211,
    },
    ModelSpec {
        name: "small.en",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin",
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        size_bytes: 487_601_967,
    },
];

pub fn builtin(name: &str) -> Option<&'static ModelSpec> {
    BUILTIN.iter().find(|m| m.name == name)
}

/// How the user named a model.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    /// A built-in name, such as `base.en`.
    #[serde(default)]
    pub name: Option<String>,
    /// A model file already on disk. Nothing is downloaded or verified.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Any ggml model. Requires `sha256`.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
}

impl Default for ModelRef {
    fn default() -> Self {
        Self {
            name: Some("base.en".into()),
            path: None,
            url: None,
            sha256: None,
        }
    }
}

/// Where a named model resolves to, and whether it must be fetched first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Already on disk; use as-is.
    Local(PathBuf),
    Download {
        url: String,
        sha256: String,
        cached_as: PathBuf,
    },
}

impl ModelRef {
    /// Resolve to a file path, deciding what to fetch. Nothing is fetched here.
    pub fn resolve(&self, cache_dir: &Path) -> Result<Source> {
        match (&self.path, &self.url, &self.name) {
            (Some(path), None, None) => Ok(Source::Local(path.clone())),
            (None, Some(url), None) => {
                let sha256 = self.sha256.clone().ok_or_else(|| {
                    Error::Model("a model url needs a sha256 to verify it against".into())
                })?;
                Ok(Source::Download {
                    cached_as: cache_dir.join(format!("{sha256}.bin")),
                    url: url.clone(),
                    sha256,
                })
            }
            (None, None, Some(name)) => {
                let spec = builtin(name).ok_or_else(|| {
                    let known: Vec<&str> = BUILTIN.iter().map(|m| m.name).collect();
                    Error::Model(format!(
                        "unknown model {name:?}; known: {}",
                        known.join(", ")
                    ))
                })?;
                Ok(Source::Download {
                    url: spec.url.into(),
                    sha256: spec.sha256.into(),
                    cached_as: cache_dir.join(format!("ggml-{}.bin", spec.name)),
                })
            }
            (None, None, None) => Self::default().resolve(cache_dir),
            _ => Err(Error::Model(
                "name, path and url are alternatives; set exactly one".into(),
            )),
        }
    }
}

/// The file for this source, downloading and verifying it if absent.
pub fn ensure(source: &Source, progress: &mut impl Write) -> Result<PathBuf> {
    match source {
        Source::Local(path) if path.exists() => Ok(path.clone()),
        Source::Local(path) => Err(Error::Model(format!("{} does not exist", path.display()))),
        Source::Download {
            url,
            sha256,
            cached_as,
        } => {
            if cached_as.exists() {
                return Ok(cached_as.clone());
            }
            if let Some(parent) = cached_as.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = writeln!(progress, "downloading {url}");
            // Into a temporary first, so an interrupted download is never
            // mistaken for a cached model on the next run.
            let tmp = cached_as.with_extension("part");
            download(url, &tmp)?;
            let got = sha256_file(&tmp)?;
            if got != *sha256 {
                let _ = std::fs::remove_file(&tmp);
                return Err(Error::Model(format!(
                    "digest mismatch: expected {sha256}, got {got}"
                )));
            }
            std::fs::rename(&tmp, cached_as)?;
            let _ = writeln!(progress, "verified and cached at {}", cached_as.display());
            Ok(cached_as.clone())
        }
    }
}

fn download(url: &str, to: &Path) -> Result<()> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| Error::Model(format!("fetching {url}: {e}")))?;
    let mut reader = response.into_body().into_reader();
    let mut file = std::fs::File::create(to)?;
    std::io::copy(&mut reader, &mut file)?;
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Where downloaded models live.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("HERDR_PLUGIN_STATE_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join("models"));
    }
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| std::io::Error::other("HOME is unset"))?;
    Ok(PathBuf::from(home).join(".cache/herdr-dictate/models"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> PathBuf {
        PathBuf::from("/cache")
    }

    #[test]
    fn every_builtin_has_a_digest_and_a_size() {
        assert!(!BUILTIN.is_empty());
        for spec in BUILTIN {
            assert_eq!(spec.sha256.len(), 64, "{} digest", spec.name);
            assert!(
                spec.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                "{} digest",
                spec.name
            );
            assert!(spec.size_bytes > 0, "{} size", spec.name);
            assert!(spec.url.starts_with("https://"), "{} url", spec.name);
        }
    }

    #[test]
    fn builtin_names_are_unique() {
        let mut names: Vec<&str> = BUILTIN.iter().map(|m| m.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }

    #[test]
    fn a_builtin_name_resolves_to_a_download() {
        let reference = ModelRef {
            name: Some("base.en".into()),
            ..Default::default()
        };
        match reference.resolve(&cache()).unwrap() {
            Source::Download { url, cached_as, .. } => {
                assert!(url.contains("ggml-base.en.bin"));
                assert_eq!(cached_as, cache().join("ggml-base.en.bin"));
            }
            other => panic!("expected a download, got {other:?}"),
        }
    }

    #[test]
    fn the_default_is_a_known_builtin() {
        assert!(matches!(
            ModelRef::default().resolve(&cache()),
            Ok(Source::Download { .. })
        ));
    }

    #[test]
    fn an_unknown_name_lists_what_is_known() {
        let reference = ModelRef {
            name: Some("enormous.en".into()),
            path: None,
            url: None,
            sha256: None,
        };
        let err = reference.resolve(&cache()).unwrap_err().to_string();
        assert!(err.contains("enormous.en"));
        assert!(err.contains("base.en"), "names what it does know: {err}");
    }

    #[test]
    fn a_path_is_used_as_is() {
        let reference = ModelRef {
            name: None,
            path: Some("/models/mine.bin".into()),
            url: None,
            sha256: None,
        };
        assert_eq!(
            reference.resolve(&cache()).unwrap(),
            Source::Local("/models/mine.bin".into())
        );
    }

    #[test]
    fn a_url_without_a_digest_is_refused() {
        let reference = ModelRef {
            name: None,
            path: None,
            url: Some("https://example.invalid/m.bin".into()),
            sha256: None,
        };
        assert!(
            reference
                .resolve(&cache())
                .unwrap_err()
                .to_string()
                .contains("sha256")
        );
    }

    #[test]
    fn a_url_with_a_digest_caches_under_it() {
        let reference = ModelRef {
            name: None,
            path: None,
            url: Some("https://example.invalid/m.bin".into()),
            sha256: Some("abc123".into()),
        };
        match reference.resolve(&cache()).unwrap() {
            Source::Download { cached_as, .. } => assert_eq!(cached_as, cache().join("abc123.bin")),
            other => panic!("expected a download, got {other:?}"),
        }
    }

    #[test]
    fn naming_two_ways_at_once_is_refused() {
        let reference = ModelRef {
            name: Some("base.en".into()),
            path: Some("/models/mine.bin".into()),
            url: None,
            sha256: None,
        };
        assert!(reference.resolve(&cache()).is_err());
    }

    #[test]
    fn a_missing_local_model_is_reported_not_downloaded() {
        let source = Source::Local("/models/absent.bin".into());
        assert!(
            ensure(&source, &mut Vec::new())
                .unwrap_err()
                .to_string()
                .contains("does not exist")
        );
    }

    #[test]
    fn hashing_matches_a_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        // SHA-256 of "abc".
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_cached_file_is_reused_without_a_download() {
        let dir = tempfile::tempdir().unwrap();
        let cached = dir.path().join("ggml-base.en.bin");
        std::fs::write(&cached, b"pretend model").unwrap();
        let source = Source::Download {
            url: "https://example.invalid/never-fetched".into(),
            sha256: "unused".into(),
            cached_as: cached.clone(),
        };
        assert_eq!(ensure(&source, &mut Vec::new()).unwrap(), cached);
    }
}
