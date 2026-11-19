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
- Nothing is sent anywhere: there's no network stack yet, no telemetry, and no account with us.

## Updates and disks

- An update (`.fup`) is refused unless its header is signed by a key the bootloader trusts, its notes and image match the hashes it signed, and the image verifies on its own. It's written to the slot not running and starts on trial: a system that doesn't start goes back to the one before, by itself.
- Freally OS writes only inside its own partitions, checked in the disk driver before every write. For now a release build uses a virtual machine's disk only: on a real computer it doesn't even read the disk.

## Reporting a problem

Please email **freallyproducts@gmail.com** with *Security* in the subject, and don't open a public issue. Include what you found, how to reproduce it, and the version (`Freally OS vX.Y.Z` on the boot screen).

You'll get a reply. Fixes ship as signed updates, and the changelog credits the reporter if they'd like.

## Supported versions

Nothing is downloadable before `v1.0.0`. Until then, reports against the current `main` are welcome.
