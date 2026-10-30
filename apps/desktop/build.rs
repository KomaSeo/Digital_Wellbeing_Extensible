//! Responsibility: Embed the Windows manifest that makes digital-wellbeing.exe always run as
//! administrator.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::manifest::ExecutionLevel;
        use embed_manifest::{embed_manifest, new_manifest};
        embed_manifest(
            new_manifest("Digital.Wellbeing")
                .requested_execution_level(ExecutionLevel::RequireAdministrator),
        )
        .expect("unable to embed manifest");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
