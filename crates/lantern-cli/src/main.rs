//! `lantern`: a line-oriented client for two people on a terminal, and the
//! demo for lantern-core. It drives the same synchronous `Node` handle the C
//! ABI wraps: commands are pushed, events are polled every few milliseconds.
//!
//! ```text
//! lantern --db alice.sqlite --name Alice --relay off
//! ```
//!
//! Type `/help` for commands. Lines that do not start with `/` go to the
//! current channel.

mod author;

use std::{
    collections::HashMap,
    io::BufRead,
    sync::mpsc,
    time::Duration,
};

use lantern_core::{Config, Event, Node, PeerId, RelayMode, RoomHandle, Scope, Status};

const HELP: &str = "\
commands:
  /id                         this node's id and direct ticket
  /name <name>                set the name friends see
  /invite [ttl-secs]          print a single-use friend invite
  /accept <ltfriend...>       redeem somebody's invite
  /friends                    list friends and presence
  /msg <friend> <text>        1:1 text (friend = name or id prefix)
  /history <friend> [n]       last n texts with a friend
  /status <online|away|busy|invisible> [note]
  /unfriend <friend>
  /channel new <label>        create a group channel and make it current
  /channel join <ltroom...>   join a channel from a ticket
  /channel invite <friend>    invite a friend to the current channel
  /channel ticket             print the current channel's ticket
  /channels                   list joined channels
  /say <text>                 send to the current channel (or just type)
  /block <friend|id>  /unblock <id>  /blocked
  /selftest                   is this node reachable, directly and via relay?
  /hint <ltnode...>           remember where a node is (relays off / LAN)
  /announcements              the author channel's verified announcements
  /support <text>             write to the author (needs --author and --author-node)
  /announce <ltannounce...>   author mode: publish a signed announcement
  /reply <id-prefix> <text>   author mode: answer a support message
  /nostr id|import <nsec>|publish|devices <npub>|invite <npub> [ttl]|inbox
                              (built with --features nostr; needs --nostr-relay)
  /quit";

struct Args {
    db: String,
    name: Option<String>,
    relay: RelayMode,
    port: Option<u16>,
    heartbeat_ms: Option<u64>,
    nostr_relays: Vec<String>,
    /// The author key whose channel to join (64 hex).
    author: Option<[u8; 32]>,
    /// The author's always-on nodes (64 hex each).
    author_nodes: Vec<PeerId>,
    /// This node is the author's: accept support messages.
    author_mode: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: lantern [--db PATH] [--name NAME] [--relay default|off|URL] [--port UDP_PORT] [--heartbeat-ms N]\n                 [--author KEYHEX [--author-node NODEID]... | --author-mode KEYHEX] [--nostr-relay WS_URL]...\n       lantern author keygen|pubkey|sign ...   (offline; see lantern author)\n\n{HELP}"
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut args = Args {
        db: "lantern.sqlite".into(),
        name: None,
        relay: RelayMode::Default,
        port: None,
        heartbeat_ms: None,
        nostr_relays: Vec::new(),
        author: None,
        author_nodes: Vec::new(),
        author_mode: false,
    };
    let parse_key = |v: String| lantern_core::author::parse_author(&v).unwrap_or_else(|e| {
        eprintln!("{e}");
        usage()
    });
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--db" => args.db = value(),
            "--name" => args.name = Some(value()),
            "--relay" => {
                args.relay = match value().as_str() {
                    "default" => RelayMode::Default,
                    "off" | "disabled" | "none" => RelayMode::Disabled,
                    url => RelayMode::Custom(url.to_string()),
                }
            }
            "--port" => args.port = Some(value().parse().unwrap_or_else(|_| usage())),
            "--heartbeat-ms" => args.heartbeat_ms = Some(value().parse().unwrap_or_else(|_| usage())),
            "--nostr-relay" => args.nostr_relays.push(value()),
            "--author" => args.author = Some(parse_key(value())),
            "--author-node" => args.author_nodes.push(parse_key(value())),
            "--author-mode" => {
                args.author = Some(parse_key(value()));
                args.author_mode = true;
            }
            "-h" | "--help" => usage(),
            _ => usage(),
        }
    }
    args
}

