//! Installs and removes bookmole's KOReader plugin on a mounted reader.
//! The only engine module that writes to a reader; it writes only inside its own plugin directory.
//! Files are compared by sha256, never mtime: reader volumes are FAT32.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::device::{is_koreader_mount, koreader_dir};
use crate::error::Result;
use crate::files::sha256_of;
use crate::koreader::{eval_table, get_int, get_str, sandboxed_lua};

/// The directory we own on a reader. Nothing outside it is ever written.
pub const PLUGIN_DIR_NAME: &str = "bookmole.koplugin";

/// Integer, compared with `>` against the reader's `_meta.lua`. Bump it whenever the shipped plugin
/// changes behaviour, or readers carrying the old one report up to date.
pub const PLUGIN_VERSION: i64 = 3;

/// Written by the installer and by the plugin (on wifi link/unlink). Both writers must produce the
/// same bytes, or each rewrites the other's work on every connection.
const PAIRING_FILE: &str = "pairing.lua";
const MANIFEST_FILE: &str = "installed.lua";
/// Written by the plugin on the device, never hashed. [`inspect`] must skip it and [`uninstall`]
/// must remove it, or a file absent from the manifest obstructs both.
const ENDPOINT_FILE: &str = "endpoint.lua";

const UNHASHED_FILES: [&str; 3] = [MANIFEST_FILE, PAIRING_FILE, ENDPOINT_FILE];

/// Whether `name` is OS litter: `.DS_Store`, or the AppleDouble `._x` shadow of a file we own.
/// Shadows of anyone else's files stay theirs.
fn is_os_litter(name: &str) -> bool {
    if name == ".DS_Store" {
        return true;
    }
    name.strip_prefix("._").is_some_and(|shadowed| {
        UNHASHED_FILES.contains(&shadowed) || FILES.iter().any(|(ours, _)| *ours == shadowed)
    })
}

fn sweep_os_litter(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if is_os_litter(&entry.file_name().to_string_lossy()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

const FILES: &[(&str, &str)] = &[
    (
        "_meta.lua",
        include_str!("../koplugin/bookmole.koplugin/_meta.lua"),
    ),
    (
        "main.lua",
        include_str!("../koplugin/bookmole.koplugin/main.lua"),
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PluginRefusal {
    #[error("{} is not a KOReader install", path.display())]
    NotAKoreaderMount { path: PathBuf },
    #[error("{} is a symlink, not a mounted reader", path.display())]
    MountIsASymlink { path: PathBuf },
    #[error(
        "a newer plugin is already installed (version {installed}; this bookmole carries {ours})"
    )]
    NewerAlreadyInstalled { installed: i64, ours: i64 },
    #[error("these files were edited on the device since bookmole wrote them: {}", paths.join(", "))]
    Modified { paths: Vec<String> },
    #[error("these files are in the plugin directory and bookmole did not put them there: {}", paths.join(", "))]
    Unrecognised { paths: Vec<String> },
    /// Installed, but not by a bookmole that left a manifest behind.
    #[error(
        "{} holds a plugin with no bookmole manifest, so it cannot be removed exactly",
        path.display()
    )]
    NoManifest { path: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStatus {
    pub mount: PathBuf,
    pub plugin_dir: PathBuf,
    pub installed: bool,
    pub installed_version: Option<i64>,
    pub our_version: i64,
    pub paired: bool,
    /// Our id for this reader. `inspect` has no database and fills it with the first pairing;
    /// `Engine::plugin_status` re-points it at the pairing we have a row for, else `None`.
    pub device_id: Option<String>,
    /// Every computer this reader is paired with, ours included, in file order.
    pub pairings: Vec<Pairing>,
    pub modified: Vec<String>,
    pub unrecognised: Vec<String>,
}

/// What an install onto this reader would do: one mutually exclusive answer.
/// `Obstructed` wins over every version case, since it gates the write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCondition {
    Absent,
    Current,
    Upgradable,
    /// Ours, but its `_meta.lua` has no readable version. Not folded into `Upgradable`: unknown is not
    /// older.
    Unversioned,
    Obstructed,
}

impl PluginStatus {
    pub fn condition(&self) -> PluginCondition {
        if self.is_obstructed() {
            return PluginCondition::Obstructed;
        }
        if !self.installed {
            return PluginCondition::Absent;
        }
        match self.installed_version {
            None => PluginCondition::Unversioned,
            Some(v) if v < self.our_version => PluginCondition::Upgradable,
            Some(_) => PluginCondition::Current,
        }
    }

    /// A plugin is already there, of any version; a same-version reinstall counts.
    pub fn is_reinstall(&self) -> bool {
        self.installed
    }

    pub fn is_version_upgrade(&self) -> bool {
        self.installed && self.installed_version.is_some_and(|v| v < self.our_version)
    }

    pub fn is_obstructed(&self) -> bool {
        !self.modified.is_empty()
            || !self.unrecognised.is_empty()
            || self.installed_version.is_some_and(|v| v > self.our_version)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub plugin_dir: PathBuf,
    pub device_id: String,
    pub version: i64,
    pub written: Vec<String>,
    pub upgraded_from: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallReport {
    pub plugin_dir: PathBuf,
    pub removed: Vec<String>,
    /// The pairing row dropped, if any. Filled from the file's first entry and re-pointed by
    /// `Engine::uninstall_plugin`, since only the database knows which computer we are.
    pub forgot_device: Option<String>,
    /// Every pairing destroyed, ours and other computers'; removing the plugin ends them all.
    pub removed_pairings: Vec<String>,
}

pub fn plugin_dir(mount: &Path) -> Option<PathBuf> {
    koreader_dir(mount).map(|k| k.join("plugins").join(PLUGIN_DIR_NAME))
}

fn check_mount(mount: &Path) -> Result<PathBuf> {
    // Checked before the contents: a symlink's target may be a KOReader install we were never pointed
    // at.
    if mount
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(PluginRefusal::MountIsASymlink {
            path: mount.to_path_buf(),
        }
        .into());
    }
    if !is_koreader_mount(mount) {
        return Err(PluginRefusal::NotAKoreaderMount {
            path: mount.to_path_buf(),
        }
        .into());
    }
    plugin_dir(mount).ok_or_else(|| {
        PluginRefusal::NotAKoreaderMount {
            path: mount.to_path_buf(),
        }
        .into()
    })
}

type Manifest = BTreeMap<String, String>;

fn read_manifest(dir: &Path) -> Result<Option<(i64, Manifest)>> {
    let path = dir.join(MANIFEST_FILE);
    let Ok(src) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let lua = sandboxed_lua()?;
    let root = eval_table(&lua, &src, MANIFEST_FILE)?;
    let version = get_int(&root, "version").unwrap_or(0);
    let mut files = Manifest::new();
    if let Ok(mlua::Value::Table(t)) = root.get::<mlua::Value>("files") {
        for pair in t.pairs::<String, String>().flatten() {
            files.insert(pair.0, pair.1);
        }
    }
    Ok(Some((version, files)))
}

fn write_manifest(dir: &Path, version: i64, files: &Manifest) -> Result<()> {
    let mut s = String::from(
        "-- Written by bookmole. What it installed, and the sha256 of each\n\
         -- file as written, so that removing the plugin can remove exactly what\n\
         -- was put here and nothing a person added afterwards.\n\
         return {\n",
    );
    s.push_str(&format!("    version = {version},\n    files = {{\n"));
    for (name, hash) in files {
        s.push_str(&format!("        [\"{name}\"] = \"{hash}\",\n"));
    }
    s.push_str("    },\n}\n");
    std::fs::write(dir.join(MANIFEST_FILE), s)?;
    Ok(())
}

fn read_installed_version(dir: &Path) -> Result<Option<i64>> {
    let Ok(src) = std::fs::read_to_string(dir.join("_meta.lua")) else {
        return Ok(None);
    };
    let lua = sandboxed_lua()?;
    let root = eval_table(&lua, &src, "_meta.lua")?;
    Ok(get_int(&root, "version"))
}

/// One computer this reader is paired with. Carries no token: this type crosses into a DTO,
/// and the secret stays in the module-private `StoredPairing`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pairing {
    /// Who this reader is to that computer; each computer mints its own.
    pub device_id: String,
    /// The computer's hostname at install time; `None` for older entries.
    pub name: Option<String>,
    pub paired_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredPairing {
    device_id: String,
    token: String,
    name: Option<String>,
    paired_at: i64,
}

impl StoredPairing {
    fn public(&self) -> Pairing {
        Pairing {
            device_id: self.device_id.clone(),
            name: self.name.clone(),
            paired_at: Some(self.paired_at),
        }
    }
}

/// `s` as a Lua string literal. Escaping matters: a hostname with a quote would write an
/// unparseable `pairing.lua`.
fn lua_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\{:03}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Every computer `pairing.lua` names. Accepts the older flat single-computer table as a one-entry
/// list; entries missing an id or token are dropped, not repaired.
fn read_stored_pairings(dir: &Path) -> Result<Vec<StoredPairing>> {
    let Ok(src) = std::fs::read_to_string(dir.join(PAIRING_FILE)) else {
        return Ok(Vec::new());
    };
    let lua = sandboxed_lua()?;
    let root = eval_table(&lua, &src, PAIRING_FILE)?;

    let entry = |t: &mlua::Table| -> Option<StoredPairing> {
        let device_id = get_str(t, "device_id").filter(|s| !s.is_empty())?;
        let token = get_str(t, "token").filter(|s| !s.is_empty())?;
        Some(StoredPairing {
            device_id,
            token,
            name: get_str(t, "name").filter(|s| !s.is_empty()),
            paired_at: get_int(t, "paired_at").unwrap_or(0),
        })
    };

    if let Ok(mlua::Value::Table(list)) = root.get::<mlua::Value>("computers") {
        let mut out = Vec::new();
        for value in list.sequence_values::<mlua::Value>().flatten() {
            if let mlua::Value::Table(t) = value
                && let Some(p) = entry(&t)
            {
                out.push(p);
            }
        }
        return Ok(out);
    }
    Ok(entry(&root).into_iter().collect())
}

fn write_pairings(dir: &Path, computers: &[StoredPairing]) -> Result<()> {
    // No `host` or `port` in an entry: the learned endpoint belongs to the device and lives in
    // `endpoint.lua`.
    let mut body = String::from(
        "-- Written by bookmole: by the installer over USB, and by the plugin\n\
         -- on this device when a computer is linked or unlinked over wifi. Do not\n\
         -- edit.\n\
         --\n\
         -- A *list*, because a reader can be paired with more than one computer.\n\
         -- Each entry is one computer: the id this reader has to that computer,\n\
         -- the secret it proves itself with, and a name to show in the menu. A\n\
         -- writer changes its own entry and leaves every other one alone.\n\
         return {\n\
         \x20   computers = {\n",
    );
    for c in computers {
        body.push_str("        {\n");
        body.push_str(&format!(
            "            device_id = {},\n",
            lua_quote(&c.device_id)
        ));
        body.push_str(&format!(
            "            token     = {},\n",
            lua_quote(&c.token)
        ));
        if let Some(name) = &c.name {
            body.push_str(&format!("            name      = {},\n", lua_quote(name)));
        }
        body.push_str(&format!("            paired_at = {},\n", c.paired_at));
        body.push_str("        },\n");
    }
    body.push_str("    },\n}\n");
    std::fs::write(dir.join(PAIRING_FILE), body)?;
    Ok(())
}

/// One address of one computer, as `endpoint.lua` records it. A computer has several; their file
/// order is the order the reader tries them in, so never sort by `seen_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredEndpoint {
    device_id: String,
    host: String,
    /// The TCP port a listener last announced; `0` means unknown. Kept so both writers produce the same
    /// bytes.
    port: u16,
    seen_at: i64,
    /// A person typed this address into the reader; it is never dropped to make room.
    typed: bool,
}

/// Reads `endpoint.lua`; absent, unreadable and empty all mean no endpoints. A flat
/// `{ host, port, seen_at }` table is read as a one-entry list.
fn read_endpoints(dir: &Path) -> Result<Vec<StoredEndpoint>> {
    let path = dir.join(ENDPOINT_FILE);
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Ok(Vec::new());
    };
    let lua = sandboxed_lua()?;
    let Ok(root) = eval_table(&lua, &body, ENDPOINT_FILE) else {
        return Ok(Vec::new());
    };
    let address = |device_id: &str, t: &mlua::Table| -> Option<StoredEndpoint> {
        let host = get_str(t, "host").filter(|h| !h.is_empty())?;
        Some(StoredEndpoint {
            device_id: device_id.to_string(),
            host,
            port: get_int(t, "port").unwrap_or(0).clamp(0, 65535) as u16,
            seen_at: get_int(t, "seen_at").unwrap_or(0),
            typed: matches!(
                t.get::<mlua::Value>("typed"),
                Ok(mlua::Value::Boolean(true))
            ),
        })
    };
    let mut ids = Vec::new();
    for pair in root.pairs::<String, mlua::Value>().flatten() {
        if let (device_id, mlua::Value::Table(t)) = pair {
            ids.push((device_id, t));
        }
    }
    ids.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (device_id, t) in ids {
        if matches!(t.get::<mlua::Value>("host"), Ok(mlua::Value::String(_))) {
            out.extend(address(&device_id, &t));
            continue;
        }
        for value in t.sequence_values::<mlua::Value>().flatten() {
            if let mlua::Value::Table(one) = value {
                out.extend(address(&device_id, &one));
            }
        }
    }
    Ok(out)
}

