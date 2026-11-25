// SPDX-License-Identifier: Apache-2.0
//! Every connection this computer can make (TASK-183, FR-060): netstack's
//! own, and each program it serves, with what it does and when. It's the
//! one list: Settings ▸ Network shows it, and netstack serves no client it
//! doesn't name (the refusal said in the log).

/// One kind of connection: whose, what, and when it's made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    /// The program (a service's name), or `netstack` for its own.
    pub who: &'static str,
    pub what: &'static str,
    pub when: &'static str,
}

const ALWAYS: [Grant; 7] = [
    Grant {
        who: "netstack",
        what: "DHCP on each network (UDP 68 to 67): an address, a router and DNS servers",
        when: "when the network comes up, and to renew its lease",
    },
    Grant {
        who: "netstack",
        what: "IPv6 router discovery and neighbour checks on each network (ICMPv6)",
        when: "when the network comes up",
    },
    Grant {
        who: "netstack",
        what: "DNS to the servers the network gave (UDP 53, TCP 53 for long answers)",
        when: "only when a program below asks for a name",
    },
    Grant { who: "terminal", what: "what you type: ip, ping, nslookup and fetch", when: "only when you run them" },
    Grant { who: "settings", what: "this page: it reads the networks and this list", when: "while it's open" },
    Grant {
        who: "monitor",
        what: "its Network tab: each network's packets and bytes",
        when: "while that tab is shown",
    },
    Grant {
        who: "updater",
        what: "the update host (HTTPS), for its list of updates and then the one you choose",
        when: "only when you press Check online, then Download",
    },
];

/// In test images only (never in a release image's list).
const TESTS: [Grant; 2] = [
    Grant {
        who: "conductor",
        what: "the test door's TCP and TLS checks, and a port it listens on",
        when: "in test images only, when a test asks",
    },
    Grant {
        who: "cdpd",
        what: "the test harness's view of the network, and the port the harness reaches it on",
        when: "in test images only",
    },
];

/// The list, for an e2e image (`e2e`) or any other.
pub fn list(e2e: bool) -> impl Iterator<Item = Grant> {
    ALWAYS.into_iter().chain(TESTS.into_iter().filter(move |_| e2e))
}

/// Whether netstack may serve `client`: a program the list names (netstack's
/// own entries aren't a client).
pub fn allowed(client: &str, e2e: bool) -> bool {
    client != "netstack" && list(e2e).any(|g| g.who == client)
}

/// Whether `client` may listen for connections (TASK-184): only the test
/// clients, only in an e2e image.
pub fn may_listen(client: &str, e2e: bool) -> bool {
    e2e && TESTS.iter().any(|g| g.who == client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netstack_serves_the_programs_the_list_names_and_no_one_else() {
        for who in ["terminal", "settings", "updater", "monitor"] {
            assert!(allowed(who, false) && allowed(who, true), "{who}");
        }
        for who in ["conductor", "cdpd"] {
            assert!(!allowed(who, false), "{who} in a release image");
            assert!(allowed(who, true), "{who} in a test image");
        }
        for who in ["netstack", "desktop", "files", "", "terminal2"] {
            assert!(!allowed(who, true), "{who}");
        }
    }

    /// Only the test clients listen (TASK-184), and only in an e2e image.
    #[test]
    fn only_the_test_clients_listen() {
        for who in ["conductor", "cdpd"] {
            assert!(may_listen(who, true) && !may_listen(who, false), "{who}");
        }
        for who in ["terminal", "settings", "updater", "monitor", "netstack", "intruder", ""] {
            assert!(!may_listen(who, true), "{who}");
        }
    }

    #[test]
    fn every_grant_says_what_and_when() {
        assert!(list(true).all(|g| !g.who.is_empty() && !g.what.is_empty() && !g.when.is_empty()));
        assert_eq!(list(false).count(), 7);
        assert_eq!(list(true).count(), 9);
    }
}
