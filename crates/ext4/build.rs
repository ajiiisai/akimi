fn main() {
    println!("cargo:rerun-if-changed=src/native.c");

    let library = pkg_config::Config::new()
        .atleast_version("1.47")
        .probe("ext2fs")
        .expect(
            "libext2fs was not found; enter `nix develop` or install e2fsprogs development files",
        );
    pkg_config::Config::new().probe("com_err").expect(
        "libcom_err was not found; enter `nix develop` or install e2fsprogs development files",
    );

    let mut build = cc::Build::new();
    build.file("src/native.c").warnings(true).opt_level(3);
    for include in library.include_paths {
        build.include(include);
    }
    build.compile("akimi_ext2fs_shim");
}