/// Writes the file byte-for-byte as the plugin's `serialiseEndpoints` would; with two writers,
/// any difference makes each rewrite the other's output.
fn write_endpoints(dir: &Path, endpoints: &[StoredEndpoint]) -> Result<()> {
    let mut body = String::from(
        "-- Written by the bookmole plugin on this device.\n\
         -- Where each paired computer has been reached, most recent first, and\n\
         -- any address typed in by hand. Safe to delete: discovery rebuilds what\n\
         -- it learned, and a typed address can be typed again.\n\
         return {\n",
    );
    // Stable sort: computers come out in id order and each one's addresses keep their given (try)
    // order.
    let mut sorted: Vec<&StoredEndpoint> = endpoints.iter().collect();
    sorted.sort_by(|a, b| a.device_id.cmp(&b.device_id));
    let mut open: Option<&str> = None;
    for e in sorted {
        if open != Some(e.device_id.as_str()) {
            if open.is_some() {
                body.push_str("    },\n");
            }
            body.push_str(&format!("    [{}] = {{\n", lua_quote(&e.device_id)));
            open = Some(e.device_id.as_str());
        }
        body.push_str(&format!(
            "        {{ host = {}, port = {}, seen_at = {}{} }},\n",
            lua_quote(&e.host),
            e.port,
            e.seen_at,
            if e.typed { ", typed = true" } else { "" }
        ));
    }
    if open.is_some() {
        body.push_str("    },\n");
    }
    body.push_str("}\n");
    std::fs::write(dir.join(ENDPOINT_FILE), body)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanAddress {
    pub address: std::net::Ipv4Addr,
    /// The interface's own broadcast address, which is not always `x.y.z.255`.
    pub broadcast: Option<std::net::Ipv4Addr>,
}

/// Addresses on interfaces that are up, can broadcast, and are neither loopback nor point-to-point
/// (a VPN tunnel is not a place a reader can be). `None` means the platform cannot say, not no
/// network.
#[cfg(unix)]
fn lan_interfaces() -> Option<Vec<LanAddress>> {
    use nix::net::if_::InterfaceFlags as F;
    let mut out = Vec::new();
    for ifa in nix::ifaddrs::getifaddrs().ok()? {
        if !ifa.flags.contains(F::IFF_UP | F::IFF_BROADCAST)
            || ifa.flags.intersects(F::IFF_LOOPBACK | F::IFF_POINTOPOINT)
        {
            continue;
        }
        let v4 = |a: &Option<nix::sys::socket::SockaddrStorage>| {
            a.as_ref()
                .and_then(|a| a.as_sockaddr_in())
                .map(nix::sys::socket::SockaddrIn::ip)
        };
        if let Some(address) = v4(&ifa.address) {
            out.push(LanAddress {
                address,
                broadcast: v4(&ifa.broadcast),
            });
        }
    }
    Some(out)
}

#[cfg(not(unix))]
fn lan_interfaces() -> Option<Vec<LanAddress>> {
    None
}

/// The source address the routing table would pick. A UDP `connect` only chooses a route; nothing
/// is sent to the unroutable TEST-NET-1 literal.
fn routed_address() -> Option<std::net::Ipv4Addr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("192.0.2.1:9").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        // IPv4 only: the plugin's discovery derives a `/24` broadcast from dotted decimals over `udp4`.
        _ => None,
    }
}

/// Picks which of `networks` to tell a reader about: the routed address if it is on a network,
/// else a private-range network address (the route may be a VPN's), else none. If the interfaces
/// could not be listed (`None`), the routed address.
fn choose_lan_address(
    networks: Option<&[LanAddress]>,
    routed: Option<std::net::Ipv4Addr>,
) -> Option<std::net::Ipv4Addr> {
    let Some(networks) = networks else {
        return routed;
    };
    if let Some(routed) = routed
        && networks.iter().any(|n| n.address == routed)
    {
        return Some(routed);
    }
    networks
        .iter()
        .map(|n| n.address)
        // Link-local (169.254/16) means DHCP never answered, so it ranks last.
        .min_by_key(|a| (!a.is_private(), a.is_link_local(), *a))
}

/// This computer's address on the network a reader would use; `None` means do not stamp one.
/// Read by the facade and passed into `install`, so tests do not stamp the host's real address.
pub fn this_lan_address() -> Option<String> {
    choose_lan_address(lan_interfaces().as_deref(), routed_address()).map(|a| a.to_string())
}

/// Where to send a datagram for every nearby reader: the limited broadcast plus each network's own,
/// since the former leaves by the default route (possibly a VPN) and some access points drop one.
pub fn broadcast_addresses() -> Vec<std::net::Ipv4Addr> {
    let mut out = vec![std::net::Ipv4Addr::BROADCAST];
    for network in lan_interfaces().unwrap_or_default() {
        if let Some(b) = network.broadcast
            && !out.contains(&b)
        {
            out.push(b);
        }
    }
    out
}

pub fn this_computer() -> Option<String> {
    let name = gethostname::gethostname().to_string_lossy().into_owned();
    let name = name.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// `n` bytes of system randomness, hex-encoded (16 for a device id, 32 for a pairing token).
pub fn mint_id(n: usize) -> Result<String> {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes)
        .map_err(|e| crate::error::EngineError::Other(format!("no system randomness: {e}")))?;
    use std::fmt::Write as _;
    Ok(bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    }))
}

pub fn inspect(mount: &Path) -> Result<PluginStatus> {
    let dir = check_mount(mount)?;
    let mut status = PluginStatus {
        mount: mount.to_path_buf(),
        plugin_dir: dir.clone(),
        installed: false,
        installed_version: None,
        our_version: PLUGIN_VERSION,
        paired: false,
        device_id: None,
        pairings: Vec::new(),
        modified: Vec::new(),
        unrecognised: Vec::new(),
    };
    if !dir.is_dir() {
        return Ok(status);
    }
    status.installed = dir.join("main.lua").is_file();
    status.installed_version = read_installed_version(&dir)?;
    status.pairings = read_stored_pairings(&dir)?
        .iter()
        .map(StoredPairing::public)
        .collect();
    status.device_id = status.pairings.first().map(|p| p.device_id.clone());

    if let Some((_, manifest)) = read_manifest(&dir)? {
        let mut present: BTreeMap<String, PathBuf> = BTreeMap::new();
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if UNHASHED_FILES.contains(&name.as_str()) || is_os_litter(&name) {
                continue;
            }
            present.insert(name, entry.path());
        }
        for (name, expected) in &manifest {
            // A missing file of ours is not an obstruction (reinstall restores it); only changed bytes are.
            if let Some(path) = present.remove(name)
                && sha256_of(&path)? != *expected
            {
                status.modified.push(name.clone());
            }
        }
        status.unrecognised = present.into_keys().collect();
    }
    Ok(status)
}

