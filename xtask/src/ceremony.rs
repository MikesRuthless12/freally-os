// SPDX-License-Identifier: Apache-2.0
//! The release key (TASK-109, FR-051): made only in the owner's key
//! ceremony (`Live-To-Do.md` K1–K5), never anywhere else.
//!
//! `cargo xtask keygen --release --to <folder>` refuses unless:
//! - the folder is on a removable or USB drive (never a fixed disk, a
//!   network drive, the repository or `~/.freally-keys/`);
//! - the machine is offline: no network adapter up (under WSL, Windows' own
//!   adapters, asked of PowerShell; WSL's virtual one is always up);
//! - no release key is on that drive already;
//! - it's run in a terminal, and the passphrase is typed twice the same
//!   (12 characters at least).
//!
//! Then it makes the seed, seals it to the drive as `freally-release.key`
//! (Argon2id at 256 MiB, 4 passes → HKDF-SHA256 → XChaCha20-Poly1305),
//! prints the public key, and writes it to `boot/src/keys.rs` (K5 compares
//! them). The seed is written nowhere else and never printed.
//!
//! `cargo xtask sign <image> --key release --from <folder>` reads the sealed
//! key from its drive (the same drive rules), opens it with the passphrase,
//! signs, and wipes the seed.

use crate::util;
use fcrypto::aead::{xopen, xseal};
use fcrypto::argon2::{Params, argon2id};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const FILE: &str = "freally-release.key";
pub const PUBLIC: &str = "freally-release.pub";
const MAGIC: &str = "freally-release-key-v1";
const INFO: &[u8] = b"freally/release-key/v1";
/// The ceremony's Argon2id: 256 MiB, 4 passes (it's run once, on purpose slow).
pub const PARAMS: Params = Params { m_kib: 256 * 1024, t: 4, p: 1 };
pub const MIN_PASSPHRASE: usize = 12;

/// What a folder is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drive {
    Removable,
    Usb,
    Fixed,
    Network,
    Unknown,
}

/// Whether `path` may hold the release key.
pub fn drive_verdict(path: &Path, drive: Drive, repo: &Path, keys: &Path) -> Result<(), String> {
    if path.starts_with(repo) {
        return Err(format!("{} is inside the repository: never there", path.display()));
    }
    if path.starts_with(keys) {
        return Err(format!("{} is the keys folder: the release key lives only on its drives", path.display()));
    }
    match drive {
        Drive::Removable | Drive::Usb => Ok(()),
        Drive::Fixed => {
            Err(format!("{} is on a fixed disk: the release key goes only on its USB drives", path.display()))
        }
        Drive::Network => Err(format!("{} is on a network drive: never there", path.display())),
        Drive::Unknown => Err(format!("can't tell what drive {} is on: refused", path.display())),
    }
}

/// Whether the machine is offline: `up` names each network adapter up
/// (loopback and WSL's virtual adapter already left out); `None`, the
/// adapters couldn't be read.
pub fn network_verdict(up: Option<&[String]>) -> Result<(), String> {
    match up {
        None => Err(String::from("can't tell whether the network is up: refused")),
        Some([]) => Ok(()),
        Some(up) => Err(format!(
            "the network is up ({}): turn the Wi-Fi off and unplug any cable, then run it again",
            up.join(", ")
        )),
    }
}

pub fn passphrase_verdict(a: &str, b: &str) -> Result<(), String> {
    if a != b {
        return Err(String::from("the two passphrases differ"));
    }
    if a.chars().count() < MIN_PASSPHRASE {
        return Err(format!("the passphrase needs {MIN_PASSPHRASE} characters at least"));
    }
    Ok(())
}

/// The key that seals the seed: Argon2id, then HKDF.
fn kek(passphrase: &str, salt: &[u8; 16], params: Params) -> Result<[u8; 32], String> {
    let mut memory = vec![[0u64; 128]; params.blocks()];
    let mut stretched = [0u8; 32];
    argon2id(params, passphrase.as_bytes(), salt, &[], &[], &mut memory, &mut stretched)
        .map_err(|e| format!("Argon2id: {e:?}"))?;
    let mut key = [0u8; 32];
    fcrypto::hkdf::hkdf(salt, &stretched, INFO, &mut key).map_err(|_| "HKDF")?;
    stretched.fill(0);
    Ok(key)
}

