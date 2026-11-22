# NetSwitch

Gestionnaire de profils réseau dans la barre des menus (Mac) / la zone de notification (Windows).

- Profils IP fixe / DHCP + DNS, appliqués en 1 clic depuis l'icône
- Plusieurs IP sur la même carte (ex. console + Dante)
- Détection auto : applique un profil quand le câble d'une interface est branché
- Scan du sous-réseau (ping + table ARP)

## Compilation (GitHub Actions)

Chaque push sur `main` compile automatiquement :
onglet **Actions** → dernier run → section **Artifacts** → `NetSwitch-mac` (.dmg) et `NetSwitch-windows` (.exe).

## Premier lancement

**Mac** : l'app n'est pas signée. Après l'avoir copiée dans Applications :
`xattr -cr /Applications/NetSwitch.app` dans le Terminal, puis ouvre-la.
Le mot de passe admin est demandé à chaque changement de profil.

**Windows** : SmartScreen → « Informations complémentaires » → « Exécuter quand même ».
L'app demande les droits administrateur au lancement (nécessaire pour changer l'IP).

## Compiler en local (optionnel)

Node.js + Rust installés, puis :
```
npm install
npm run icons
npm run build
```
