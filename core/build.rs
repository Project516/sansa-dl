fn main() {
    let vendored = [
        "alloc", "n2e_99", "n2e_ds", "ucl_init", "ucl_ptr", "ucl_util",
    ]
    .map(|name| format!("vendor/ucl/src/{name}.c"));
    cc::Build::new()
        .files(&vendored)
        .file("src/ucl_shim.c")
        .include("vendor/ucl/include")
        .include("vendor/ucl/src")
        .include("vendor/ucl")
        .opt_level(2)
        .warnings(false)
        .compile("ucl");
    println!("cargo:rerun-if-changed=vendor/ucl");
    println!("cargo:rerun-if-changed=src/ucl_shim.c");
}
