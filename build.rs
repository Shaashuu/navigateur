// Sous Windows, on intègre l'icône dans le .exe (visible dans l'Explorateur et la barre des tâches).
fn main() {
    println!("cargo:rerun-if-changed=packaging/feather.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/feather.ico");
        res.set("ProductName", "Feather");
        res.set("FileDescription", "Feather");
        if let Err(err) = res.compile() {
            println!("cargo:warning=icône Windows non intégrée : {err}");
        }
    }
}
