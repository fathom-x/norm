//! Rewrites crates/overpay-sdk/src/generated.rs from crates/overpay-sdk/spec/overpay-v1.json.

use std::path::Path;

fn main() {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../overpay-sdk");
    let spec_path = sdk.join("spec/overpay-v1.json");
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&spec_path)
            .unwrap_or_else(|e| panic!("{}: {e}", spec_path.display())),
    )
    .expect("the spec is JSON");
    let out = sdk.join("src/generated.rs");
    std::fs::write(&out, overpay_sdk_codegen::render(&spec)).expect("write generated.rs");
    println!("wrote {}", out.display());
}
