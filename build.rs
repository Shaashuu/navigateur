// Sous Windows, on intègre l'icône dans le .exe (visible dans l'Explorateur et la barre des tâches).
fn main() {
    println!("cargo:rerun-if-changed=packaging/feather.ico");
    // Windows : les DLL dont feather.exe dépend sont cherchées seulement dans
    // System32 (LOAD_LIBRARY_SEARCH_SYSTEM32), jamais dans le dossier de
    // l'exécutable : souvent Téléchargements, où un site pourrait déposer une
    // fausse DLL qui serait chargée au lancement suivant. Voir aussi
    // `load_system_dlls` dans main.rs.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
    }
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
