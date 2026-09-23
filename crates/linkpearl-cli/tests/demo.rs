//! The demo as a test: two `linkpearl` processes on one machine, relays off,
//! driven through stdin exactly as a person would type, befriend each other,
//! chat 1:1 and share a channel. `scripts/demo.sh` is the same thing with the
//! transcripts left on screen.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

struct Cli {
    name: &'static str,
    child: Child,
    stdin: ChildStdin,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
}

impl Cli {
    fn start(name: &'static str, dir: &std::path::Path) -> Cli {
        Self::start_with(name, dir, &[])
    }

    fn start_with(name: &'static str, dir: &std::path::Path, extra: &[&str]) -> Cli {
        let mut child = Command::new(env!("CARGO_BIN_EXE_linkpearl"))
            .args(["--db", dir.join(format!("{name}.sqlite")).to_str().unwrap()])
            .args(["--name", name, "--relay", "off", "--heartbeat-ms", "500"])
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn linkpearl");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Cli {
            name,
            child,
            stdin,
            lines,
            seen: Vec::new(),
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Wait for an output line containing `needle`; return it.
    fn expect(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(30);
        if let Some(line) = self.seen.iter().find(|l| l.contains(needle)) {
            return line.clone();
        }
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if line.contains(needle) {
                        return line;
                    }
                }
                Err(_) => panic!(
                    "{} never printed {needle:?}; it printed:\n{}",
                    self.name,
                    self.seen.join("\n")
                ),
            }
        }
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn two_clis_befriend_chat_and_share_a_channel() {
    let dir = tempfile::tempdir().unwrap();
    let mut alice = Cli::start("Alice", dir.path());
    let mut bob = Cli::start("Bob", dir.path());
    alice.expect("ready ");
    bob.expect("ready ");

    alice.send("/invite");
    let invite = alice.expect("invite lpfriend");
    let ticket = invite.trim_start_matches("invite ").trim();
    bob.send(&format!("/accept {ticket}"));
    bob.expect("friend added: Alice");
    alice.expect("friend added: Bob");
    alice.expect("* Bob is online");
    bob.expect("* Alice is online");

    bob.send("/msg alice hello from the other terminal");
    alice.expect("<Bob> hello from the other terminal");
    bob.expect("delivered");

    alice.send("/status away crafting");
    bob.expect("* Alice is away \"crafting\"");

    alice.send("/channel new static");
    alice.expect("joined #static");
    alice.send("/channel invite Bob");
    bob.expect("Alice invited you to #static");
    let join = bob.expect("/channel join lproom");
    bob.send(&join);
    bob.expect("joined #static");
    alice.expect("#static: Bob joined");
    alice.send("pull in 5");
    bob.expect("#static <Alice> pull in 5");
    bob.send("/say ready");
    alice.expect("#static <Bob> ready");

    alice.send("/friends");
    alice.expect("Bob");
    bob.send("/quit");
    bob.expect("bye");
    alice.expect("* Bob is offline");
    alice.send("/quit");
    alice.expect("bye");
}

fn author_tool(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_linkpearl"))
        .arg("author")
        .args(args)
        .output()
        .expect("run linkpearl author");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn the_author_publishes_and_answers_a_player() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("author.key");
    let key = key.to_str().unwrap();
    author_tool(&["keygen", "--key", key]);
    let public = author_tool(&["pubkey", "--key", key]).trim().to_string();

    let mut author = Cli::start_with("Author", dir.path(), &["--author-mode", &public]);
    author.expect("ready ");
    let ticket = author.expect("lpnode").trim().to_string();
    let id_line = author.seen.iter().find(|l| l.starts_with("ready ")).unwrap().clone();
    let author_id = id_line.split_whitespace().nth(1).unwrap().to_string();

    // A player: the author's key and node are all the plugin would embed. With
    // relays off the player also needs a hint for where that node is.
    let mut player = Cli::start_with("Player", dir.path(), &["--author", &public, "--author-node", &author_id]);
    player.expect("ready ");
    player.send(&format!("/hint {ticket}"));
    player.expect("noted");

    let signed = author_tool(&["sign", "--key", key, "--seq", "1", "--title", "Hello", "--body", "Welcome to Linkpearl"]);
    // The node may not have met the player yet; announce once they are neighbours.
    std::thread::sleep(Duration::from_secs(2));
    author.send(&format!("/announce {}", signed.trim()));
    author.expect("announcement #1 published");
    player.expect("[author, verified] #1 Hello");
    player.expect("Welcome to Linkpearl");

    player.send("/support the invite button does nothing");
    author.expect("[support from");
    player.expect("delivered");
    let prefix = author
        .expect("(/reply ")
        .split("(/reply ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    author.send(&format!("/reply {prefix} fixed in 0.1.1"));
    player.expect("[author] fixed in 0.1.1");

    player.send("/selftest");
    let report = player.expect("\"direct\":");
    assert!(report.contains("\"direct\":{\"ok\":true"), "{report}");

    player.send("/quit");
    author.send("/quit");
    player.expect("bye");
    author.expect("bye");
}