/// The seed sealed under `passphrase`: the key file's text.
pub fn seal(
    seed: &[u8; 32],
    passphrase: &str,
    salt: &[u8; 16],
    nonce: &[u8; 24],
    params: Params,
) -> Result<String, String> {
    let mut key = kek(passphrase, salt, params)?;
    let mut data = *seed;
    let tag = xseal(&key, nonce, MAGIC.as_bytes(), &mut data);
    key.fill(0);
    Ok(format!(
        "# Freally OS release key, sealed with its passphrase. It lives only on its two drives.\n\
         {MAGIC} m={} t={} p={} {} {} {} {}\n",
        params.m_kib,
        params.t,
        params.p,
        util::hex(salt),
        util::hex(nonce),
        util::hex(&data),
        util::hex(&tag)
    ))
}

/// The seed from a key file's text, opened with `passphrase`.
pub fn open(text: &str, passphrase: &str) -> Result<[u8; 32], String> {
    let line = text.lines().find_map(|l| l.strip_prefix(MAGIC)).ok_or("not a Freally release key file")?;
    let words: Vec<&str> = line.split_whitespace().collect();
    let [m, t, p, salt, nonce, data, tag] = words.as_slice() else {
        return Err(String::from("the release key file is damaged"));
    };
    let num = |w: &str, k: &str| -> Result<u32, String> {
        w.strip_prefix(k).and_then(|v| v.parse().ok()).ok_or_else(|| String::from("the release key file is damaged"))
    };
    let params = Params { m_kib: num(m, "m=")?, t: num(t, "t=")?, p: num(p, "p=")? };
    let fixed = |h: &str| util::unhex(h).map_err(|_| String::from("the release key file is damaged"));
    let salt: [u8; 16] = fixed(salt)?.try_into().map_err(|_| "the release key file is damaged")?;
    let nonce: [u8; 24] = fixed(nonce)?.try_into().map_err(|_| "the release key file is damaged")?;
    let mut seed: [u8; 32] = fixed(data)?.try_into().map_err(|_| "the release key file is damaged")?;
    let tag: [u8; 16] = fixed(tag)?.try_into().map_err(|_| "the release key file is damaged")?;
    let mut key = kek(passphrase, &salt, params)?;
    let opened = xopen(&key, &nonce, MAGIC.as_bytes(), &mut seed, &tag);
    key.fill(0);
    opened.map_err(|_| String::from("that passphrase doesn't open this key (or the file was changed)"))?;
    Ok(seed)
}

// ---- What the machine says --------------------------------------------------

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/sys/kernel/osrelease").is_ok_and(|s| s.to_lowercase().contains("microsoft"))
}

fn powershell(script: &str) -> Option<String> {
    let out =
        Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Each network adapter that's up (loopback and virtual ones left out);
/// `None` if they can't be read.
fn adapters_up() -> Option<Vec<String>> {
    if is_wsl() {
        let names = powershell(
            "Get-NetAdapter | Where-Object { $_.Status -eq 'Up' -and -not $_.Virtual } | ForEach-Object { $_.Name }",
        )?;
        return Some(names.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect());
    }
    let mut up = Vec::new();
    for e in std::fs::read_dir("/sys/class/net").ok()? {
        let e = e.ok()?;
        let name = e.file_name().to_string_lossy().to_string();
        if name == "lo" {
            continue;
        }
        if std::fs::read_to_string(e.path().join("operstate")).is_ok_and(|s| s.trim() == "up") {
            up.push(name);
        }
    }
    Some(up)
}

/// What drive `path` is on.
fn drive_of(path: &Path) -> Drive {
    if is_wsl() {
        // /mnt/e/… is Windows' drive E.
        let mut parts = path.components().skip(1);
        let (Some(m), Some(letter)) = (parts.next(), parts.next()) else { return Drive::Unknown };
        let letter = letter.as_os_str().to_string_lossy().to_uppercase();
        if m.as_os_str() != "mnt" || letter.len() != 1 {
            // Inside WSL's own disk: a virtual disk on the laptop's drive.
            return Drive::Fixed;
        }
        let said = powershell(&format!(
            "$v = Get-Volume -DriveLetter {letter} -ErrorAction SilentlyContinue; \
             $d = Get-Partition -DriveLetter {letter} -ErrorAction SilentlyContinue | Get-Disk; \
             \"$($v.DriveType) $($d.BusType)\""
        ));
        return match said.as_deref().map(str::split_whitespace).map(|w| w.collect::<Vec<_>>()) {
            Some(w) if w.contains(&"USB") => Drive::Usb,
            Some(w) if w.first() == Some(&"Removable") => Drive::Removable,
            Some(w) if w.first() == Some(&"Fixed") => Drive::Fixed,
            Some(w) if w.first() == Some(&"Network") => Drive::Network,
            _ => Drive::Unknown,
        };
    }
    // Linux: the mount's block device, its disk's `removable`, or a USB path.
    let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else { return Drive::Unknown };
    let best = info
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let point = f.get(4)?;
            path.starts_with(point)
                .then(|| (point.len(), f.get(2).copied(), l.contains(" - nfs") || l.contains(" - cifs")))
        })
        .max_by_key(|(n, _, _)| *n);
    let Some((_, Some(dev), network)) = best else { return Drive::Unknown };
    if network {
        return Drive::Network;
    }
    let Ok(sys) = std::fs::canonicalize(format!("/sys/dev/block/{dev}")) else { return Drive::Unknown };
    if sys.to_string_lossy().contains("/usb") {
        return Drive::Usb;
    }
    let disk = if sys.join("partition").exists() { sys.parent().map(Path::to_path_buf) } else { Some(sys) };
    match disk.and_then(|d| std::fs::read_to_string(d.join("removable")).ok()).as_deref().map(str::trim) {
        Some("1") => Drive::Removable,
        Some("0") => Drive::Fixed,
        _ => Drive::Unknown,
    }
}

