use std::path::Path;

#[test]
fn cargo_package_library_and_binary_are_named_vaire() {
    assert_eq!(env!("CARGO_PKG_NAME"), "vaire");
    assert_eq!(
        std::any::type_name::<vaire::provider::ProviderId>(),
        "vaire::provider::ProviderId"
    );

    let binary = Path::new(env!("CARGO_BIN_EXE_vaire"));
    assert_eq!(binary.file_name().unwrap(), "vaire");
}