fn hex(id: &PeerId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn short(id: &PeerId) -> String {
    hex(id)[..10].to_string()
}

struct Cli {
    node: Node,
    /// Names for peers: friends by their profile name, channel members by
    /// the room presence they announce.
    names: HashMap<PeerId, String>,
    channels: Vec<RoomHandle>,
    current: Option<RoomHandle>,
    running: bool,
    #[cfg_attr(not(feature = "nostr"), allow(dead_code))]
    nostr_relays: Vec<String>,
    author: Option<[u8; 32]>,
    author_room: Option<RoomHandle>,
    author_nodes: Vec<PeerId>,
    /// Author mode: who wrote in, for /reply.
    writers: Vec<PeerId>,
}

impl Cli {
    fn name_of(&self, peer: &PeerId) -> String {
        match self.names.get(peer) {
            Some(n) if !n.is_empty() => n.clone(),
            _ => short(peer),
        }
    }

    fn refresh_names(&mut self) {
        for f in self.node.friends() {
            self.names.insert(f.peer, f.name);
        }
    }

    /// A friend by exact (case-insensitive) name, or by node id hex prefix.
    fn friend(&self, who: &str) -> Result<PeerId, String> {
        let friends = self.node.friends();
        let by_name: Vec<_> = friends
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case(who))
            .collect();
        if by_name.len() == 1 {
            return Ok(by_name[0].peer);
        }
        let prefix = who.to_ascii_lowercase();
        let by_id: Vec<_> = friends
            .iter()
            .filter(|f| !prefix.is_empty() && hex(&f.peer).starts_with(&prefix))
            .collect();
        match (by_name.len(), by_id.len()) {
            (_, 1) => Ok(by_id[0].peer),
            (0, 0) => Err(format!("no friend called {who:?} (see /friends)")),
            _ => Err(format!("{who:?} is ambiguous; use an id prefix")),
        }
    }

    fn command(&mut self, line: &str) -> Result<(), String> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        if !line.starts_with('/') {
            return self.say(line);
        }
        let (cmd, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let e = |e: lantern_core::Error| e.to_string();
        match cmd {
            "/help" => println!("{HELP}"),
            "/quit" | "/exit" => self.running = false,
            "/id" => {
                println!("id     {}", hex(&self.node.node_id()));
                println!("name   {}", self.node.display_name());
                println!("ticket {}", self.node.node_ticket());
            }
            "/name" => {
                self.node.set_display_name(rest).map_err(e)?;
                println!("name set to {}", self.node.display_name());
            }
            "/invite" => {
                let ttl = match rest {
                    "" => None,
                    s => Some(Duration::from_secs(s.parse().map_err(|_| "ttl is seconds")?)),
                };
                println!("invite {}", self.node.invite_create(ttl).map_err(e)?);
            }
            "/accept" => {
                let h = self.node.invite_accept(rest).map_err(e)?;
                println!("redeeming invite (#{h})...");
            }
            "/friends" => {
                let friends = self.node.friends();
                if friends.is_empty() {
                    println!("no friends yet: /invite, or /accept a ticket");
                }
                for f in friends {
                    let state = if f.online { f.status.as_str() } else { "offline" };
                    let note = if f.note.is_empty() { String::new() } else { format!(" \"{}\"", f.note) };
                    println!("  {:<16} {:<8} {}{}", f.name, state, short(&f.peer), note);
                }
            }
            "/msg" => {
                let (who, text) = rest.split_once(char::is_whitespace).ok_or("usage: /msg <friend> <text>")?;
                let peer = self.friend(who)?;
                let id = self.node.friend_send(&peer, text.trim()).map_err(e)?;
                println!("-> {} [{:016x}] {}", self.name_of(&peer), id, text.trim());
            }
            "/history" => {
                let mut parts = rest.split_whitespace();
                let peer = self.friend(parts.next().ok_or("usage: /history <friend> [n]")?)?;
                let n = parts.next().and_then(|n| n.parse().ok()).unwrap_or(20);
                for m in self.node.friend_history(&peer, n).map_err(e)? {
                    let who = if m.outgoing { "me".to_string() } else { self.name_of(&peer) };
                    let mark = match (m.outgoing, m.delivered_at) {
                        (true, None) => " (not delivered yet)",
                        _ => "",
                    };
                    println!("  <{who}> {}{mark}", m.text);
                }
            }
            "/status" => {
                let (s, note) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let status = match s {
                    "online" => Status::Online,
                    "away" => Status::Away,
                    "busy" => Status::Busy,
                    "invisible" => Status::Invisible,
                    _ => return Err("usage: /status <online|away|busy|invisible> [note]".into()),
                };
                self.node.set_presence(status, note).map_err(e)?;
                println!("status {}{}", status.as_str(), if note.is_empty() { String::new() } else { format!(" \"{}\"", note.trim()) });
            }
            "/unfriend" => {
                let peer = self.friend(rest)?;
                self.node.friend_remove(&peer).map_err(e)?;
            }
            "/channel" => {
                let (sub, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                let arg = arg.trim();
                match sub {
                    "new" => {
                        let room = self.node.channel_create(arg).map_err(e)?;
                        self.joined(room);
                        println!("channel #{} created; ticket:", self.node.room_label(room).map_err(e)?);
                        println!("{}", self.node.room_ticket(room).map_err(e)?);
                    }
                    "join" => {
                        let room = self.node.room_join(Scope::Custom, "", Some(arg)).map_err(e)?;
                        self.joined(room);
                        println!("joining #{}...", self.node.room_label(room).map_err(e)?);
                    }
                    "invite" => {
                        let room = self.current.ok_or("no current channel")?;
                        let peer = self.friend(arg)?;
                        self.node.channel_invite(&peer, room).map_err(e)?;
                        println!("invited {} to #{}", self.name_of(&peer), self.node.room_label(room).map_err(e)?);
                    }
                    "ticket" => {
                        let room = self.current.ok_or("no current channel")?;
                        println!("{}", self.node.room_ticket(room).map_err(e)?);
                    }
                    _ => return Err("usage: /channel new|join|invite|ticket ...".into()),
                }
            }
            "/channels" => {
                for &room in &self.channels {
                    let mark = if Some(room) == self.current { "*" } else { " " };
                    let peers = self.node.room_peers(room).map(|p| p.len()).unwrap_or(0);
                    let label = self.node.room_label(room).unwrap_or_default();
                    println!(" {mark} #{label} ({peers} neighbours)");
                }
            }
            "/say" => return self.say(rest),
            "/block" => {
                let peer = self.friend(rest).or_else(|_| parse_id(rest))?;
                self.node.block(&peer).map_err(e)?;
                println!("blocked {}", self.name_of(&peer));
            }
            "/unblock" => {
                let peer = parse_id(rest)
                    .or_else(|_| {
                        let hits: Vec<_> = self.node.blocked().into_iter().filter(|p| hex(p).starts_with(rest)).collect();
                        if hits.len() == 1 { Ok(hits[0]) } else { Err("give the id (see /blocked)".to_string()) }
                    })?;
                self.node.unblock(&peer).map_err(e)?;
                println!("unblocked {}", short(&peer));
            }
            "/blocked" => {
                let list = self.node.blocked();
                if list.is_empty() {
                    println!("nobody is blocked");
                }
                for p in list {
                    println!("  {}", hex(&p));
                }
            }
            "/selftest" => {
                let h = self.node.selftest().map_err(e)?;
                println!("selftest #{h} running (up to ~40 s)...");
            }
            "/hint" => {
                self.node.add_address_hint(rest).map_err(e)?;
                println!("noted");
            }
            "/announcements" => {
                let author = self.author.ok_or("no --author given")?;
                let list = self.node.announcements(&author, 20).map_err(e)?;
                if list.is_empty() {
                    println!("no announcements yet");
                }
                for a in list {
                    println!("  [#{}] {} — {}", a.seq, a.title, a.body);
                }
            }
            "/announce" => {
                let room = self.author_room.ok_or("not in an author channel (--author-mode KEY)")?;
                let seq = self.node.author_announce(room, rest).map_err(e)?;
                println!("announcement #{seq} published");
            }
            "/support" => {
                let to = *self.author_nodes.first().ok_or("no --author-node given")?;
                let id = self.node.support_send(&to, rest).map_err(e)?;
                println!("-> author [{id:016x}] {rest}");
            }
            "/reply" => {
                let (who, text) = rest.split_once(char::is_whitespace).ok_or("usage: /reply <id-prefix> <text>")?;
                let hits: Vec<PeerId> = self.writers.iter().filter(|p| hex(p).starts_with(who)).copied().collect();
                let [peer] = hits[..] else {
                    return Err("no single writer matches that prefix".into());
                };
                let id = self.node.support_reply(&peer, text.trim()).map_err(e)?;
                println!("-> {} [{id:016x}] {}", short(&peer), text.trim());
            }
            "/nostr" => return self.nostr(rest),
            _ => return Err(format!("unknown command {cmd}; /help")),
        }
        Ok(())
    }

    #[cfg(not(feature = "nostr"))]
    fn nostr(&mut self, _: &str) -> Result<(), String> {
        Err("this lantern was built without --features nostr".into())
    }

    #[cfg(feature = "nostr")]
    fn nostr(&mut self, rest: &str) -> Result<(), String> {
        let e = |e: lantern_core::Error| e.to_string();
        let (sub, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        let arg = arg.trim();
        let relays = &self.nostr_relays;
        let need_relays = || {
            if relays.is_empty() {
                Err("no relays: start with --nostr-relay wss://...".to_string())
            } else {
                Ok(())
            }
        };
        match sub {
            "id" => {
                self.node.nostr_keys().map_err(e)?;
                println!("nostr {}", self.node.nostr_npub().unwrap_or_default());
            }
            "import" => {
                self.node.nostr_import(arg).map_err(e)?;
                println!("nostr {}", self.node.nostr_npub().unwrap_or_default());
            }
            "publish" => {
                need_relays()?;
                let h = self.node.nostr_publish_devices(relays).map_err(e)?;
                println!("publishing device list (#{h})...");
            }
            "devices" => {
                need_relays()?;
                let h = self.node.nostr_fetch_devices(relays, arg).map_err(e)?;
                println!("looking up devices (#{h})...");
            }
            "invite" => {
                need_relays()?;
                let (who, ttl) = arg.split_once(char::is_whitespace).unwrap_or((arg, ""));
                let ttl = match ttl.trim() {
                    "" => None,
                    s => Some(Duration::from_secs(s.parse().map_err(|_| "ttl is seconds")?)),
                };
                let h = self.node.nostr_send_invite(relays, who, ttl).map_err(e)?;
                println!("sending invite (#{h})...");
            }
            "inbox" => {
                need_relays()?;
                let h = self.node.nostr_check_inbox(relays).map_err(e)?;
                println!("checking inbox (#{h})...");
            }
            _ => return Err("usage: /nostr id|import|publish|devices|invite|inbox".into()),
        }
        Ok(())
    }

    fn joined(&mut self, room: RoomHandle) {
        self.channels.push(room);
        self.current = Some(room);
        // Room presence is how channel members who are not friends learn our
        // name. It is only what the player typed as their name.
        let _ = self.node.presence_set(room, self.node.display_name().as_bytes());
    }

    fn say(&mut self, text: &str) -> Result<(), String> {
        let room = self.current.ok_or("no current channel: /channel new or /channel join")?;
        self.node.room_send(room, text.as_bytes()).map_err(|e| e.to_string())?;
        let label = self.node.room_label(room).unwrap_or_default();
        println!("#{label} <me> {text}");
        Ok(())
    }

    fn label(&self, room: RoomHandle) -> String {
        self.node.room_label(room).unwrap_or_else(|_| room.to_string())
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Ready { node_id } => println!("ready {} as {:?}", hex(&node_id), self.node.display_name()),
            Event::FriendAdded { peer, name, .. } => {
                self.names.insert(peer, name.clone());
                println!("friend added: {name} ({})", short(&peer));
            }
            Event::FriendRemoved { peer } => {
                println!("friend removed: {}", self.name_of(&peer));
                self.names.remove(&peer);
            }
            Event::InviteFailed { reason, .. } => println!("invite failed: {reason}"),
            Event::FriendOnline { peer, status, note } => {
                self.refresh_names();
                let note = if note.is_empty() { String::new() } else { format!(" \"{note}\"") };
                println!("* {} is {}{note}", self.name_of(&peer), status.as_str());
            }
            Event::FriendOffline { peer } => println!("* {} is offline", self.name_of(&peer)),
            Event::FriendText { peer, text, .. } => println!("<{}> {text}", self.name_of(&peer)),
            Event::FriendDelivered { peer, id } => {
                println!("delivered [{id:016x}] to {}", self.name_of(&peer))
            }
            Event::ChannelInvite { peer, ticket } => {
                let label = ticket
                    .parse::<lantern_core::RoomTicket>()
                    .map(|t| t.label)
                    .unwrap_or_default();
                println!("{} invited you to #{label}; to join:", self.name_of(&peer));
                println!("/channel join {ticket}");
            }
            Event::RoomJoined { room } => println!("joined #{}", self.label(room)),
            Event::RoomLeft { room } => println!("left #{}", self.label(room)),
            Event::PeerJoined { room, peer } => {
                println!("#{}: {} joined", self.label(room), self.name_of(&peer))
            }
            Event::PeerLeft { room, peer } => {
                println!("#{}: {} left", self.label(room), self.name_of(&peer))
            }
            Event::Presence { peer, data, .. } => {
                if let Ok(name) = String::from_utf8(data) {
                    // A friend's own name wins over what a room says.
                    self.names.entry(peer).or_insert(name);
                }
            }
            Event::Message { room, peer, data } => println!(
                "#{} <{}> {}",
                self.label(room),
                self.name_of(&peer),
                String::from_utf8_lossy(&data)
            ),
            Event::Error { message } => println!("error: {message}"),
            Event::RateLimited { peer, what } => {
                println!("rate limit: dropping {what} frames from {}", self.name_of(&peer))
            }
            Event::Announcement { seq, title, body, .. } => {
                println!("[author, verified] #{seq} {title}");
                if !body.is_empty() {
                    println!("  {body}");
                }
            }
            Event::SupportMessage { peer, text, .. } => {
                if !self.writers.contains(&peer) {
                    self.writers.push(peer);
                }
                println!("[support from {}] {text}   (/reply {} ...)", short(&peer), short(&peer));
            }
            Event::SupportReply { text, .. } => println!("[author] {text}"),
            Event::SupportDelivered { id, .. } => println!("delivered [{id:016x}] to the author"),
            Event::SupportFailed { id, reason, .. } => println!("not delivered [{id:016x}]: {reason}"),
            Event::SelfTest { handle, report } => println!("selftest #{handle}: {report}"),
            Event::NostrDone { handle, ok, detail } => {
                println!("nostr #{handle} {}: {detail}", if ok { "done" } else { "failed" })
            }
            Event::NostrInvite { from, ticket } => {
                println!("nostr invite from {}; to accept:", hex(&from));
                println!("/accept {ticket}");
            }
            Event::NostrDevices { user, devices, .. } => {
                println!("nostr {} has {} device(s):", short(&user), devices.len());
                for d in devices {
                    let tag = if self.names.contains_key(&d) { " (friend)" } else { "" };
                    println!("  {}{tag}", hex(&d));
                }
            }
            // Blob, direct-connection and log events are not part of this demo.
            _ => {}
        }
    }
}

fn parse_id(text: &str) -> Result<PeerId, String> {
    lantern_core::author::parse_author(text).map_err(|_| "not a 64-character node id".to_string())
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("author") {
        std::process::exit(author::run(&raw[1..]));
    }
    let args = parse_args();
    let mut config = Config {
        db_path: Some(args.db.clone().into()),
        relay: args.relay,
        bind_port: args.port,
        accept_support: args.author_mode,
        ..Config::default()
    };
    if let Some(ms) = args.heartbeat_ms {
        config.heartbeat_ms = ms;
    }
    let node = match Node::open(config) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("could not open {}: {e}", args.db);
            std::process::exit(1);
        }
    };
    if let Some(name) = &args.name {
        if let Err(e) = node.set_display_name(name) {
            eprintln!("name: {e}");
        }
    }
    let mut cli = Cli {
        node,
        names: HashMap::new(),
        channels: Vec::new(),
        current: None,
        running: true,
        nostr_relays: args.nostr_relays,
        author: args.author,
        author_room: None,
        author_nodes: args.author_nodes.clone(),
        writers: Vec::new(),
    };
    if let Some(key) = args.author {
        cli.node.set_support_contacts(&args.author_nodes);
        match cli.node.author_join(&key, &args.author_nodes) {
            Ok(room) => {
                cli.author_room = Some(room);
                if args.author_mode {
                    println!("author mode: support messages accepted; this node's ticket:");
                    println!("{}", cli.node.node_ticket());
                }
            }
            Err(e) => println!("author channel: {e}"),
        }
    }
    cli.refresh_names();

    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) => {
                    if tx.send(Some(l)).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send(None);
    });

    while cli.running {
        for event in cli.node.poll(256) {
            cli.event(event);
        }
        match rx.recv_timeout(Duration::from_millis(25)) {
            Ok(Some(line)) => {
                if let Err(msg) = cli.command(&line) {
                    println!("error: {msg}");
                }
            }
            Ok(None) | Err(mpsc::RecvTimeoutError::Disconnected) => cli.running = false,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    // Let anything already queued go out and print what came back.
    std::thread::sleep(Duration::from_millis(200));
    for event in cli.node.poll(256) {
        cli.event(event);
    }
    cli.node.close();
    println!("bye");
}