/// A line typed without echo (a passphrase).
fn quietly(prompt: &str) -> Result<String, String> {
    let tty = Command::new("stty").arg("-echo").stdin(Stdio::inherit()).status().map_err(|e| e.to_string())?;
    if !tty.success() {
        return Err(String::from("run it in a terminal: the passphrase is typed, never passed"));
    }
    eprint!("{prompt}");
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    let _ = Command::new("stty").arg("echo").stdin(Stdio::inherit()).status();
    eprintln!();
    read.map_err(|e| e.to_string())?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| format!("/dev/urandom: {e}"))?;
    Ok(b)
}

/// The folder a ceremony command names (`--to` or `--from`), checked.
fn checked_folder(args: &[String], flag: &str) -> Result<PathBuf, String> {
    let to = args
        .iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .ok_or_else(|| format!("{flag} <the release key drive's folder>"))?;
    let to = std::fs::canonicalize(to).map_err(|e| format!("{to}: {e}"))?;
    if !to.is_dir() {
        return Err(format!("{} isn't a folder", to.display()));
    }
    let keys = std::fs::canonicalize(util::keys_dir()).unwrap_or_else(|_| util::keys_dir());
    drive_verdict(&to, drive_of(&to), &util::repo_root(), &keys)?;
    Ok(to)
}

/// `cargo xtask keygen --release --to <folder>` (K2).
pub fn keygen(args: &[String]) -> Result<(), String> {
    let to = checked_folder(args, "--to")?;
    network_verdict(adapters_up().as_deref())?;
    let file = to.join(FILE);
    if file.exists() {
        return Err(format!("{} is there already: a release key is made once", file.display()));
    }
    let a = quietly("The release key's passphrase: ")?;
    let b = quietly("Again: ")?;
    passphrase_verdict(&a, &b)?;
    let mut seed = random::<32>()?;
    let text = seal(&seed, &a, &random::<16>()?, &random::<24>()?, PARAMS)?;
    let public = fcrypto::ed25519::public_key(&seed);
    seed.fill(0);
    std::fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))?;
    std::fs::write(to.join(PUBLIC), format!("{}\n", util::hex(&public))).map_err(|e| e.to_string())?;
    crate::keys::set_release_public(&public)?;
    println!("keygen: the release key, sealed, is on the drive: {}", file.display());
    println!("keygen: its public key: {}", util::hex(&public));
    println!("keygen: boot/src/keys.rs now trusts it in release bootloaders (K5: compare, then commit)");
    Ok(())
}

/// The seed from its drive (`--from`), opened with the passphrase typed.
pub fn load(args: &[String]) -> Result<[u8; 32], String> {
    let from = checked_folder(args, "--from")?;
    let text = std::fs::read_to_string(from.join(FILE)).map_err(|e| format!("{}: {e}", from.join(FILE).display()))?;
    open(&text, &quietly("The release key's passphrase: ")?)
}

/// The files `sign` was given: every argument but the flags and their values.
fn files_named(args: &[String]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--key" || a == "--from" {
            it.next();
        } else {
            out.push(PathBuf::from(a));
        }
    }
    out
}

