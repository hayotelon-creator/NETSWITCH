//! Accès au réseau : liste des cartes, application des profils, scan.
//! macOS : networksetup / ifconfig (mot de passe admin demandé via osascript).
//! Windows : PowerShell (l'app est lancée en administrateur via son manifeste).

use crate::Profile;
use serde::Serialize;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Serialize, Clone, Debug)]
pub struct Addr {
    pub ip: String,
    pub prefix: u8,
}

#[derive(Serialize, Clone, Debug)]
pub struct Iface {
    /// macOS : nom du service réseau ; Windows : alias de la carte
    pub id: String,
    /// macOS : en0, en7… ; Windows : description du matériel
    pub device: String,
    pub up: bool,
    pub mac: String,
    pub dhcp: Option<bool>,
    pub addrs: Vec<Addr>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Host {
    pub ip: String,
    pub mac: String,
    pub ping: bool,
    pub me: bool,
}

// ---------- Utilitaires ----------

#[allow(unused_mut)]
fn cmd(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW : pas de fenêtre console
    }
    c
}

fn run(c: &mut Command) -> Result<String, String> {
    let o = c.output().map_err(|e| e.to_string())?;
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
        return Err(if err.is_empty() { out.trim().to_string() } else { err });
    }
    Ok(out)
}

fn ip_ok(s: &str) -> Result<String, String> {
    s.trim()
        .parse::<Ipv4Addr>()
        .map(|i| i.to_string())
        .map_err(|_| format!("Adresse IP invalide : « {s} »"))
}

/// Accepte "255.255.255.0", "/24" ou "24".
pub fn mask_to_prefix(m: &str) -> Result<u8, String> {
    let t = m.trim().trim_start_matches('/');
    if let Ok(p) = t.parse::<u8>() {
        if p <= 32 {
            return Ok(p);
        }
    }
    let ip: Ipv4Addr = t.parse().map_err(|_| format!("Masque invalide : « {m} »"))?;
    let v = u32::from(ip);
    if v.leading_ones() + v.trailing_zeros() != 32 {
        return Err(format!("Masque invalide : « {m} »"));
    }
    Ok(v.leading_ones() as u8)
}

pub fn prefix_to_mask(p: u8) -> String {
    let v = if p == 0 { 0 } else { u32::MAX << (32 - p as u32) };
    Ipv4Addr::from(v).to_string()
}

fn mask_or_default(m: &str) -> Result<u8, String> {
    if m.trim().is_empty() {
        Ok(24)
    } else {
        mask_to_prefix(m)
    }
}

fn norm_mac(t: &str) -> Option<String> {
    let parts: Vec<&str> = t.split(|c| c == ':' || c == '-').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut out = Vec::with_capacity(6);
    for p in parts {
        if p.is_empty() || p.len() > 2 || !p.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        out.push(format!("{:0>2}", p.to_lowercase()));
    }
    let s = out.join(":");
    if s == "ff:ff:ff:ff:ff:ff" || s == "00:00:00:00:00:00" {
        return None;
    }
    Some(s)
}

fn find_iface(id: &str) -> Result<Iface, String> {
    list_interfaces()?
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| format!("Interface introuvable : « {id} »"))
}

fn dns_list(p: &Profile) -> Result<Vec<String>, String> {
    p.dns
        .iter()
        .filter(|d| !d.trim().is_empty())
        .map(|d| ip_ok(d))
        .collect()
}

// ---------- macOS ----------

#[cfg(target_os = "macos")]
pub fn list_interfaces() -> Result<Vec<Iface>, String> {
    let order = run(cmd("/usr/sbin/networksetup").arg("-listnetworkserviceorder"))?;
    let mut res = Vec::new();
    let mut current: Option<String> = None;
    for line in order.lines() {
        let l = line.trim();
        if l.starts_with("(Hardware Port:") {
            let name = current.take();
            let dev = l
                .find("Device: ")
                .map(|p| l[p + 8..].trim_end_matches(')').trim().to_string())
                .unwrap_or_default();
            let Some(name) = name else { continue };
            if dev.is_empty() {
                continue;
            }
            let info = run(cmd("/sbin/ifconfig").arg(&dev)).unwrap_or_default();
            let mut it = Iface {
                id: name,
                device: dev,
                up: false,
                mac: String::new(),
                dhcp: None,
                addrs: Vec::new(),
            };
            let mut has_status = false;
            for il in info.lines() {
                let t = il.trim();
                if let Some(r) = t.strip_prefix("ether ") {
                    it.mac = norm_mac(r.trim()).unwrap_or_else(|| r.trim().to_string());
                } else if let Some(r) = t.strip_prefix("inet ") {
                    let parts: Vec<&str> = r.split_whitespace().collect();
                    if parts.len() >= 3 && parts[1] == "netmask" {
                        let m = u32::from_str_radix(parts[2].trim_start_matches("0x"), 16).unwrap_or(0);
                        it.addrs.push(Addr { ip: parts[0].to_string(), prefix: m.count_ones() as u8 });
                    }
                } else if let Some(r) = t.strip_prefix("status: ") {
                    has_status = true;
                    it.up = r.trim() == "active";
                }
            }
            if !has_status {
                it.up = !it.addrs.is_empty();
            }
            res.push(it);
        } else if l.starts_with('(') {
            // "(1) Nom du service" ou "(*) Service désactivé"
            if let Some(pos) = l.find(") ") {
                current = Some(l[pos + 2..].to_string());
            }
        }
    }
    Ok(res)
}