fn refuse_if_obstructed(status: &PluginStatus) -> Result<()> {
    if !status.modified.is_empty() {
        return Err(PluginRefusal::Modified {
            paths: status.modified.clone(),
        }
        .into());
    }
    if !status.unrecognised.is_empty() {
        return Err(PluginRefusal::Unrecognised {
            paths: status.unrecognised.clone(),
        }
        .into());
    }
    Ok(())
}

/// Installs or upgrades the plugin and writes the pairing. The caller supplies `device_id`, `token`
/// and `paired_at` (the stored time on an upgrade) so the database and `pairing.lua` agree.
/// Only the entry carrying `device_id` is rewritten; other computers' entries are kept.
pub fn install(
    mount: &Path,
    device_id: &str,
    token: &str,
    paired_at: i64,
    name: Option<&str>,
    endpoint: Option<&str>,
) -> Result<InstallReport> {
    let status = inspect(mount)?;
    refuse_if_obstructed(&status)?;
    if let Some(installed) = status.installed_version
        && installed > PLUGIN_VERSION
    {
        return Err(PluginRefusal::NewerAlreadyInstalled {
            installed,
            ours: PLUGIN_VERSION,
        }
        .into());
    }

    let dir = &status.plugin_dir;
    std::fs::create_dir_all(dir)?;

    let mut manifest = Manifest::new();
    let mut written = Vec::new();
    for (name, body) in FILES {
        let path = dir.join(name);
        std::fs::write(&path, body)?;
        manifest.insert((*name).to_string(), sha256_of(&path)?);
        written.push((*name).to_string());
    }
    write_manifest(dir, PLUGIN_VERSION, &manifest)?;

    // Read stored pairings back off the device: the public `Pairing` has no token, and rewriting from
    // it would blank other computers' secrets.
    let mut computers = read_stored_pairings(dir)?;
    let ours = StoredPairing {
        device_id: device_id.to_string(),
        token: token.to_string(),
        name: name.map(str::to_string),
        paired_at,
    };
    match computers.iter().position(|c| c.device_id == device_id) {
        // In place, so the menu order stays stable across upgrades.
        Some(i) => computers[i] = ours,
        None => computers.push(ours),
    }
    write_pairings(dir, &computers)?;
    written.push(MANIFEST_FILE.to_string());
    written.push(PAIRING_FILE.to_string());

    // Stamp this computer's address first among its endpoints, keeping the rest and any typed one. A
    // stamped address makes the first push unicast, which works through a Kindle's firewall; it is
    // only a hint the device overwrites, so a missing address is not an error.
    if let Some(host) = endpoint.filter(|h| !h.is_empty()) {
        let mut endpoints = read_endpoints(dir)?;
        let known = endpoints
            .iter()
            .position(|e| e.device_id == device_id && e.host == host)
            .map(|i| endpoints.remove(i));
        let ours = known.unwrap_or_else(|| StoredEndpoint {
            device_id: device_id.to_string(),
            host: host.to_string(),
            port: 0,
            seen_at: paired_at,
            typed: false,
        });
        let first = endpoints
            .iter()
            .position(|e| e.device_id == device_id)
            .unwrap_or(endpoints.len());
        endpoints.insert(first, ours);
        write_endpoints(dir, &endpoints)?;
        written.push(ENDPOINT_FILE.to_string());
    }
    written.sort();
    // Last, after every write, so the OS shadow files those writes left are swept too.
    sweep_os_litter(dir);

    Ok(InstallReport {
        plugin_dir: dir.clone(),
        device_id: device_id.to_string(),
        version: PLUGIN_VERSION,
        written,
        upgraded_from: status.installed_version.filter(|_| status.installed),
    })
}

