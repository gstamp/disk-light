fn main() {
    println!("cargo:rerun-if-changed=assets/disk-light.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winres::WindowsResource::new()
            .set_icon("assets/disk-light.ico")
            .set("ProductName", "Disk Light")
            .set("FileDescription", "Disk Light")
            .compile()
            .expect("could not compile Disk Light's Windows resources");
    }
}