#[cfg(target_os = "macos")]
pub fn apply(p: &Profile) -> Result<(), String> {
    let it = find_iface(&p.iface)?;
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let svc = q(&p.iface);
    let dev = q(&it.device);
    let mut sh: Vec<String> = Vec::new();

    // On retire les IP secondaires existantes (alias)
    for a in it.addrs.iter().skip(1) {
        sh.push(format!("(/sbin/ifconfig {dev} -alias {} || true)", a.ip));
    }

    if p.dhcp {
        sh.push(format!("/usr/sbin/networksetup -setdhcp {svc}"));
    } else {
        let ip = ip_ok(&p.ip)?;
        let mask = prefix_to_mask(mask_or_default(&p.mask)?);
        let gw = if p.gateway.trim().is_empty() {
            String::new()
        } else {
            format!(" {}", ip_ok(&p.gateway)?)
        };
        sh.push(format!("/usr/sbin/networksetup -setmanual {svc} {ip} {mask}{gw}"));
    }

    let dns = dns_list(p)?;
    sh.push(format!(
        "/usr/sbin/networksetup -setdnsservers {svc} {}",
        if dns.is_empty() { "Empty".to_string() } else { dns.join(" ") }
    ));

    let extras: Vec<_> = p.extra.iter().filter(|e| !e.ip.trim().is_empty()).collect();
    if !extras.is_empty() {
        sh.push("sleep 1".into());
    }
    for e in extras {
        let ip = ip_ok(&e.ip)?;
        let m = prefix_to_mask(mask_or_default(&e.mask)?);
        sh.push(format!("/sbin/ifconfig {dev} alias {ip} netmask {m}"));
    }

    let script = sh.join(" && ");
    let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");
    let osa = format!(
        "do shell script \"{escaped}\" with prompt \"NetSwitch veut modifier la configuration réseau.\" with administrator privileges"
    );
    match run(cmd("/usr/bin/osascript").arg("-e").arg(osa)) {
        Ok(_) => Ok(()),
        Err(e) if e.contains("-128") => Err("Annulé (mot de passe non saisi)".into()),
        Err(e) => Err(e),
    }
}

// ---------- Windows ----------

