use std::fmt::Write as _;

fn main() {
    // Base des fabricants : si "manuf.txt" (format Wireshark) est présent,
    // on en extrait une table compacte OUI -> nom. Sinon la table est vide.
    println!("cargo:rerun-if-changed=manuf.txt");
    let mut out = String::new();
    if let Ok(src) = std::fs::read_to_string("manuf.txt") {
        for line in src.lines() {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split('\t').map(str::trim).filter(|c| !c.is_empty()).collect();
            if cols.len() < 2 {
                continue;
            }
            let prefix = cols[0];
            if prefix.contains('/') {
                continue; // plages plus fines que /24 ignorées
            }
            let hex: String = prefix.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_uppercase();
            if hex.len() != 6 {
                continue;
            }
            let name = cols.get(2).copied().unwrap_or(cols[1]);
            let name: String = name.replace('\t', " ").chars().take(48).collect();
            let _ = writeln!(out, "{hex}\t{name}");
        }
    }
    let dest = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("oui.txt");
    std::fs::write(dest, out).expect("écriture oui.txt");

    // Sur Windows, l'app demande les droits administrateur au lancement (UAC)
    // pour pouvoir modifier la configuration réseau.
    let attrs = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows-app-manifest.xml")),
    );
    tauri_build::try_build(attrs).expect("échec de tauri-build");
}
