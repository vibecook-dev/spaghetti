//! Physical workspace crate for RFC 012 X3.
//!
//! This crate must not depend on `spaghetti-napi`. It only asserts that the
//! landed logical layer modules and architecture checkers remain the
//! dependency boundary after crate extraction begins.

use std::path::Path;

/// Landed RFC 012 logical layers that must remain distinct modules.
pub const RFC012_LAYER_MODULES: &[&str] = &[
    "adapter",
    "source",
    "decode_runtime",
    "runtime_semantic_reducer",
    "engine",
    "observer",
];

/// Pre-landing parallel authorities that must not reappear.
pub const RETIRED_RFC012_LAYER_MODULES: &[&str] = &[
    "catalog_contract",
    "observation_contract",
    "scoped_observation",
];

pub fn napi_src_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../spaghetti-napi/src")
}

pub fn architecture_checker() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/architecture/check_rfc011_boundaries.py")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_boundaries_mirror_landed_rfc012_dependency_layers() {
        let src = napi_src_root();
        for layer in RFC012_LAYER_MODULES {
            let path = src.join(layer);
            assert!(
                path.is_dir() || path.with_extension("rs").is_file(),
                "missing RFC 012 layer {layer} under {}",
                src.display()
            );
        }
        for retired in RETIRED_RFC012_LAYER_MODULES {
            let path = src.join(retired);
            assert!(
                !path.is_dir() && !path.with_extension("rs").is_file(),
                "retired RFC 012 parallel authority {retired} reappeared under {}",
                src.display()
            );
        }
        assert!(
            architecture_checker().is_file(),
            "architecture checker must remain the executable dependency ratchet"
        );
        let cargo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let manifest = std::fs::read_to_string(cargo).unwrap();
        assert!(manifest.contains("spaghetti-architecture"));
        assert!(manifest.contains("spaghetti-napi"));
        assert!(
            manifest.contains("spaghetti-coverage"),
            "workspace must list the extracted coverage crate"
        );
        assert!(
            !manifest.contains("spaghetti-napi") || manifest.contains("spaghetti-architecture"),
            "workspace must list both the engine crate and this boundary crate"
        );
        let coverage = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spaghetti-coverage");
        assert!(coverage.join("src/lib.rs").is_file());
        let coverage_manifest = std::fs::read_to_string(coverage.join("Cargo.toml")).unwrap();
        assert!(
            !coverage_manifest.contains("rusqlite")
                && !coverage_manifest.contains("napi")
                && !coverage_manifest.contains("spaghetti-napi"),
            "spaghetti-coverage must stay store-free and transport-free"
        );
    }
}