#[cfg(windows)]
fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        s.push(T[(n >> 18 & 63) as usize] as char);
        s.push(T[(n >> 12 & 63) as usize] as char);
        s.push(if c.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        s.push(if c.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    s
}

/// Lance un script PowerShell (encodé, donc sans souci de guillemets/accents).
#[cfg(windows)]
fn ps(script: &str) -> Result<String, String> {
    let full = format!(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; $ProgressPreference='SilentlyContinue'; \
         try {{ $ErrorActionPreference='Stop'; {script} }} catch {{ Write-Output ('ERR:' + $_.Exception.Message); exit 1 }}"
    );
    let bytes: Vec<u8> = full.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let o = cmd("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &b64(&bytes)])
        .output()
        .map_err(|e| e.to_string())?;
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    if let Some(pos) = out.find("ERR:") {
        return Err(out[pos + 4..].trim().to_string());
    }
    if !o.status.success() {
        return Err(format!("PowerShell a échoué : {}", out.trim()));
    }
    Ok(out)
}

#[cfg(windows)]
fn psq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

#[cfg(windows)]
pub fn list_interfaces() -> Result<Vec<Iface>, String> {
    let script = r#"
$ad = @(Get-NetAdapter | ForEach-Object { [pscustomobject]@{ Name=[string]$_.Name; Status=[string]$_.Status; Mac=[string]$_.MacAddress; Desc=[string]$_.InterfaceDescription } })
$ip = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue | ForEach-Object { [pscustomobject]@{ Alias=[string]$_.InterfaceAlias; Ip=[string]$_.IPAddress; Prefix=[int]$_.PrefixLength } })
$dh = @(Get-NetIPInterface -AddressFamily IPv4 -ErrorAction SilentlyContinue | ForEach-Object { [pscustomobject]@{ Alias=[string]$_.InterfaceAlias; Dhcp=[string]$_.Dhcp } })
ConvertTo-Json -InputObject @{ a=$ad; i=$ip; d=$dh } -Depth 4 -Compress
"#;
    let out = ps(script)?;
    let v: serde_json::Value = serde_json::from_str(out.trim()).map_err(|e| format!("Lecture des cartes : {e}"))?;
    let arr = |k: &str| v.get(k).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let s = |x: &serde_json::Value, k: &str| x.get(k).and_then(|y| y.as_str()).unwrap_or("").to_string();

    let ips = arr("i");
    let dh = arr("d");
    let mut res = Vec::new();
    for a in arr("a") {
        let name = s(&a, "Name");
        let addrs = ips
            .iter()
            .filter(|i| s(i, "Alias") == name)
            .map(|i| Addr {
                ip: s(i, "Ip"),
                prefix: i.get("Prefix").and_then(|p| p.as_u64()).unwrap_or(0) as u8,
            })
            .collect();
        let dhcp = dh.iter().find(|d| s(d, "Alias") == name).map(|d| s(d, "Dhcp") == "Enabled");
        let mac_raw = s(&a, "Mac");
        res.push(Iface {
            up: s(&a, "Status") == "Up",
            mac: norm_mac(&mac_raw).unwrap_or(mac_raw),
            device: s(&a, "Desc"),
            dhcp,
            addrs,
            id: name,
        });
    }
    Ok(res)
}

#[cfg(windows)]
pub fn apply(p: &Profile) -> Result<(), String> {
    find_iface(&p.iface)?;
    let mut sc = vec![
        format!("$a = {}", psq(&p.iface)),
        "Get-NetRoute -InterfaceAlias $a -DestinationPrefix '0.0.0.0/0' -ErrorAction SilentlyContinue | Remove-NetRoute -Confirm:$false -ErrorAction SilentlyContinue".into(),
        "Get-NetIPAddress -InterfaceAlias $a -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false -ErrorAction SilentlyContinue".into(),
    ];
    let extras: Vec<_> = p.extra.iter().filter(|e| !e.ip.trim().is_empty()).collect();

    if p.dhcp {
        if !extras.is_empty() {
            return Err("Sur Windows, les IP supplémentaires nécessitent une IP fixe (pas de DHCP).".into());
        }
        sc.push("Set-NetIPInterface -InterfaceAlias $a -AddressFamily IPv4 -Dhcp Enabled".into());
    } else {
        let ip = ip_ok(&p.ip)?;
        let prefix = mask_or_default(&p.mask)?;
        sc.push("Set-NetIPInterface -InterfaceAlias $a -AddressFamily IPv4 -Dhcp Disabled".into());
        let gw = if p.gateway.trim().is_empty() {
            String::new()
        } else {
            format!(" -DefaultGateway '{}'", ip_ok(&p.gateway)?)
        };
        sc.push(format!(
            "New-NetIPAddress -InterfaceAlias $a -AddressFamily IPv4 -IPAddress '{ip}' -PrefixLength {prefix}{gw} | Out-Null"
        ));
        for e in extras {
            let eip = ip_ok(&e.ip)?;
            let ep = mask_or_default(&e.mask)?;
            sc.push(format!(
                "New-NetIPAddress -InterfaceAlias $a -AddressFamily IPv4 -IPAddress '{eip}' -PrefixLength {ep} | Out-Null"
            ));
        }
    }

    let dns = dns_list(p)?;
    if dns.is_empty() {
        sc.push("Set-DnsClientServerAddress -InterfaceAlias $a -ResetServerAddresses".into());
    } else {
        let list: Vec<String> = dns.iter().map(|d| format!("'{d}'")).collect();
        sc.push(format!("Set-DnsClientServerAddress -InterfaceAlias $a -ServerAddresses @({})", list.join(",")));
    }

    ps(&sc.join("\n")).map(|_| ())
}

// ---------- Autres systèmes (pour que ça compile partout) ----------

#[cfg(not(any(windows, target_os = "macos")))]
pub fn list_interfaces() -> Result<Vec<Iface>, String> {
    Err("Système non pris en charge".into())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn apply(_p: &Profile) -> Result<(), String> {
    Err("Système non pris en charge".into())
}

// ---------- Scan (commun) ----------

fn ping(ip: Ipv4Addr) -> bool {
    let target = ip.to_string();
    #[cfg(windows)]
    {
        // "TTL=" n'apparaît que si l'hôte a vraiment répondu
        run(cmd("ping").args(["-n", "1", "-w", "500", &target]))
            .map(|o| o.contains("TTL="))
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        cmd("/sbin/ping")
            .args(["-c", "1", "-W", "500", &target])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

fn arp_table() -> HashMap<Ipv4Addr, String> {
    #[cfg(windows)]
    let out = run(cmd("arp").arg("-a")).unwrap_or_default();
    #[cfg(not(windows))]
    let out = run(cmd("/usr/sbin/arp").arg("-an")).unwrap_or_default();

    let mut map = HashMap::new();
    for line in out.lines() {
        let mut ip = None;
        let mut mac = None;
        for tok in line.split_whitespace() {
            let t = tok.trim_matches(|c| c == '(' || c == ')');
            if ip.is_none() {
                if let Ok(a) = t.parse::<Ipv4Addr>() {
                    ip = Some(a);
                    continue;
                }
            }
            if mac.is_none() {
                mac = norm_mac(t);
            }
        }
        if let (Some(a), Some(m)) = (ip, mac) {
            map.insert(a, m);
        }
    }
    map
}

pub fn scan(iface: &str) -> Result<Vec<Host>, String> {
    let it = find_iface(iface)?;
    let a = it
        .addrs
        .iter()
        .find(|a| !a.ip.starts_with("169.254."))
        .or(it.addrs.first())
        .ok_or_else(|| "Cette interface n'a pas d'adresse IPv4".to_string())?;
    let me: Ipv4Addr = a.ip.parse().map_err(|_| "Adresse locale illisible".to_string())?;

    // Limite à 1022 adresses max (/22) autour de notre IP
    let p = a.prefix.clamp(22, 32) as u32;
    let mask = if p == 0 { 0 } else { u32::MAX << (32 - p) };
    let net = u32::from(me) & mask;
    let size = if p >= 32 { 1 } else { 1u32 << (32 - p) };
    let targets: Vec<Ipv4Addr> = if size <= 2 {
        vec![]
    } else {
        (1..size - 1).map(|k| Ipv4Addr::from(net + k)).filter(|ip| *ip != me).collect()
    };

    let alive = Mutex::new(Vec::new());
    let idx = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..64 {
            s.spawn(|| loop {
                let i = idx.fetch_add(1, Ordering::SeqCst);
                if i >= targets.len() {
                    break;
                }
                if ping(targets[i]) {
                    alive.lock().unwrap().push(targets[i]);
                }
            });
        }
    });

    let arp = arp_table();
    let broadcast = net | !mask;
    let mut hosts: HashMap<Ipv4Addr, Host> = HashMap::new();
    for ip in alive.into_inner().unwrap() {
        hosts.insert(ip, Host { ip: ip.to_string(), mac: arp.get(&ip).cloned().unwrap_or_default(), ping: true, me: false });
    }
    // Appareils qui ne répondent pas au ping mais sont dans la table ARP
    for (ip, mac) in &arp {
        let v = u32::from(*ip);
        if v & mask == net && v != net && v != broadcast && *ip != me {
            hosts.entry(*ip).or_insert(Host { ip: ip.to_string(), mac: mac.clone(), ping: false, me: false });
        }
    }
    hosts.insert(me, Host { ip: me.to_string(), mac: it.mac.clone(), ping: true, me: true });

    let mut v: Vec<Host> = hosts.into_values().collect();
    v.sort_by_key(|h| h.ip.parse::<Ipv4Addr>().map(u32::from).unwrap_or(0));
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn masks() {
        assert_eq!(mask_to_prefix("255.255.255.0").unwrap(), 24);
        assert_eq!(mask_to_prefix("/16").unwrap(), 16);
        assert!(mask_to_prefix("255.0.255.0").is_err());
        assert_eq!(prefix_to_mask(22), "255.255.252.0");
        assert_eq!(norm_mac("0:1c:b3:a:ff:1").unwrap(), "00:1c:b3:0a:ff:01");
        assert_eq!(norm_mac("AA-BB-CC-DD-EE-FF").unwrap(), "aa:bb:cc:dd:ee:ff");
    }
}
