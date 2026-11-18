# Security

## The seal

Every byte of a Freally OS system image is verified before it runs:

- the image header carries an **Ed25519 signature** and the **SHA-256 Merkle root** of every 4 KiB block;
- Freally's own UEFI bootloader checks both before it hands over, and shows an integrity screen instead of booting a changed image;
- release bootloaders trust **one key**, Mike's release key, which is made and kept offline; development and test images are signed with other keys that release machines refuse;
- there's no flag, variable or boot option that skips a check, and development-only code (the Brain, serial logging, the CDP test door) is compiled out of release builds rather than switched off.

## Reporting a problem

Please email **freallyproducts@gmail.com** with *Security* in the subject, and don't open a public issue. Include what you found, how to reproduce it, and the version (`Freally OS vX.Y.Z` on the boot screen).

You'll get a reply. Fixes ship as signed updates, and the changelog credits the reporter if they'd like.

## Supported versions

Nothing is downloadable before `v1.0.0`. Until then, reports against the current `main` are welcome.
