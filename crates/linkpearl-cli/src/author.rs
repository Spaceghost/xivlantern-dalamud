//! `linkpearl author ...`: the mod author's offline tools. These never open a
//! node and never touch the network; the private key stays in one file on the
//! author's machine.
//!
//! ```text
//! linkpearl author keygen                 # once; prints the public key to embed
//! linkpearl author pubkey
//! linkpearl author sign --seq 3 --title "Patch 0.2" --body "What changed..."
//! ```
//!
//! `sign` prints an `lpannounce...` line. Paste it into a node that is in the
//! author channel (`/announce lpannounce...` in `linkpearl --author-mode`).

use std::path::PathBuf;

use linkpearl_core::author::{self, Announcement, SignedAnnouncement};

pub const USAGE: &str = "\
usage: linkpearl author keygen [--key PATH]
       linkpearl author pubkey [--key PATH]
       linkpearl author sign --seq N --title TEXT [--body TEXT | --body-file PATH] [--key PATH]

The key defaults to $XDG_CONFIG_HOME/linkpearl/author.key (~/.config/linkpearl/author.key).
It is created mode 0600 and never overwritten. Keep it out of every repository.";

fn default_key() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("linkpearl").join("author.key")
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Run `linkpearl author <args>`; returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    match run_inner(args) {
        Ok(out) => {
            println!("{out}");
            0
        }
        Err(e) => {
            eprintln!("{e}\n\n{USAGE}");
            2
        }
    }
}

fn run_inner(args: &[String]) -> Result<String, String> {
    let (verb, rest) = args.split_first().ok_or("which author command?")?;
    let mut key = default_key();
    let (mut seq, mut title, mut body) = (None::<u64>, None::<String>, String::new());
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--key" => key = PathBuf::from(value()?),
            "--seq" => seq = Some(value()?.parse().map_err(|_| "--seq is a number")?),
            "--title" => title = Some(value()?),
            "--body" => body = value()?,
            "--body-file" => {
                let path = value()?;
                body = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    match verb.as_str() {
        "keygen" => {
            let public = author::keygen(&key).map_err(|e| e.to_string())?;
            Ok(format!(
                "author key written to {} (mode 0600; keep it off every repository)\npublic key: {}",
                key.display(),
                hex(&public)
            ))
        }
        "pubkey" => {
            let k = author::load_key(&key).map_err(|e| e.to_string())?;
            Ok(hex(k.public().as_bytes()))
        }
        "sign" => {
            let k = author::load_key(&key).map_err(|e| e.to_string())?;
            let a = Announcement {
                seq: seq.ok_or("--seq is required, and must grow with every announcement")?,
                issued_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
                title: title.ok_or("--title is required")?,
                body: body.trim_end().to_string(),
            };
            let signed = SignedAnnouncement::sign(&k, &a).map_err(|e| e.to_string())?;
            Ok(signed.to_string())
        }
        other => Err(format!("unknown author command {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn keygen_pubkey_and_sign_round_trip_offline() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("author.key");
        let key_s = key.to_str().unwrap();
        let out = run_inner(&s(&["keygen", "--key", key_s])).unwrap();
        let public = run_inner(&s(&["pubkey", "--key", key_s])).unwrap();
        assert!(out.ends_with(&public), "{out}");
        assert!(run_inner(&s(&["keygen", "--key", key_s])).is_err(), "never overwritten");

        let line = run_inner(&s(&["sign", "--key", key_s, "--seq", "3", "--title", "Hello", "--body", "World"])).unwrap();
        let signed: SignedAnnouncement = line.parse().unwrap();
        let author = author::parse_author(&public).unwrap();
        let a = signed.verify(&author).expect("verifies against the printed public key");
        assert_eq!((a.seq, a.title.as_str(), a.body.as_str()), (3, "Hello", "World"));

        assert!(run_inner(&s(&["sign", "--key", key_s, "--title", "x"])).is_err(), "--seq is required");
        assert!(run_inner(&s(&["frobnicate"])).is_err());
    }
}
