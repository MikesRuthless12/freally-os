// SPDX-License-Identifier: Apache-2.0
//! The guard, first form: any `ERROR` line on COM1 that the spec didn't expect
//! fails it. (From Phase 3 it also asks `Freally.getFaults` through cdpd.)

pub fn check(log: &str, allowed: &[String]) -> Result<(), String> {
    let unexpected: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("ERROR"))
        .filter(|l| !allowed.iter().any(|a| l.contains(a.as_str())))
        .collect();
    if unexpected.is_empty() { Ok(()) } else { Err(format!("the guard: COM1 reported {}", unexpected.join(" | "))) }
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn unexpected_errors_fail_and_expected_ones_pass() {
        let log = "freally-boot 0.1.0\nfreally-boot: ERROR image refused: x\n";
        assert!(check(log, &[]).is_err());
        assert!(check(log, &["image refused".to_string()]).is_ok());
        assert!(check("all fine\n", &[]).is_ok());
    }
}
