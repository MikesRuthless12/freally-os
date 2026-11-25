# Security

## The seal

Every byte of a Freally OS system image is verified before it runs:

- the image header carries an **Ed25519 signature** and the **SHA-256 Merkle root** of every 4 KiB block;
- Freally's own UEFI bootloader checks both before it hands over, and shows an integrity screen instead of booting a changed image;
- release bootloaders trust **one key**, Mike's release key, which is made and kept offline; development and test images are signed with other keys that release machines refuse;
- there's no flag, variable or boot option that skips a check, and development-only code (the Brain, serial logging, the CDP test door) is compiled out of release builds rather than switched off. A check on every build (`cargo xtask validate`) looks for them in the release image itself.

## The release key

- It's made on a machine with every network adapter down, onto a removable drive and nowhere else, sealed with a passphrase (Argon2id at 256 MiB, then XChaCha20-Poly1305). Two copies, kept apart.
- Signing with it refuses a tree that isn't committed and tagged with its version. Until it exists, release builds are signed with a staging key that only test machines see.

## Your data

- Each account's home is its own encrypted subvolume (XChaCha20-Poly1305 per 4 KiB block). Its key is sealed under one made from the password (Argon2id at 64 MiB, three passes), opened at login and wiped at logout.
- The account records (each password's salt and sealed key) can be read by the accounts service alone; a wrong password waits a second longer each time from the third.
- An exported account (`.facct`) is sealed with its password; what's copied to the clipboard is emptied when the session ends.
- There's no telemetry and no account with us: nothing about you is sent anywhere. What does go over the network, and when, is below.

## The network

- Freally OS connects only when something asks, and **Settings ▸ Network** lists every kind of connection it can make: netstack's own (DHCP for an address, IPv6 router discovery, and DNS to the servers the network gave, only when a program asks for a name), then each program it serves: the Terminal's `ip`, `ping`, `nslookup` and `fetch` when you run them, Settings, Monitor's Network tab while it's shown, and the updater only when you press *Check online* and then *Download*. Any other program is refused, and the refusal is logged.
- Nothing listens for connections from outside: no server, no remote login, no open port. (Test images listen for the test harness; release images don't carry that code.)
- Encrypted connections are TLS 1.3 only (no older TLS, no resumption, no 0-RTT), the server's certificate chain checked against roots built into the image, and an update is installed only if it's signed (below).
- Every parser that reads what comes off the network is fuzzed (Ethernet, ARP, IPv4 and IPv6, ICMP, UDP, TCP, DHCP, DNS, X.509, TLS, HTTP), and every table a peer or a program can grow has a limit.
- A known gap: a network card's DMA isn't yet confined by an IOMMU, so a faulty or hostile card could reach memory it wasn't given. Confining it (Intel VT-d) comes later.

## Updates and disks

- An update (`.fup`) is refused unless its header is signed by a key the bootloader trusts, its notes and image match the hashes it signed, and the image verifies on its own. It's written to the slot not running and starts on trial: a system that doesn't start goes back to the one before, by itself.
- Freally OS writes only inside its own partitions, checked in the disk driver before every write. For now a release build uses a virtual machine's disk only: on a real computer it doesn't even read the disk.

## Reporting a problem

Please email **freallyproducts@gmail.com** with *Security* in the subject, and don't open a public issue. Include what you found, how to reproduce it, and the version (`Freally OS vX.Y.Z` on the boot screen).

You'll get a reply. Fixes ship as signed updates, and the changelog credits the reporter if they'd like.

## Supported versions

Nothing is downloadable before `v1.0.0`. Until then, reports against the current `main` are welcome.