/// Removes exactly what we installed. Refuses if the directory holds anything we did not write or
/// a file of ours was edited: leaving the plugin is recoverable, deleting a person's work is not.
pub fn uninstall(mount: &Path) -> Result<UninstallReport> {
    let status = inspect(mount)?;
    let dir = status.plugin_dir.clone();
    let removed_pairings: Vec<String> = status
        .pairings
        .iter()
        .map(|p| p.device_id.clone())
        .collect();
    if !dir.is_dir() {
        return Ok(UninstallReport {
            plugin_dir: dir,
            removed: Vec::new(),
            forgot_device: status.device_id,
            removed_pairings,
        });
    }
    let Some((_, manifest)) = read_manifest(&dir)? else {
        return Err(PluginRefusal::NoManifest { path: dir }.into());
    };
    refuse_if_obstructed(&status)?;

    let mut removed = Vec::new();
    for name in manifest.keys() {
        let path = dir.join(name);
        if path.is_file() {
            std::fs::remove_file(&path)?;
            removed.push(name.clone());
        }
    }
    // The device wrote `endpoint.lua` and nobody else will remove it.
    for name in UNHASHED_FILES {
        let path = dir.join(name);
        if path.is_file() {
            std::fs::remove_file(&path)?;
            removed.push(name.to_string());
        }
    }
    sweep_os_litter(&dir);
    // `remove_dir`, not `remove_dir_all`: anything still inside keeps the directory, backing up the
    // refusals above.
    let _ = std::fs::remove_dir(&dir);
    removed.sort();

    Ok(UninstallReport {
        plugin_dir: dir,
        removed,
        forgot_device: status.device_id,
        removed_pairings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::tests::install as fake_reader;

    fn mount() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        fake_reader(tmp.path());
        tmp
    }

    fn dir_of(mount: &Path) -> PathBuf {
        plugin_dir(mount).unwrap()
    }

    fn snapshot(root: &Path) -> BTreeMap<String, String> {
        fn walk(base: &Path, dir: &Path, skip: &Path, out: &mut BTreeMap<String, String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path == skip {
                    continue;
                }
                if path.is_dir() {
                    walk(base, &path, skip, out);
                } else {
                    let rel = path.strip_prefix(base).unwrap().display().to_string();
                    out.insert(rel, sha256_of(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &dir_of(root), &mut out);
        out
    }

    fn install_here(m: &Path) -> InstallReport {
        install(m, "dev-1", "tok-1", 1_700_000_000, Some("desk"), None).unwrap()
    }

    fn install_here_from(m: &Path, host: &str) -> InstallReport {
        install(m, "dev-1", "tok-1", 1_700_000_000, Some("desk"), Some(host)).unwrap()
    }

    fn refusal(e: crate::error::EngineError) -> PluginRefusal {
        match e {
            crate::error::EngineError::PluginRefused(r) => r,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_install_writes_our_directory_and_touches_nothing_else() {
        let tmp = mount();
        let before = snapshot(tmp.path());

        let report = install_here(tmp.path());

        assert_eq!(
            snapshot(tmp.path()),
            before,
            "an install changed something outside its own directory"
        );
        assert!(dir_of(tmp.path()).join("main.lua").is_file());
        assert_eq!(
            report.written,
            vec!["_meta.lua", "installed.lua", "main.lua", "pairing.lua"],
            "with no address to offer, no endpoint is written"
        );
        assert_eq!(report.upgraded_from, None);
    }

    #[test]
    fn an_install_stamps_where_this_computer_can_be_reached() {
        let tmp = mount();
        let report = install_here_from(tmp.path(), "192.0.2.63");
        let dir = dir_of(tmp.path());

        assert!(report.written.contains(&ENDPOINT_FILE.to_string()));
        assert_eq!(
            read_endpoints(&dir).unwrap(),
            vec![StoredEndpoint {
                device_id: "dev-1".into(),
                host: "192.0.2.63".into(),
                port: 0,
                seen_at: 1_700_000_000,
                typed: false,
            }]
        );

        let status = inspect(tmp.path()).unwrap();
        assert!(status.unrecognised.is_empty(), "{status:?}");
        assert!(status.modified.is_empty(), "{status:?}");
        assert!(!status.is_obstructed());
    }

    #[test]
    fn both_writers_of_the_endpoint_cache_agree_byte_for_byte() {
        let tmp = mount();
        install_here_from(tmp.path(), "192.0.2.63");
        let ours = std::fs::read_to_string(dir_of(tmp.path()).join(ENDPOINT_FILE)).unwrap();

        let theirs = run_plugin_lua(
            r#"return RB.serialiseEndpoints({
                   ["dev-1"] = { { host = "192.0.2.63", port = 0, seen_at = 1700000000 } },
               })"#,
        )
        .unwrap();

        assert_eq!(ours.trim_end(), theirs.trim_end());

        // Given out of id order: sorting computers is each writer's job; addresses keep their given order.
        let dir = tempfile::tempdir().unwrap();
        let e = |id: &str, host: &str, port: u16, seen_at: i64, typed: bool| StoredEndpoint {
            device_id: id.into(),
            host: host.into(),
            port,
            seen_at,
            typed,
        };
        write_endpoints(
            dir.path(),
            &[
                e("bbb", "203.0.113.9", 51001, 20, false),
                e("aaa", "desk.lan", 0, 0, true),
                e("aaa", "192.0.2.20", 51000, 10, false),
                e("aaa", "203.0.113.42", 0, 5, true),
            ],
        )
        .unwrap();
        let ours = std::fs::read_to_string(dir.path().join(ENDPOINT_FILE)).unwrap();
        let theirs = run_plugin_lua(
            r#"return RB.serialiseEndpoints({
                   bbb = { { host = "203.0.113.9", port = 51001, seen_at = 20 } },
                   aaa = {
                       { host = "desk.lan", port = 0, seen_at = 0, typed = true },
                       { host = "192.0.2.20", port = 51000, seen_at = 10 },
                       { host = "203.0.113.42", port = 0, seen_at = 5, typed = true },
                   },
               })"#,
        )
        .unwrap();
        assert_eq!(ours, theirs);

        let back = read_endpoints(dir.path()).unwrap();
        assert_eq!(
            back.iter()
                .map(|e| format!(
                    "{}@{}{}",
                    e.device_id,
                    e.host,
                    if e.typed { "*" } else { "" }
                ))
                .collect::<Vec<_>>(),
            [
                "aaa@desk.lan*",
                "aaa@192.0.2.20",
                "aaa@203.0.113.42*",
                "bbb@203.0.113.9"
            ]
        );
        let read_by_plugin = run_plugin_lua(&format!(
            "local got = RB.parseEndpoints(load({ours:?})())
             local out = {{}}
             for _, a in ipairs(got.aaa) do
                 out[#out + 1] = a.host .. (a.typed and '*' or '')
             end
             return table.concat(out, ' ') .. ' | ' .. got.bbb[1].host"
        ))
        .unwrap();
        assert_eq!(
            read_by_plugin,
            "desk.lan* 192.0.2.20 203.0.113.42* | 203.0.113.9"
        );
    }

    #[test]
    fn an_install_puts_its_address_first_and_keeps_the_others() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        std::fs::write(
            dir.join(ENDPOINT_FILE),
            r#"return {
                ["dev-1"] = {
                    { host = "198.51.100.7", port = 51000, seen_at = 30 },
                    { host = "desk.lan", port = 0, seen_at = 0, typed = true },
                    { host = "192.0.2.63", port = 51003, seen_at = 20 },
                },
            }"#,
        )
        .unwrap();

        install_here_from(tmp.path(), "192.0.2.63");
        let hosts = |dir: &Path| {
            read_endpoints(dir)
                .unwrap()
                .into_iter()
                .map(|e| (e.host, e.port, e.typed))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            hosts(&dir),
            [
                ("192.0.2.63".to_string(), 51003, false),
                ("198.51.100.7".to_string(), 51000, false),
                ("desk.lan".to_string(), 0, true),
            ]
        );

        install_here_from(tmp.path(), "203.0.113.4");
        assert_eq!(
            hosts(&dir).iter().map(|h| h.0.as_str()).collect::<Vec<_>>(),
            ["203.0.113.4", "192.0.2.63", "198.51.100.7", "desk.lan"]
        );
    }

    #[test]
    fn install_then_uninstall_leaves_the_reader_as_it_found_it() {
        let tmp = mount();
        let before = snapshot(tmp.path());

        install_here(tmp.path());
        let report = uninstall(tmp.path()).unwrap();

        assert_eq!(snapshot(tmp.path()), before);
        assert!(
            !dir_of(tmp.path()).exists(),
            "the directory itself is part of what we put there"
        );
        assert_eq!(report.forgot_device.as_deref(), Some("dev-1"));
    }

    #[test]
    fn an_ordinary_directory_is_refused() {
        let plain = tempfile::tempdir().unwrap();
        let err = refusal(install(plain.path(), "d", "t", 0, None, None).unwrap_err());
        assert!(matches!(err, PluginRefusal::NotAKoreaderMount { .. }));
        assert_eq!(
            std::fs::read_dir(plain.path()).unwrap().count(),
            0,
            "a refusal must not create anything"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_reader_is_refused() {
        let tmp = mount();
        let link_home = tempfile::tempdir().unwrap();
        let link = link_home.path().join("KOBOeReader");
        std::os::unix::fs::symlink(tmp.path(), &link).unwrap();

        assert!(is_koreader_mount(&link));
        let err = refusal(install(&link, "d", "t", 0, None, None).unwrap_err());
        assert!(matches!(err, PluginRefusal::MountIsASymlink { .. }));
        assert!(!dir_of(tmp.path()).exists());
    }

    #[test]
    fn a_newer_plugin_is_not_overwritten() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());

        let newer = format!("return {{ version = {} }}\n", PLUGIN_VERSION + 1);
        std::fs::write(dir.join("_meta.lua"), &newer).unwrap();
        let mut manifest = read_manifest(&dir).unwrap().unwrap().1;
        manifest.insert(
            "_meta.lua".into(),
            sha256_of(&dir.join("_meta.lua")).unwrap(),
        );
        write_manifest(&dir, PLUGIN_VERSION + 1, &manifest).unwrap();

        let err = refusal(install(tmp.path(), "d", "t", 0, None, None).unwrap_err());
        assert_eq!(
            err,
            PluginRefusal::NewerAlreadyInstalled {
                installed: PLUGIN_VERSION + 1,
                ours: PLUGIN_VERSION,
            }
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("_meta.lua")).unwrap(),
            newer,
            "the refusal wrote anyway"
        );
    }

    #[test]
    fn a_file_the_user_edited_is_never_overwritten_and_never_deleted() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        let theirs = "-- I fixed a bug in your plugin\n";
        std::fs::write(dir.join("main.lua"), theirs).unwrap();

        let status = inspect(tmp.path()).unwrap();
        assert_eq!(status.modified, vec!["main.lua"]);
        assert!(status.is_obstructed());

        for err in [
            install(tmp.path(), "d", "t", 0, None, None).unwrap_err(),
            uninstall(tmp.path()).unwrap_err(),
        ] {
            assert!(matches!(refusal(err), PluginRefusal::Modified { .. }));
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("main.lua")).unwrap(),
            theirs
        );
    }

    #[test]
    fn something_we_did_not_write_blocks_both_operations() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        std::fs::write(dir.join("notes.txt"), "mine\n").unwrap();

        let status = inspect(tmp.path()).unwrap();
        assert_eq!(status.unrecognised, vec!["notes.txt"]);

        let err = refusal(uninstall(tmp.path()).unwrap_err());
        assert!(matches!(err, PluginRefusal::Unrecognised { .. }));
        assert!(dir.join("notes.txt").is_file());
        assert!(dir.join("main.lua").is_file(), "nothing was removed");
    }

    #[test]
    fn what_macos_leaves_beside_our_files_obstructs_nothing() {
        let tmp = mount();
        install_here_from(tmp.path(), "192.0.2.63");
        let dir = dir_of(tmp.path());
        let litter = [
            "._main.lua",
            ".__meta.lua",
            "._installed.lua",
            "._pairing.lua",
            "._endpoint.lua",
            ".DS_Store",
        ];
        let drop_litter = || {
            for name in litter {
                std::fs::write(dir.join(name), b"\0\x05\x16\x07 apple double").unwrap();
            }
        };

        drop_litter();
        let status = inspect(tmp.path()).unwrap();
        assert!(status.unrecognised.is_empty(), "{status:?}");
        assert!(!status.is_obstructed());

        install_here_from(tmp.path(), "192.0.2.63");
        for name in litter {
            assert!(!dir.join(name).exists(), "{name} survived an install");
        }

        drop_litter();
        uninstall(tmp.path()).unwrap();
        assert!(
            !dir.exists(),
            "the shadows must not keep the directory alive"
        );
    }

    #[test]
    fn a_shadow_of_somebody_elses_file_is_still_somebody_elses() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        for name in [
            "._notes.txt",
            "._",
            "._main.lua.bak",
            ".hidden",
            "main.lua~",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
            let status = inspect(tmp.path()).unwrap();
            assert_eq!(status.unrecognised, vec![name.to_string()], "{name}");
            assert!(matches!(
                refusal(
                    install(tmp.path(), "dev-1", "tok-1", 1_700_000_000, None, None).unwrap_err()
                ),
                PluginRefusal::Unrecognised { .. }
            ));
            assert!(matches!(
                refusal(uninstall(tmp.path()).unwrap_err()),
                PluginRefusal::Unrecognised { .. }
            ));
            assert!(dir.join(name).exists(), "{name} must never be deleted");
            std::fs::remove_file(dir.join(name)).unwrap();
        }
    }

    #[test]
    fn a_plugin_with_no_manifest_is_not_ours_to_remove() {
        let tmp = mount();
        let dir = dir_of(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.lua"), "-- somebody else's\n").unwrap();

        let err = refusal(uninstall(tmp.path()).unwrap_err());
        assert!(matches!(err, PluginRefusal::NoManifest { .. }));
        assert!(dir.join("main.lua").is_file());
    }

    #[test]
    fn uninstalling_a_reader_that_never_had_it_is_not_an_error() {
        let tmp = mount();
        let report = uninstall(tmp.path()).unwrap();
        assert!(report.removed.is_empty());
        assert_eq!(report.forgot_device, None);
    }

    #[test]
    fn an_upgrade_reports_what_it_replaced() {
        let tmp = mount();
        install_here(tmp.path());
        let again = install(
            tmp.path(),
            "dev-1",
            "tok-1",
            1_700_000_001,
            Some("desk"),
            None,
        )
        .unwrap();
        assert_eq!(again.upgraded_from, Some(PLUGIN_VERSION));
        assert_eq!(
            inspect(tmp.path()).unwrap().device_id.as_deref(),
            Some("dev-1")
        );
    }

    /// `_meta.lua` must never `require("gettext")`: the sidecar sandbox cannot load it.
    #[test]
    fn the_shipped_meta_is_readable_by_the_sidecar_sandbox() {
        let lua = sandboxed_lua().unwrap();
        let meta = FILES.iter().find(|(n, _)| *n == "_meta.lua").unwrap().1;
        let t = eval_table(&lua, meta, "_meta.lua").unwrap();
        assert_eq!(get_int(&t, "version"), Some(PLUGIN_VERSION));
        // Comment lines are skipped: the file's own header mentions `require`.
        let code = meta
            .lines()
            .filter(|l| !l.trim_start().starts_with("--"))
            .collect::<String>();
        assert!(
            !code.contains("require("),
            "a `require` in _meta.lua makes the installed version unreadable"
        );
    }

    #[test]
    fn the_pairing_we_write_reads_back() {
        let tmp = mount();
        install(
            tmp.path(),
            "abc123",
            "s3cret",
            1_700_000_000,
            Some("desk"),
            None,
        )
        .unwrap();
        let dir = dir_of(tmp.path());
        let src = std::fs::read_to_string(dir.join(PAIRING_FILE)).unwrap();

        let read = read_stored_pairings(&dir).unwrap();
        assert_eq!(
            read,
            vec![StoredPairing {
                device_id: "abc123".into(),
                token: "s3cret".into(),
                name: Some("desk".into()),
                paired_at: 1_700_000_000,
            }]
        );
        assert!(
            !src.contains("host") && !src.contains("port"),
            "no endpoint is written until there is a listener to name"
        );
    }

    #[test]
    fn a_second_computer_does_not_steal_the_reader_from_the_first() {
        let tmp = mount();
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_700_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        install(
            tmp.path(),
            "dev-b",
            "tok-b",
            1_700_000_100,
            Some("desktop"),
            None,
        )
        .unwrap();

        let dir = dir_of(tmp.path());
        let stored = read_stored_pairings(&dir).unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|c| c.device_id.as_str())
                .collect::<Vec<_>>(),
            vec!["dev-a", "dev-b"],
        );
        assert_eq!(
            stored[0].token, "tok-a",
            "the first computer's secret survived the second computer's install"
        );
        assert_eq!(stored[0].name.as_deref(), Some("laptop"));
        assert_eq!(stored[0].paired_at, 1_700_000_000);

        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_700_000_000,
            Some("laptop2"),
            None,
        )
        .unwrap();
        let again = read_stored_pairings(&dir).unwrap();
        assert_eq!(
            again
                .iter()
                .map(|c| c.device_id.as_str())
                .collect::<Vec<_>>(),
            vec!["dev-a", "dev-b"],
        );
        assert_eq!(again[0].name.as_deref(), Some("laptop2"));
        assert_eq!(again[1].token, "tok-b");
    }

    #[test]
    fn the_version_one_flat_file_is_read_as_one_computer() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        std::fs::write(
            dir.join(PAIRING_FILE),
            "-- Written by bookmole over USB. Do not edit.\n\
             return {\n\
             \x20   device_id = \"old-id\",\n\
             \x20   token     = \"old-token\",\n\
             \x20   paired_at = 1690000000,\n\
             }\n",
        )
        .unwrap();

        let status = inspect(tmp.path()).unwrap();
        assert_eq!(status.device_id.as_deref(), Some("old-id"));
        assert_eq!(status.pairings.len(), 1);
        assert_eq!(status.pairings[0].name, None, "version 1 wrote no name");
        assert_eq!(status.pairings[0].paired_at, Some(1_690_000_000));

        install(
            tmp.path(),
            "old-id",
            "old-token",
            1_690_000_000,
            Some("desk"),
            None,
        )
        .unwrap();
        let stored = read_stored_pairings(&dir).unwrap();
        assert_eq!(stored.len(), 1, "the same computer, in the list shape");
        assert_eq!(stored[0].token, "old-token");
        assert_eq!(stored[0].paired_at, 1_690_000_000);
    }

    #[test]
    fn an_entry_missing_its_id_or_its_token_is_not_a_computer() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        std::fs::write(
            dir.join(PAIRING_FILE),
            "return { computers = {\n\
             \x20 { device_id = \"a\", token = \"t\" },\n\
             \x20 { device_id = \"b\" },\n\
             \x20 { token = \"t\" },\n\
             \x20 { device_id = \"\", token = \"t\" },\n\
             \x20 \"not a table\",\n\
             } }\n",
        )
        .unwrap();
        let stored = read_stored_pairings(&dir).unwrap();
        assert_eq!(
            stored
                .iter()
                .map(|c| c.device_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a"],
        );
    }

    #[test]
    fn a_name_that_could_break_the_file_is_escaped() {
        let tmp = mount();
        let hostile = "o\"liver's\\box\nnewline";
        install(
            tmp.path(),
            "dev-1",
            "tok-1",
            1_700_000_000,
            Some(hostile),
            None,
        )
        .unwrap();
        let stored = read_stored_pairings(&dir_of(tmp.path())).unwrap();
        assert_eq!(stored[0].name.as_deref(), Some(hostile));
    }

    #[test]
    fn the_endpoint_the_device_writes_blocks_neither_operation() {
        let tmp = mount();
        install_here(tmp.path());
        let dir = dir_of(tmp.path());
        let learned = "return { [\"dev-1\"] = { host = \"192.0.2.20\", port = 51861 } }\n";
        std::fs::write(dir.join(ENDPOINT_FILE), learned).unwrap();

        let status = inspect(tmp.path()).unwrap();
        assert!(status.unrecognised.is_empty(), "{status:?}");
        assert!(!status.is_obstructed());

        install_here(tmp.path());
        assert_eq!(
            std::fs::read_to_string(dir.join(ENDPOINT_FILE)).unwrap(),
            learned
        );

        install(
            tmp.path(),
            "dev-2",
            "tok-2",
            1_700_000_500,
            Some("laptop"),
            Some("192.0.2.63"),
        )
        .unwrap();
        let after = read_endpoints(&dir).unwrap();
        assert_eq!(
            after,
            vec![
                StoredEndpoint {
                    device_id: "dev-1".into(),
                    host: "192.0.2.20".into(),
                    port: 51861,
                    seen_at: 0,
                    typed: false,
                },
                StoredEndpoint {
                    device_id: "dev-2".into(),
                    host: "192.0.2.63".into(),
                    port: 0,
                    seen_at: 1_700_000_500,
                    typed: false,
                },
            ]
        );

        let report = uninstall(tmp.path()).unwrap();
        assert!(report.removed.contains(&ENDPOINT_FILE.to_string()));
        assert!(!dir.exists());
    }

    /// Runs the plugin's pure menu logic against the installer's bytes. `require` is stubbed with a
    /// table that answers every index and call with itself.
    fn run_plugin_lua(script: &str) -> mlua::Result<String> {
        let lua = mlua::Lua::new();
        let stub = lua.create_table()?;
        let meta = lua.create_table()?;
        let s = stub.clone();
        meta.set(
            "__index",
            lua.create_function(move |_, (_t, _k): (mlua::Table, mlua::Value)| Ok(s.clone()))?,
        )?;
        let s = stub.clone();
        meta.set(
            "__call",
            lua.create_function(move |_, _: mlua::MultiValue| Ok(s.clone()))?,
        )?;
        stub.set_metatable(Some(meta));

        let gettext = lua.create_function(|_, s: String| Ok(s))?;
        let ffi_util = lua.create_table()?;
        ffi_util.set(
            "template",
            lua.create_function(|_, (fmt, args): (String, mlua::Variadic<String>)| {
                let mut out = fmt;
                for (i, a) in args.iter().enumerate() {
                    out = out.replace(&format!("%{}", i + 1), a);
                }
                Ok(out)
            })?,
        )?;

        let s = stub.clone();
        lua.globals().set(
            "require",
            lua.create_function(move |lua, name: String| {
                if let Ok(mlua::Value::Table(fakes)) =
                    lua.globals().get::<mlua::Value>("__rb_modules")
                    && let Ok(fake) = fakes.get::<mlua::Value>(name.as_str())
                    && !fake.is_nil()
                {
                    return Ok(fake);
                }
                match name.as_str() {
                    "gettext" => lua.globals().get::<mlua::Value>("__rb_gettext"),
                    "ffi/util" => lua.globals().get::<mlua::Value>("__rb_ffi_util"),
                    "ui/widget/container/widgetcontainer" => {
                        lua.globals().get::<mlua::Value>("__rb_widget")
                    }
                    "ffi/sha2" => lua.globals().get::<mlua::Value>("__rb_sha2"),
                    _ => Ok(mlua::Value::Table(s.clone())),
                }
            })?,
        )?;
        lua.globals().set("__rb_gettext", gettext)?;
        lua.globals().set("__rb_ffi_util", ffi_util)?;

        // `ffi/sha2` uses the desktop's own HMAC so the reader's signing is tested against the verifier.
        let sha2 = lua.create_table()?;
        sha2.set("sha256", lua.create_function(|_, s: String| Ok(s))?)?;
        sha2.set(
            "hmac",
            lua.create_function(|_, (_hash, key, msg): (mlua::Value, String, String)| {
                Ok(crate::wireless::mac(&key, &msg))
            })?,
        )?;
        lua.globals().set("__rb_sha2", sha2)?;
        lua.globals().set("__rb_modules", lua.create_table()?)?;

        // `extend`/`new` are real: under the swallow-everything stub `self.window_server` would never be
        // nil, so the menu under test would differ from the device's.
        let widget: mlua::Value = lua
            .load(
                r#"local W = {}
                   function W:extend(t)
                       t = t or {}
                       setmetatable(t, { __index = self })
                       return t
                   end
                   function W:new(o)
                       o = o or {}
                       setmetatable(o, { __index = self })
                       if o.init then o:init() end
                       return o
                   end
                   return W"#,
            )
            .eval()?;
        lua.globals().set("__rb_widget", widget)?;

        let main = FILES.iter().find(|(n, _)| *n == "main.lua").unwrap().1;
        let module: mlua::Table = lua.load(main).set_name("main.lua").eval()?;
        lua.globals().set("RB", module)?;
        // Stringified inside the VM's lifetime: an `mlua::Value` outliving its `Lua` panics on read.
        lua.load(script).eval::<mlua::Value>().map(|v| match v {
            mlua::Value::String(s) => s.to_string_lossy().to_string(),
            mlua::Value::Integer(i) => i.to_string(),
            other => format!("{other:?}"),
        })
    }

    #[test]
    fn every_shipped_lua_file_compiles() {
        let lua = mlua::Lua::new();
        for (name, body) in FILES {
            lua.load(*body)
                .set_name(*name)
                .into_function()
                .unwrap_or_else(|e| panic!("{name} does not compile: {e}"));
        }
    }

    /// Instantiates the plugin and asks for its menu: a throw while KOReader builds the menu hides it
    /// entirely, and compiling the files does not catch that.
    #[test]
    fn the_menu_builds_on_a_paired_reader() {
        let tmp = mount();
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_700_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        let dir = dir_of(tmp.path());

        let labels = run_plugin_lua(&format!(
            r#"RB.path = {path:?}
               RB.ui = require("anything")
               RB:init()
               local items = {{}}
               RB:addToMainMenu(items)
               local entry = items.bookmole
               local out = {{ entry.text .. "/" .. entry.sorting_hint }}
               for _, it in ipairs(entry.sub_item_table_func()) do
                   out[#out + 1] = it.text or it.text_func()
                   if it.sub_item_table_func then
                       for _, computer in ipairs(it.sub_item_table_func()) do
                           out[#out + 1] = "> " .. computer.text
                           for _, line in ipairs(computer.sub_item_table_func()) do
                               out[#out + 1] = ">> " .. line.text
                           end
                       end
                   end
               end
               return table.concat(out, " | ")"#,
            path = dir.to_string_lossy()
        ))
        .unwrap();

        assert_eq!(
            labels,
            concat!(
                "bookmole/tools | Send highlights to laptop",
                " | Let a computer fetch highlights",
                " | Linked computers | > laptop | >> Add an address | >> Unlink laptop",
                " | Link a computer over wifi | How sending and fetching work"
            ),
            "the menu a paired reader draws"
        );
    }

    #[test]
    fn the_menu_builds_on_an_unpaired_reader() {
        let tmp = mount();
        let dir = dir_of(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        let labels = run_plugin_lua(&format!(
            r#"RB.path = {path:?}
               RB.ui = require("anything")
               RB:init()
               local items = {{}}
               RB:addToMainMenu(items)
               local out = {{}}
               for _, it in ipairs(items.bookmole.sub_item_table_func()) do
                   out[#out + 1] = it.text or it.text_func()
               end
               return table.concat(out, " | ")"#,
            path = dir.to_string_lossy()
        ))
        .unwrap();
        assert_eq!(
            labels,
            "Link a computer over wifi | How sending and fetching work"
        );
    }

    /// The firewall commands are asserted as strings, since they never run in CI; a nil port or an
    /// add/delete mismatch would leave a bad rule on a reader.
    #[test]
    fn the_firewall_rules_are_the_ones_koreaders_own_plugins_use() {
        let rules = run_plugin_lua(
            r#"return table.concat({
                   RB.firewallRule("A", "INPUT", "udp", 61862),
                   RB.firewallRule("D", "OUTPUT", "tcp", 8080),
                   tostring(RB.firewallRule("A", "INPUT", "udp", nil)),
                   tostring(RB.firewallRule("X", "INPUT", "udp", 1)),
                   tostring(RB.firewallRule("A", "FORWARD", "udp", 1)),
                   tostring(RB.firewallRule("A", "INPUT", "icmp", 1)),
                   tostring(RB.firewallRule("A", "INPUT", "udp", 70000)),
               }, "\n")"#,
        )
        .unwrap();

        assert_eq!(
            rules.lines().collect::<Vec<_>>(),
            vec![
                "iptables -A INPUT -p udp --dport 61862 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT",
                "iptables -D OUTPUT -p tcp --sport 8080 -m conntrack --ctstate ESTABLISHED -j ACCEPT",
                "nil",
                "nil",
                "nil",
                "nil",
                "nil",
            ]
        );
    }

    #[test]
    fn the_plugin_reads_every_pairing_shape_the_installer_can_write() {
        let tmp = mount();
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_700_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        install(tmp.path(), "dev-b", "tok-b", 1_700_000_100, None, None).unwrap();
        let list = std::fs::read_to_string(dir_of(tmp.path()).join(PAIRING_FILE)).unwrap();

        let names = run_plugin_lua(&format!(
            "local raw = load({list:?})()
             local out = {{}}
             for _, e in ipairs(RB.normalisePairings(raw)) do
                 table.insert(out, RB.computerName(e))
             end
             return table.concat(out, ',')"
        ))
        .unwrap();
        assert_eq!(names, "laptop,dev-b");

        for (src, want) in [
            (r#"return { device_id = "x", token = "t" }"#, 1),
            (r#"return { device_id = "x" }"#, 0),
            (r#"return { computers = {} }"#, 0),
            (r#"return { }"#, 0),
            (r#"return "nonsense""#, 0),
        ] {
            let n =
                run_plugin_lua(&format!("return #RB.normalisePairings(load({src:?})())")).unwrap();
            assert_eq!(n, want.to_string(), "{src}");
        }
    }

    #[test]
    fn the_ladder_tries_the_cheap_rungs_first_and_degrades() {
        let rungs = |cached: &str, name: &str| {
            run_plugin_lua(&format!(
                "local out = {{}}
                 for _, r in ipairs(RB.ladder({cached}, {name})) do
                     table.insert(out, r.kind .. ':' .. r.host)
                 end
                 return table.concat(out, ' ')"
            ))
            .unwrap()
        };

        assert_eq!(
            rungs(r#"{ { host = "192.0.2.20", port = 51000 } }"#, r#""desk""#),
            "cached:192.0.2.20 broadcast:255.255.255.255 broadcast:192.0.2.255 \
             dns:desk dns:desk.lan dns:desk.home.arpa"
        );

        assert_eq!(
            rungs("nil", r#""desk""#),
            "broadcast:255.255.255.255 dns:desk dns:desk.lan dns:desk.home.arpa"
        );

        assert_eq!(
            rungs(r#"{ { host = "203.0.113.5" } }"#, "nil"),
            "cached:203.0.113.5 broadcast:255.255.255.255 broadcast:203.0.113.255"
        );

        assert_eq!(
            rungs(r#"{ { host = "desk.lan" } }"#, "nil"),
            "dns:desk.lan broadcast:255.255.255.255"
        );

        assert_eq!(
            rungs("nil", r#""203.0.113.5""#),
            "broadcast:255.255.255.255"
        );

        assert_eq!(
            rungs(
                r#"{ { host = "198.51.100.7" }, { host = "desk", typed = true },
                     { host = "192.0.2.20" }, { host = "192.0.2.63" } }"#,
                r#""desk""#
            ),
            "cached:198.51.100.7 dns:desk cached:192.0.2.20 cached:192.0.2.63 \
             broadcast:255.255.255.255 broadcast:198.51.100.255 broadcast:192.0.2.255 \
             dns:desk.lan dns:desk.home.arpa"
        );

        // Garbage in the list is skipped, never thrown on: this runs on a device with no traceback reader.
        assert_eq!(
            rungs(r#"{ "nonsense", { host = "" }, { port = 1 } }"#, "nil"),
            "broadcast:255.255.255.255"
        );
    }

    #[test]
    fn the_endpoint_cache_round_trips_and_is_stable() {
        let script = "local written = RB.serialiseEndpoints({\n\
             \x20 [\"bbb\"] = { { host = \"203.0.113.9\", port = 51001, seen_at = 20 } },\n\
             \x20 [\"aaa\"] = { { host = \"192.0.2.20\", port = 51000, seen_at = 10 },\n\
             \x20             { host = \"desk.lan\", typed = true } },\n\
             })\n\
             local back = RB.parseEndpoints(load(written)())\n\
             local again = RB.serialiseEndpoints(back)\n\
             return (written == again and 'stable ' or 'UNSTABLE ')\n\
                 .. back.aaa[1].host .. ':' .. back.aaa[1].port\n\
                 .. ' ' .. back.aaa[2].host .. (back.aaa[2].typed and '*' or '')\n\
                 .. ' ' .. back.bbb[1].host .. ':' .. back.bbb[1].port";
        assert_eq!(
            run_plugin_lua(script).unwrap(),
            "stable 192.0.2.20:51000 desk.lan* 203.0.113.9:51001"
        );

        assert_eq!(
            run_plugin_lua(
                r#"local old = RB.parseEndpoints({ ["dev-1"] = { host = "192.0.2.20", port = 51000, seen_at = 10 } })
                   return #old["dev-1"] .. " " .. old["dev-1"][1].host .. ":" .. old["dev-1"][1].port"#
            )
            .unwrap(),
            "1 192.0.2.20:51000"
        );

        for bad in [
            "nil",
            r#""not a table""#,
            r#"{ ["a"] = { port = 1 } }"#,
            r#"{ ["a"] = { host = "" } }"#,
            r#"{ ["a"] = { { host = "" }, "junk" } }"#,
            r#"{ [1] = { host = "203.0.113.1" } }"#,
        ] {
            let n = run_plugin_lua(&format!(
                "local n = 0
                 for _ in pairs(RB.parseEndpoints({bad})) do n = n + 1 end
                 return n"
            ))
            .unwrap();
            assert_eq!(n, "0", "{bad}");
        }
    }

    #[test]
    fn a_computer_keeps_several_addresses_and_never_drops_a_typed_one() {
        let hosts = |script: &str| {
            run_plugin_lua(&format!(
                "local list = {script}
                 local out = {{}}
                 for _, a in ipairs(list) do
                     out[#out + 1] = a.host .. (a.typed and '*' or '') .. '@' .. (a.seen_at or 0)
                 end
                 return table.concat(out, ' ')"
            ))
            .unwrap()
        };

        assert_eq!(
            hosts(r#"RB.rememberAddress(nil, "192.0.2.20", 51000, 100)"#),
            "192.0.2.20@100"
        );
        assert_eq!(
            hosts(
                r#"RB.rememberAddress({ { host = "192.0.2.20", seen_at = 100 } }, "198.51.100.7", 51000, 200)"#
            ),
            "198.51.100.7@200 192.0.2.20@100"
        );
        assert_eq!(
            hosts(
                r#"RB.rememberAddress({ { host = "198.51.100.7", seen_at = 200 }, { host = "192.0.2.20", seen_at = 100 } }, "192.0.2.20", 51000, 300)"#
            ),
            "192.0.2.20@300 198.51.100.7@200"
        );
        assert_eq!(
            hosts(
                r#"RB.rememberAddress({
                       { host = "a.lan", typed = true },
                       { host = "203.0.113.1", seen_at = 4 },
                       { host = "203.0.113.2", seen_at = 3 },
                       { host = "203.0.113.3", seen_at = 2 },
                       { host = "b.lan", typed = true, seen_at = 1 },
                   }, "203.0.113.9", 51000, 500)"#
            ),
            "203.0.113.9@500 a.lan*@0 203.0.113.1@4 203.0.113.2@3 b.lan*@1"
        );
        assert_eq!(
            hosts(
                r#"RB.rememberAddress({ { host = "203.0.113.1", seen_at = 4 }, { host = "b.lan", typed = true } }, "b.lan", 51000, 600)"#
            ),
            "b.lan*@600 203.0.113.1@4"
        );

        assert_eq!(
            hosts(r#"RB.addTypedAddress({ { host = "203.0.113.1", seen_at = 4 } }, "desk.lan")"#),
            "desk.lan*@0 203.0.113.1@4"
        );
        assert_eq!(
            hosts(
                r#"RB.addTypedAddress({ { host = "203.0.113.2", seen_at = 9 }, { host = "203.0.113.1", seen_at = 4 } }, "203.0.113.1")"#
            ),
            "203.0.113.1*@4 203.0.113.2@9"
        );
        assert_eq!(
            hosts(
                r#"RB.removeAddress({ { host = "203.0.113.2", seen_at = 9 }, { host = "desk.lan", typed = true } }, "desk.lan")"#
            ),
            "203.0.113.2@9"
        );

        for (typed, want) in [
            ("192.0.2.20", "192.0.2.20"),
            ("  desk.lan \n", "desk.lan"),
            ("my-laptop", "my-laptop"),
            ("", "nil"),
            ("   ", "nil"),
            ("desk lan", "nil"),
            ("-desk", "nil"),
            ("desk\"; os.exit()", "nil"),
            ("http://desk", "nil"),
        ] {
            assert_eq!(
                run_plugin_lua(&format!("return tostring(RB.validAddress({typed:?}))")).unwrap(),
                want,
                "{typed:?}"
            );
        }
    }

    #[test]
    fn both_writers_of_the_pairing_file_agree_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let awkward = "o'brien's \"mac\" \\ book\n\ttwo";
        write_pairings(
            dir.path(),
            &[
                StoredPairing {
                    device_id: "dev-a".into(),
                    token: "tok-a".into(),
                    name: Some("laptop".into()),
                    paired_at: 1_700_000_000,
                },
                StoredPairing {
                    device_id: "dev-b".into(),
                    token: "tok-b".into(),
                    name: None,
                    paired_at: 1_700_000_100,
                },
                StoredPairing {
                    device_id: "dev-c".into(),
                    token: "tok-c".into(),
                    name: Some(awkward.into()),
                    paired_at: 0,
                },
            ],
        )
        .unwrap();
        let ours = std::fs::read_to_string(dir.path().join(PAIRING_FILE)).unwrap();

        let theirs = run_plugin_lua(&format!(
            r#"return RB.serialisePairings({{
                   {{ device_id = "dev-a", token = "tok-a", name = "laptop", paired_at = 1700000000 }},
                   {{ device_id = "dev-b", token = "tok-b", paired_at = 1700000100 }},
                   {{ device_id = "dev-c", token = "tok-c", name = {awkward:?}, paired_at = 0 }},
               }})"#
        ))
        .unwrap();
        assert_eq!(ours, theirs);

        std::fs::write(dir.path().join(PAIRING_FILE), &theirs).unwrap();
        let back = read_stored_pairings(dir.path()).unwrap();
        assert_eq!(back.len(), 3);
        assert_eq!(back[2].name.as_deref(), Some(awkward));
        assert_eq!(back[1].name, None);

        let edited = run_plugin_lua(
            r#"local list = {
                   { device_id = "a", token = "ta" },
                   { device_id = "b", token = "tb" },
                   { device_id = "c", token = "tc" },
               }
               list = RB.upsertPairing(list, { device_id = "b", token = "NEW" })
               list = RB.upsertPairing(list, { device_id = "d", token = "td" })
               list = RB.removePairing(list, "a")
               local out = {}
               for _, e in ipairs(list) do out[#out + 1] = e.device_id .. "=" .. e.token end
               return table.concat(out, " ")"#,
        )
        .unwrap();
        assert_eq!(edited, "b=NEW c=tc d=td");
    }

    #[test]
    fn a_code_the_reader_shows_is_a_code_the_computer_accepts() {
        use crate::wireless::{PAIRING_CODE_LEN, normalise_code};
        for start in [0u8, 16, 100, 240] {
            let bytes: String = (0..16)
                .map(|i| format!("\\{}", start.wrapping_add(i * 7)))
                .collect();
            let shown = run_plugin_lua(&format!(
                "return RB.formatCode(RB.codeFromBytes(\"{bytes}\"))"
            ))
            .unwrap();
            assert_eq!(shown.len(), PAIRING_CODE_LEN + 3, "{shown}");
            let code = normalise_code(&shown).unwrap_or_else(|| panic!("{shown} was refused"));
            assert_eq!(code, shown.replace('-', ""));
            assert_eq!(
                normalise_code(&shown.to_lowercase().replace('-', " ")).as_deref(),
                Some(code.as_str())
            );
        }
        assert_eq!(
            run_plugin_lua(r#"return tostring(RB.codeFromBytes("too short"))"#).unwrap(),
            "nil"
        );
        assert_eq!(
            run_plugin_lua(r#"return RB.hexFromBytes("\0\1\254\255")"#).unwrap(),
            "0001feff"
        );
    }

    /// Runs the reader's half of a wifi pairing against the computer's half; returns what the reader
    /// would send and store as `nonce|device_id|reader|plugin|mac|token|name|paired_at|proof`, or
    /// `silent`.
    fn reader_answers(code: &str, pairings: &str, pair: &crate::wireless::Pair) -> String {
        let known = pair
            .known
            .iter()
            .map(|k| format!("{k:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        run_plugin_lua(&format!(
            r#"local sha = require("ffi/sha2")
               local state = {{ code = {code:?}, pairings = {pairings}, plugin = 3, reader = "Kindle" }}
               local msg = {{ v = {v}, kind = {kind:?}, nonce = {nonce:?}, at = {at},
                             name = {name:?}, known = {{ {known} }}, mac = {mac:?} }}
               local reply, entry = RB.answerPair(state, msg, RB.hmac,
                   function() return "reader-nonce", "00112233445566778899aabbccddeeff" end)
               if not reply then return "silent" end
               -- Asked twice, it says the same thing and stores nothing new.
               local again, second = RB.answerPair(state, msg, RB.hmac,
                   function() return "DIFFERENT", "ffffffffffffffffffffffffffffffff" end)
               assert(again == reply and second == nil, "a repeated request must get the same answer")
               -- A different request under the same code gets nothing: one code, one computer.
               local other = {{}}
               for k, v in pairs(msg) do other[k] = v end
               other.nonce = "another-nonce"
               other.mac = RB.hmac(state.code, RB.pairChallenge(other.nonce, other.at, other.known, other.name))
               assert(RB.answerPair(state, other, RB.hmac, function() return "x", "y" end) == nil,
                   "a used code must not link a second computer")
               return table.concat({{ reply.nonce, reply.device_id, reply.reader, reply.plugin,
                   reply.mac, entry.token, tostring(entry.name), entry.paired_at,
                   tostring(reply.proof) }}, "|")"#,
            v = pair.v,
            kind = pair.kind,
            nonce = pair.nonce,
            at = pair.at,
            name = pair.name,
            mac = pair.mac,
        ))
        .unwrap()
    }

    fn a_pair(code: &str, known: &[&str], name: &str) -> crate::wireless::Pair {
        use crate::wireless::{PROTOCOL_VERSION, mac, pair_challenge};
        let known: Vec<String> = known.iter().map(|s| s.to_string()).collect();
        crate::wireless::Pair {
            v: PROTOCOL_VERSION,
            kind: "pair".into(),
            nonce: "desk-nonce".into(),
            at: 1_700_000_000,
            name: name.into(),
            mac: mac(
                code,
                &pair_challenge("desk-nonce", 1_700_000_000, &known, name),
            ),
            known,
        }
    }

    #[test]
    fn the_readers_half_of_a_wifi_pairing_agrees_with_the_computers() {
        use crate::wireless::{mac, paired_challenge, token_challenge, verify};
        let code = "0123456789ABCDEF";

        // The name contains a colon, as a hostname may, since it sits inside a signed string.
        let pair = a_pair(code, &["aaaa", "bbbb"], "ann's mac:2");
        let said = reader_answers(code, "{}", &pair);
        let f: Vec<&str> = said.split('|').collect();
        let (nonce, device_id, reader, plugin, their_mac, token, name, paired_at) =
            (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7]);
        assert_eq!(device_id, "00112233445566778899aabbccddeeff", "a fresh id");
        assert_eq!((reader, plugin), ("Kindle", "3"));
        assert!(
            verify(
                code,
                &paired_challenge("desk-nonce", nonce, device_id, 3, reader),
                their_mac
            ),
            "the computer must be able to verify the reader's answer"
        );
        assert_eq!(
            token,
            mac(code, &token_challenge("desk-nonce", nonce, device_id)),
            "both sides must derive the same token"
        );
        assert_eq!(token.len(), 64, "the shape every other token has");
        assert_eq!(name, "ann's mac:2");
        assert_eq!(f[8], "nil", "a new reader has no old token to prove");
        assert_eq!(
            paired_at, "1700000000",
            "the computer's clock, not the reader's"
        );

        let pair = a_pair(code, &["aaaa", "dev-old"], "desk");
        let said = reader_answers(
            code,
            r#"{ { device_id = "other", token = "t0", paired_at = 5 },
                 { device_id = "dev-old", token = "t1", paired_at = 1600000000 } }"#,
            &pair,
        );
        let f: Vec<&str> = said.split('|').collect();
        assert_eq!(f[1], "dev-old", "recognised, not duplicated");
        assert_ne!(f[5], "t1", "re-keyed");
        assert_eq!(
            f[5],
            mac(code, &token_challenge("desk-nonce", f[0], "dev-old"))
        );
        assert_eq!(f[7], "1600000000");
        assert!(
            verify(
                "t1",
                &crate::wireless::rekey_challenge("desk-nonce", f[0], "dev-old"),
                f[8]
            ),
            "a reader answering as one the computer knows must prove the old token"
        );

        assert_eq!(reader_answers("FEDCBA9876543210", "{}", &pair), "silent");
        let mut tampered = a_pair(code, &["aaaa"], "desk");
        tampered.name = "evil".into();
        assert_eq!(reader_answers(code, "{}", &tampered), "silent");
        let mut tampered = a_pair(code, &["aaaa"], "desk");
        tampered.known.push("dev-old".into());
        assert_eq!(
            reader_answers(
                code,
                r#"{ { device_id = "dev-old", token = "t1" } }"#,
                &tampered
            ),
            "silent",
            "the list of ids is signed: adding one must not take over an entry"
        );
        let mut wrong_kind = a_pair(code, &[], "desk");
        wrong_kind.kind = "hello".into();
        assert_eq!(reader_answers(code, "{}", &wrong_kind), "silent");
        let mut wrong_version = a_pair(code, &[], "desk");
        wrong_version.v = 2;
        assert_eq!(reader_answers(code, "{}", &wrong_version), "silent");
    }

    #[test]
    fn the_challenges_the_plugin_builds_match_the_listeners() {
        use crate::wireless::{
            body_challenge, here_challenge, open_challenge, pair_challenge, paired_challenge,
            token_challenge,
        };
        let built = run_plugin_lua(
            "return string.format('here:%d:%s:%d', 1, 'n1', 51000) .. '|' ..
                    string.format('open:%d:%s', 1, 'n1') .. '|' ..
                    string.format('body:%d:%s:%s', 1, 'n1', 'deadbeef') .. '|' ..
                    RB.pairChallenge('n1', 1700000000, { 'aa', 'bb' }, 'desk') .. '|' ..
                    RB.pairChallenge('n1', 1700000000, {}, '') .. '|' ..
                    RB.pairedChallenge('n1', 'n2', 'dev', 3, 'Kindle') .. '|' ..
                    RB.tokenChallenge('n1', 'n2', 'dev') .. '|' ..
                    RB.rekeyChallenge('n1', 'n2', 'dev')",
        )
        .unwrap();
        assert_eq!(
            built,
            format!(
                "{}|{}|{}|{}|{}|{}|{}|{}",
                here_challenge("n1", 51000),
                open_challenge("n1"),
                body_challenge("n1", "deadbeef"),
                pair_challenge("n1", 1_700_000_000, &["aa".into(), "bb".into()], "desk"),
                pair_challenge("n1", 1_700_000_000, &[], ""),
                paired_challenge("n1", "n2", "dev", 3, "Kindle"),
                token_challenge("n1", "n2", "dev"),
                crate::wireless::rekey_challenge("n1", "n2", "dev")
            )
        );
    }

    #[test]
    #[expect(clippy::too_many_lines)]
    fn a_code_typed_on_a_computer_links_it_on_the_reader() {
        use crate::wireless::{mac, paired_challenge, token_challenge, verify};
        let tmp = mount();
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_600_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        let dir = dir_of(tmp.path());

        // The stubbed `/dev/urandom` answers sevens, so the code is sixteen 7s; the same bytes mint nonce
        // and id.
        let code = "7777777777777777";
        let minted = "07".repeat(16);
        let pair = a_pair(code, &["zzzz"], "desk");
        let wrong = a_pair("0000000000000000", &["zzzz"], "desk");
        let as_lua = |p: &crate::wireless::Pair| {
            format!(
                "{{ v = {}, kind = {:?}, nonce = {:?}, at = {}, name = {:?}, known = {{ \"zzzz\" }}, mac = {:?} }}",
                p.v, p.kind, p.nonce, p.at, p.name, p.mac
            )
        };

        let said = run_plugin_lua(&format!(
            r#"local real_open = io.open
               io.open = function(path, mode)
                   if path == "/dev/urandom" then
                       return {{ read = function(_, n) return string.rep("\7", n) end,
                                close = function() end }}
                   end
                   return real_open(path, mode)
               end
               local inbox = {{ GOOD = {good}, WRONG = {wrong} }}
               local queue = {{ "WRONG", "GOOD", "GOOD" }}
               local sent, closed = {{}}, false
               __rb_modules.json = {{
                   decode = function(s) return inbox[s] end,
                   encode = function(t) return t end,
               }}
               __rb_modules.socket = {{ udp4 = function() return {{
                   setoption = function() return true end,
                   setsockname = function() return true end,
                   settimeout = function() end,
                   setpeername = function() return nil end,
                   receivefrom = function()
                       local next = table.remove(queue, 1)
                       if next then return next, "192.0.2.63", 40000 end
                   end,
                   sendto = function(_, payload, host, port)
                       sent[#sent + 1] = {{ payload = payload, to = host .. ":" .. port }}
                       return true
                   end,
                   close = function() closed = true end,
               }} end }}

               RB.path = {path:?}
               RB.ui = require("anything")
               RB:init()
               RB:openPairing()
               assert(RB:pairingOpen(), "a code is showing")

               -- A wrong code: silence, and nothing written.
               RB.pairing.responder:waitEvent()
               assert(#sent == 0 and #RB.pairings == 1, "a wrong code changes nothing")
               -- The right one, and then the computer asking again.
               RB.pairing.responder:waitEvent()
               RB.pairing.responder:waitEvent()
               assert(#sent == 2 and sent[1].payload == sent[2].payload,
                   "a repeated request gets the same answer")
               -- Nothing more arrives; polling an empty socket is not an error.
               RB.pairing.responder:waitEvent()

               RB:closePairing("test")
               assert(closed and not RB:pairingOpen(), "the code is withdrawn")
               RB:closePairing("twice")

               local labels = {{}}
               for _, it in ipairs(RB:buildMenu()) do
                   labels[#labels + 1] = it.text or it.text_func()
               end
               local r = sent[1].payload
               return table.concat({{ r.kind, r.nonce, r.device_id, r.reader, r.plugin, r.mac,
                   sent[1].to, #RB.pairings, labels[1], labels[2] }}, "|")"#,
            good = as_lua(&pair),
            wrong = as_lua(&wrong),
            path = dir.to_string_lossy()
        ))
        .unwrap();
        let f: Vec<&str> = said.split('|').collect();
        assert_eq!(f[0], "paired");
        assert_eq!((f[1], f[2]), (minted.as_str(), minted.as_str()));
        assert_eq!(f[4], PLUGIN_VERSION.to_string());
        assert!(verify(
            code,
            &paired_challenge("desk-nonce", f[1], f[2], PLUGIN_VERSION, f[3]),
            f[5]
        ));
        assert_eq!(f[6], "192.0.2.63:40000", "answered where it was asked from");
        assert_eq!(f[7], "2");
        assert_eq!(
            (f[8], f[9]),
            ("Send highlights to laptop", "Send highlights to desk"),
            "the new computer is in the menu without a restart"
        );

        let stored = read_stored_pairings(&dir).unwrap();
        assert_eq!(stored.len(), 2);
        assert_eq!(
            (stored[0].device_id.as_str(), stored[0].token.as_str()),
            ("dev-a", "tok-a")
        );
        assert_eq!(stored[1].device_id, minted);
        assert_eq!(stored[1].name.as_deref(), Some("desk"));
        assert_eq!(stored[1].paired_at, 1_700_000_000);
        assert_eq!(
            stored[1].token,
            mac(code, &token_challenge("desk-nonce", &minted, &minted))
        );
        let endpoints = read_endpoints(&dir).unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(
            (endpoints[0].device_id.as_str(), endpoints[0].host.as_str()),
            (minted.as_str(), "192.0.2.63")
        );

        let status = inspect(tmp.path()).unwrap();
        assert!(!status.is_obstructed(), "{status:?}");
        assert_eq!(status.pairings.len(), 2);
        install(
            tmp.path(),
            "dev-a",
            "tok-a",
            1_600_000_000,
            Some("laptop"),
            None,
        )
        .unwrap();
        assert_eq!(read_stored_pairings(&dir).unwrap(), stored);
        let report = uninstall(tmp.path()).unwrap();
        assert_eq!(report.removed_pairings.len(), 2);
        assert!(!dir.exists(), "uninstall is still exact");
    }

    #[test]
    fn a_reader_is_told_the_networks_address_and_never_the_tunnels() {
        let ip = |s: &str| s.parse::<std::net::Ipv4Addr>().unwrap();
        let net = |a: &str| LanAddress {
            address: ip(a),
            broadcast: None,
        };
        let pick = |nets: Option<&[LanAddress]>, routed: Option<&str>| {
            choose_lan_address(nets, routed.map(ip)).map(|a| a.to_string())
        };

        let wifi = [net("192.0.2.222")];
        assert_eq!(
            pick(Some(&wifi), Some("192.0.2.222")).as_deref(),
            Some("192.0.2.222")
        );
        assert_eq!(
            pick(Some(&wifi), Some("198.51.100.2")).as_deref(),
            Some("192.0.2.222")
        );
        let both = [net("192.0.2.222"), net("192.0.2.40")];
        assert_eq!(
            pick(Some(&both), Some("192.0.2.40")).as_deref(),
            Some("192.0.2.40")
        );
        let mixed = [net("169.254.7.7"), net("203.0.113.9"), net("10.0.0.5")];
        assert_eq!(pick(Some(&mixed), None).as_deref(), Some("10.0.0.5"));
        assert_eq!(
            pick(Some(&[net("169.254.7.7")]), None).as_deref(),
            Some("169.254.7.7")
        );
        assert_eq!(pick(Some(&[]), Some("198.51.100.2")), None);
        assert_eq!(
            pick(None, Some("198.51.100.2")).as_deref(),
            Some("198.51.100.2")
        );
        assert_eq!(pick(None, None), None);
    }

    #[test]
    fn the_broadcast_list_always_starts_with_the_limited_broadcast() {
        let all = broadcast_addresses();
        assert_eq!(all[0], std::net::Ipv4Addr::BROADCAST);
        let mut unique = all.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), all.len(), "no address is named twice");
        for network in lan_interfaces().unwrap_or_default() {
            assert!(!network.address.is_loopback());
        }
    }

    #[test]
    fn minted_ids_are_the_length_asked_for_and_not_each_other() {
        let a = mint_id(16).unwrap();
        let b = mint_id(16).unwrap();
        assert_eq!(a.len(), 32);
        assert_eq!(mint_id(32).unwrap().len(), 64);
        assert_ne!(a, b);
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn install_and_uninstall_restore_any_tree(
                extras in proptest::collection::vec("[a-z]{1,8}", 0..6)
            ) {
                let tmp = mount();
                for (i, name) in extras.iter().enumerate() {
                    let d = tmp.path().join("koreader").join("plugins").join(format!("{name}.koplugin"));
                    std::fs::create_dir_all(&d).unwrap();
                    std::fs::write(d.join("main.lua"), format!("-- plugin {i}\n")).unwrap();
                }
                let before = snapshot(tmp.path());
                install_here(tmp.path());
                // An endpoint learned between install and uninstall must not make `uninstall` refuse.
                std::fs::write(
                    dir_of(tmp.path()).join(ENDPOINT_FILE),
                    "return { [\"dev-1\"] = { host = \"203.0.113.2\", port = 1 } }\n",
                ).unwrap();
                uninstall(tmp.path()).unwrap();
                prop_assert_eq!(snapshot(tmp.path()), before);
            }
        }
    }
}
