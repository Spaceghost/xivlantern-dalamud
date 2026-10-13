#!/usr/bin/env python3
"""Prepare release news offline. Never read keys, sign, publish, or invoke a node."""
import argparse
import json
from pathlib import Path
import releasekit

MAX_BODY = 1500
MAX_TITLE = 120

def bounded_body(text: str, link: str) -> str:
    footer = "Full release notes: " + link
    if len(footer.encode("utf-8")) > MAX_BODY:
        raise ValueError("release link exceeds the announcement limit")
    lines = []
    for line in text.splitlines():
        candidate = "\n".join(lines + [line, "", footer])
        if len(candidate.encode("utf-8")) > MAX_BODY:
            break
        lines.append(line)
    if not lines:
        raise ValueError("no changelog summary fits; shorten the first entry")
    return "\n".join(lines + ["", footer])

def prepare(config: dict, version: releasekit.Version, seq: int, output: Path) -> None:
    if not 1 <= seq <= 2**64 - 1:
        raise ValueError("sequence must be a positive u64 and greater than every previously signed announcement")
    title = releasekit.title(config, version)
    if len(title.encode("utf-8")) > MAX_TITLE:
        raise ValueError("release title exceeds the native 120-byte limit")
    summary = releasekit.installer_notes(config, version, limit=1000000)
    if not summary:
        raise ValueError("the release has no shipped changelog entries")
    link = "https://github.com/" + config["REPO"] + "/releases/tag/" + version.tag
    body = bounded_body(summary, link)
    full = releasekit.notes(config, version, assets=None)
    output.mkdir(parents=True, exist_ok=False)
    (output / "announcement-body.txt").write_text(body + "\n", encoding="utf-8")
    (output / "release-notes.md").write_text(full, encoding="utf-8")
    (output / "announcement.json").write_text(json.dumps({
        "tag": version.tag, "seq": seq, "title": title,
        "body_file": "announcement-body.txt", "signed": False, "published": False,
        "signing_arguments": ["author", "sign", "--seq", str(seq), "--title", title,
                              "--body-file", str((output / "announcement-body.txt").resolve())],
        "verification_arguments_template": ["author", "verify", "--public-key", "<author-public-key>",
                                            "--announcement-file", "<signed-announcement-file>"],
        "node_handoff_arguments_template": ["--db", "<author-node-database>", "--author-mode", "<author-public-key>",
                                           "--announce-file", "<signed-announcement-file>"],
        "node_handoff_effect": "Starting the author node with this file queues the verified announcement for publication; this preparation step does not do that.",
        "sequence_requirement": "Must exceed every signed or reserved announcement sequence for this author key.",
        "publication_requirement": "Verify the signed announcement offline and hand it to the author node separately; no game-chat command is needed."
    }, indent=2) + "\n", encoding="utf-8")

def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--seq", type=int, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    try:
        prepare(releasekit.conf(), releasekit.Version(args.tag), args.seq, args.out)
    except (releasekit.Fail, ValueError, FileExistsError) as error:
        parser.error(str(error))
    print("Prepared unsigned, unpublished release news in " + str(args.out))
    return 0
if __name__ == "__main__":
    raise SystemExit(main())