/// `cargo xtask sign --key release --from <drive> <file>…` (FR-051): only a
/// committed tree whose HEAD carries this version's tag; each image signed
/// in place, each update package made again around its image signed; then
/// `SHA256SUMS.txt` beside them, naming each.
pub fn sign_release(args: &[String]) -> Result<(), String> {
    let files = files_named(args);
    let dir = files.first().and_then(|f| f.parent()).map(Path::to_path_buf).ok_or(
        "cargo xtask sign --key release --from <the release key drive's folder> <image or .fup>… (one folder)",
    )?;
    if files.iter().any(|f| f.parent() != Some(dir.as_path())) {
        return Err("the files to sign are in one folder: SHA256SUMS.txt goes beside them".into());
    }
    crate::release::committed_and_tagged()?;
    let mut seed = load(args)?;
    let signed = sign_each(&files, &seed);
    seed.fill(0);
    let mut sums = String::new();
    for (f, bytes) in files.iter().zip(signed?) {
        std::fs::write(f, &bytes).map_err(|e| format!("{}: {e}", f.display()))?;
        let name = f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        sums.push_str(&format!("{}  {name}\n", util::hex(&fcrypto::sha2::sha256(&bytes))));
        println!("sign: {} signed with the release key", f.display());
    }
    std::fs::write(dir.join("SHA256SUMS.txt"), sums).map_err(|e| e.to_string())?;
    println!("sign: {}", dir.join("SHA256SUMS.txt").display());
    Ok(())
}

/// Each file signed, in memory (nothing is written unless all of them sign).
fn sign_each(files: &[PathBuf], seed: &[u8; 32]) -> Result<Vec<Vec<u8>>, String> {
    let mut out = Vec::new();
    for f in files {
        let bytes = std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let at = |e: &str| format!("{}: {e}", f.display());
        out.push(if bytes.starts_with(&fsi::fup::MAGIC) {
            let (oldest, notes, image) = fsi::fup::unpack(&bytes).map_err(at)?;
            let mut image = image.to_vec();
            fsi::sign(&mut image, seed).map_err(|e| at(e.describe()))?;
            fsi::fup::make(oldest, notes, &image, seed).map_err(at)?
        } else {
            let mut image = bytes;
            fsi::sign(&mut image, seed).map_err(|e| at(e.describe()))?;
            image
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIGHT: Params = Params { m_kib: 64, t: 1, p: 1 };

    #[test]
    fn only_a_removable_or_usb_drive_outside_the_repo_and_keys() {
        let (repo, keys) = (Path::new("/home/m/os"), Path::new("/home/m/.freally-keys"));
        let usb = Path::new("/mnt/e/freally");
        assert!(drive_verdict(usb, Drive::Usb, repo, keys).is_ok());
        assert!(drive_verdict(usb, Drive::Removable, repo, keys).is_ok());
        for d in [Drive::Fixed, Drive::Network, Drive::Unknown] {
            assert!(drive_verdict(usb, d, repo, keys).is_err(), "{d:?}");
        }
        assert!(drive_verdict(Path::new("/home/m/os/keys"), Drive::Usb, repo, keys).is_err(), "the repo");
        assert!(drive_verdict(Path::new("/home/m/.freally-keys/r"), Drive::Usb, repo, keys).is_err(), "the keys");
    }

    #[test]
    fn the_files_to_sign_are_the_arguments_but_the_flags() {
        let args: Vec<String> =
            ["a/SYSTEM.FSI", "--key", "release", "--from", "/mnt/e/k", "a/x.fup"].map(String::from).to_vec();
        assert_eq!(files_named(&args), [PathBuf::from("a/SYSTEM.FSI"), PathBuf::from("a/x.fup")]);
    }

    #[test]
    fn only_offline() {
        assert!(network_verdict(Some(&[])).is_ok());
        let up = [String::from("Wi-Fi")];
        assert!(network_verdict(Some(&up)).unwrap_err().contains("Wi-Fi"));
        assert!(network_verdict(None).is_err(), "can't tell: refused");
    }

    #[test]
    fn the_passphrase_twice_and_long_enough() {
        assert!(passphrase_verdict("correct horse battery", "correct horse battery").is_ok());
        assert!(passphrase_verdict("correct horse battery", "correct horse batterY").is_err());
        assert!(passphrase_verdict("short", "short").is_err());
    }

    #[test]
    fn sealed_it_opens_only_with_its_passphrase_and_unchanged() {
        // A stand-in seed, never a key: the file's format is what's tested.
        let seed = [7u8; 32];
        let text = seal(&seed, "correct horse battery", &[1; 16], &[2; 24], LIGHT).unwrap();
        assert!(!text.contains(&util::hex(&seed)), "the seed isn't in the file in the clear");
        assert_eq!(open(&text, "correct horse battery").unwrap(), seed);
        assert!(open(&text, "correct horse batterY").is_err());
        // One hex digit of the sealed seed changed.
        let at = text.find(MAGIC).unwrap() + MAGIC.len() + 60;
        let mut changed = text.clone().into_bytes();
        changed[at] = if changed[at] == b'0' { b'1' } else { b'0' };
        assert!(open(&String::from_utf8(changed).unwrap(), "correct horse battery").is_err());
        assert!(open("freally-release-key-v1 m=64", "x").is_err(), "damaged");
    }
}
