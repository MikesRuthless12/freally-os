// SPDX-License-Identifier: Apache-2.0
//! vfs (TASK-100, FR-043): one namespace for every process, and its rules.
//! This library is the paths and the permissions, tested on the host; the
//! service (`main.rs`) resolves them through ffs.
//!
//! - `/` holds `home` and `system`.
//! - `/system` is the system subvolume: read-only for everyone, but
//!   `accounts` writes the account records under `/system/accounts`.
//! - `/home/<user>` is that account's home: the session's account alone may
//!   read or write it; another's is `AccessDenied` whether it exists or not.
//! - With no session, no home is anyone's.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use fcore_abi::FErr;

/// Where a path lands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// `/`.
    Root,
    /// `/home`.
    Homes,
    /// Under `/system`: the components after it.
    System(Vec<String>),
    /// Under `/home/<user>`: the user and the components after it.
    Home(String, Vec<String>),
}

/// An absolute path's place: `/`-separated, no `.` or `..`, no empty
/// components (a trailing `/` is allowed), each 1–255 bytes.
pub fn parse(path: &str) -> Result<Place, FErr> {
    let rest = path.strip_prefix('/').ok_or(FErr::InvalidArgs)?;
    let mut parts: Vec<String> = Vec::new();
    for (i, c) in rest.split('/').enumerate() {
        if c.is_empty() {
            // Only a trailing slash, or "/" itself.
            if i + 1 == rest.split('/').count() {
                continue;
            }
            return Err(FErr::InvalidArgs);
        }
        if c == "." || c == ".." || c.len() > 255 || c.contains('\0') {
            return Err(FErr::InvalidArgs);
        }
        parts.push(String::from(c));
    }
    Ok(match parts.first().map(String::as_str) {
        None => Place::Root,
        Some("system") => Place::System(parts[1..].to_vec()),
        Some("home") if parts.len() == 1 => Place::Homes,
        Some("home") => Place::Home(parts[1].clone(), parts[2..].to_vec()),
        Some(_) => return Err(FErr::NotFound),
    })
}

/// What a request does to a place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
}

/// Who asks: the service conductor named the connection, and the
/// session's account (if one is logged in).
pub struct Caller<'a> {
    pub service: &'a str,
    pub session: Option<&'a str>,
}

/// The services that act for the system, not for the session's account.
const SYSTEM_SERVICES: &[&str] = &["accounts"];

/// Whether `who` may `access` `place`.
pub fn check(who: &Caller, place: &Place, access: Access) -> Result<(), FErr> {
    let system = SYSTEM_SERVICES.contains(&who.service);
    match (place, access) {
        (Place::Root | Place::Homes, Access::Read) => Ok(()),
        (Place::Root | Place::Homes, Access::Write) => Err(FErr::AccessDenied),
        (Place::System(_), Access::Read) => Ok(()),
        // The account records: accounts alone.
        (Place::System(parts), Access::Write) if system && parts.first().is_some_and(|p| p == "accounts") => Ok(()),
        (Place::System(_), Access::Write) => Err(FErr::AccessDenied),
        (Place::Home(user, _), _) if !system && who.session == Some(user.as_str()) => Ok(()),
        (Place::Home(..), _) => Err(FErr::AccessDenied),
    }
}

/// What `/home` lists for `who`: the session's own home, and nothing else.
pub fn homes_listed(who: &Caller) -> Vec<String> {
    who.session.map(|u| alloc::vec![String::from(u)]).unwrap_or_default()
}

/// A home's subvolume name.
pub fn home_subvol(user: &str) -> String {
    alloc::format!("home:{user}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| String::from(*x)).collect()
    }

    #[test]
    fn paths_land_where_they_say() {
        assert_eq!(parse("/"), Ok(Place::Root));
        assert_eq!(parse("/home"), Ok(Place::Homes));
        assert_eq!(parse("/home/"), Ok(Place::Homes));
        assert_eq!(parse("/system/accounts/mike.rec"), Ok(Place::System(s(&["accounts", "mike.rec"]))));
        assert_eq!(
            parse("/home/mike/Documents/notes.txt"),
            Ok(Place::Home(String::from("mike"), s(&["Documents", "notes.txt"])))
        );
        assert_eq!(parse("/home/mike"), Ok(Place::Home(String::from("mike"), Vec::new())));
        for bad in ["home/mike", "/home/../system", "/home/./x", "/home//x", ""] {
            assert_eq!(parse(bad), Err(FErr::InvalidArgs), "{bad:?}");
        }
        assert_eq!(parse("/media/usb"), Err(FErr::NotFound), "not mounted (later: /media/<label>)");
    }

    #[test]
    fn a_home_is_its_account_s_alone() {
        let mike = Caller { service: "terminal", session: Some("mike") };
        let nobody = Caller { service: "terminal", session: None };
        let accounts = Caller { service: "accounts", session: Some("mike") };
        let own = parse("/home/mike/x").unwrap();
        let other = parse("/home/sam/x").unwrap();
        assert_eq!(check(&mike, &own, Access::Write), Ok(()));
        assert_eq!(check(&mike, &other, Access::Read), Err(FErr::AccessDenied), "another user's home");
        assert_eq!(check(&nobody, &own, Access::Read), Err(FErr::AccessDenied), "no session, no home");
        assert_eq!(check(&accounts, &own, Access::Read), Err(FErr::AccessDenied), "accounts reads no one's files");
        // The system: read by all, written by no one but accounts, and only its records.
        let sys = parse("/system/version").unwrap();
        let recs = parse("/system/accounts/mike.rec").unwrap();
        assert_eq!(check(&mike, &sys, Access::Read), Ok(()));
        assert_eq!(check(&mike, &recs, Access::Write), Err(FErr::AccessDenied));
        assert_eq!(check(&accounts, &recs, Access::Write), Ok(()));
        assert_eq!(check(&accounts, &sys, Access::Write), Err(FErr::AccessDenied));
        assert_eq!(check(&mike, &Place::Homes, Access::Write), Err(FErr::AccessDenied));
        assert_eq!(homes_listed(&mike), s(&["mike"]));
        assert!(homes_listed(&nobody).is_empty());
    }
}
