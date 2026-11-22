fn main() {
    // Sur Windows, l'app demande les droits administrateur au lancement (UAC)
    // pour pouvoir modifier la configuration réseau.
    let attrs = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows-app-manifest.xml")),
    );
    tauri_build::try_build(attrs).expect("échec de tauri-build");
}
