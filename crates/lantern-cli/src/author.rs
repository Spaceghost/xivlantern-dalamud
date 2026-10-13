//! `lantern author ...`: the mod author's offline tools. These never open a
//! node and never touch the network; the private key stays in one file on the
//! author's machine.
//!
//! ```text
//! lantern author keygen                 # once; prints the public key to embed
//! lantern author pubkey
//! lantern author sign --seq 3 --title "Patch 0.2" --body "What changed..."
//! ```
//!
//! `sign` prints an `ltannounce...` line. Paste it into a node that is in the
//! author channel (`/announce ltannounce...` in `lantern --author-mode`).

mod sequence;

use std::{io::Read, path::{Path, PathBuf}};

use lantern_core::author::{self, Announcement, SignedAnnouncement};

pub const USAGE: &str = "\
usage: lantern author keygen [--key PATH]
       lantern author pubkey [--key PATH]
       lantern author sign --seq N --title TEXT [--body TEXT | --body-file PATH] [--key PATH] [--previous-seq N]
       lantern author verify --public-key HEX --announcement-file PATH

The key defaults to $XDG_CONFIG_HOME/lantern/author.key (~/.config/lantern/author.key).
It is created mode 0600 and never overwritten. Keep it out of every repository.";

fn default_key() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("lantern").join("author.key")
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Run `lantern author <args>`; returns the process exit code.
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
    let (mut previous, mut public, mut announcement_file) = (None::<u64>, None::<String>, None::<PathBuf>);
    let (mut seq, mut title, mut body) = (None::<u64>, None::<String>, String::new());
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        let allowed = match verb.as_str() {
            "keygen" | "pubkey" => flag == "--key",
            "sign" => matches!(flag.as_str(), "--key" | "--seq" | "--previous-seq" | "--title" | "--body" | "--body-file"),
            "verify" => matches!(flag.as_str(), "--public-key" | "--announcement-file"),
            _ => false,
        };
        if !allowed { return Err(format!("option {flag} is not valid for {verb}")); }
        let mut value = || it.next().cloned().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--key" => key = PathBuf::from(value()?),
            "--previous-seq" => previous = Some(value()?.parse().map_err(|_| "--previous-seq is a number")?),
            "--public-key" => public = Some(value()?),
            "--announcement-file" => announcement_file = Some(PathBuf::from(value()?)),
            "--seq" => seq = Some(value()?.parse().map_err(|_| "--seq is a number")?),
            "--title" => title = Some(value()?),
            "--body" => body = value()?,
            "--body-file" => {
                let path = value()?;
                body.clear();
                std::fs::File::open(path).map_err(|_| "body file could not be opened")?
                    .take((author::MAX_BODY + 3) as u64).read_to_string(&mut body)
                    .map_err(|_| "body file is not valid UTF-8")?;
                // Allow a final LF or Windows CRLF written by draft preparation.
                if body.len() > author::MAX_BODY + 2 { return Err("body file exceeds the announcement input limit".into()); }
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    match verb.as_str() {
        "keygen" => {
            let public = author::keygen(&key).map_err(|e| e.to_string())?;
            sequence::initialize(&key, &hex(&public), 0)?;
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
            let a = Announcement {
                seq: seq.ok_or("--seq is required, and must grow with every announcement")?,
                issued_at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
                title: title.ok_or("--title is required")?,
                body: body.trim_end().to_string(),
            };
            if a.seq == 0 || a.title.trim().is_empty() || a.title.len() > author::MAX_TITLE || a.body.len() > author::MAX_BODY {
                return Err("announcement needs a positive sequence and a title/body within the byte limits".into());
            }
            let k = author::load_key(&key).map_err(|e| e.to_string())?;
            let signed = SignedAnnouncement::sign(&k, &a).map_err(|e| e.to_string())?;
            sequence::reserve(&key, &hex(k.public().as_bytes()), a.seq, previous)?;
            Ok(signed.to_string())
        }
        "verify" => {
            let public = author::parse_author(&public.ok_or("--public-key is required; no private key is read")?)
                .map_err(|e| e.to_string())?;
            let path = announcement_file.ok_or("--announcement-file is required")?;
            let (_, a) = verified_file(&path, &public)?;
            Ok(format!("verified public key: {}\nsequence: {}\ntitle: {:?}\nbody: {:?}", hex(&public), a.seq, a.title, a.body))
        }
        other => Err(format!("unknown author command {other}")),
    }
}

/// Read and verify before opening any network node. Returns a canonical public
/// signed packet and its verified payload; never accesses a private key.
pub(crate) fn verified_file(path: &Path, public: &[u8; 32]) -> Result<(String, Announcement), String> {
    let mut text = String::new();
    std::fs::File::open(path).map_err(|_| "announcement file could not be opened")?.take(8193)
        .read_to_string(&mut text).map_err(|_| "announcement is not UTF-8 text")?;
    if text.len() > 8192 { return Err("announcement exceeds the input limit".into()); }
    let signed: SignedAnnouncement = text.parse().map_err(|_| "invalid announcement encoding")?;
    let canonical = signed.to_string();
    if canonical != text.trim().to_ascii_lowercase() { return Err("announcement encoding is not canonical".into()); }
    let payload = signed.verify(public).ok_or("announcement signature or author is invalid")?;
    Ok((canonical, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn oversized_body_is_rejected_before_key_read_and_does_not_consume_sequence() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("body.txt");
        let key = dir.path().join("missing.key");
        let args = |key: &str| s(&["sign", "--key", key, "--seq", "1", "--title", "Release", "--body-file", path.to_str().unwrap()]);
        let file = std::fs::File::create(&path).unwrap(); file.set_len(16 * 1024 * 1024).unwrap(); drop(file);
        assert!(run_inner(&args(key.to_str().unwrap())).unwrap_err().contains("input limit"));
        run_inner(&s(&["keygen", "--key", key.to_str().unwrap()])).unwrap();
        std::fs::write(&path, "星".repeat(501)).unwrap();
        assert!(run_inner(&args(key.to_str().unwrap())).is_err());
        std::fs::write(&path, format!("{}\r\n", "x".repeat(author::MAX_BODY))).unwrap();
        let signed: SignedAnnouncement = run_inner(&args(key.to_str().unwrap())).unwrap().parse().unwrap();
        let public = author::parse_author(&run_inner(&s(&["pubkey", "--key", key.to_str().unwrap()])).unwrap()).unwrap();
        let payload = signed.verify(&public).unwrap();
        assert_eq!(payload.seq, 1); assert_eq!(payload.body.len(), author::MAX_BODY);
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

        assert!(run_inner(&s(&["sign", "--key", key_s, "--seq", "3", "--title", "again"])).is_err(), "sequence cannot repeat");
        let signed_file = dir.path().join("signed.txt");
        std::fs::write(&signed_file, &line).unwrap();
        let verified = run_inner(&s(&["verify", "--public-key", &public, "--announcement-file", signed_file.to_str().unwrap()])).unwrap();
        assert!(verified.contains("sequence: 3"));
        let mut tampered = signed.clone(); tampered.sig[0] ^= 1;
        std::fs::write(&signed_file, tampered.to_string()).unwrap();
        assert!(run_inner(&s(&["verify", "--public-key", &public, "--announcement-file", signed_file.to_str().unwrap()])).is_err());
        assert!(run_inner(&s(&["sign", "--key", key_s, "--title", "x"])).is_err(), "--seq is required");
        assert!(run_inner(&s(&["frobnicate"])).is_err());
    }
}
