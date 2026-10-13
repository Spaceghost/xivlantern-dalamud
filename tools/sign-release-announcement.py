#!/usr/bin/env python3
"""Sign a reviewed release-news draft locally. No node, network or publication."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess


def bounded_text(path: Path, limit: int) -> str:
    if path.is_symlink():
        raise ValueError("draft files cannot be symbolic links")
    with path.open("rb") as file:
        data = file.read(limit + 1)
    if len(data) > limit:
        raise ValueError("draft file exceeds its input limit")
    return data.decode("utf-8")


def report_string(text: str) -> str:
    # Native verifier uses Rust Debug strings, including \u{...} escapes.
    if not text.startswith('"') or not text.endswith('"'):
        raise ValueError("invalid verifier string")
    result = []; index = 1
    while index < len(text) - 1:
        char = text[index]; index += 1
        if char != "\\":
            if char == '"': raise ValueError("unescaped verifier quote")
            result.append(char); continue
        if index >= len(text) - 1: raise ValueError("truncated verifier escape")
        char = text[index]; index += 1
        escapes = {"n":"\n", "r":"\r", "t":"\t", "0":"\0", '"':'"', "\\":"\\"}
        if char in escapes:
            result.append(escapes[char]); continue
        if char != "u" or text[index] != "{":
            raise ValueError("unsupported verifier escape")
        end = text.index("}",index)
        if end >= len(text) - 1: raise ValueError("truncated verifier Unicode escape")
        digits = text[index+1:end]
        if not 1 <= len(digits) <= 6 or any(c not in "0123456789abcdefABCDEF" for c in digits):
            raise ValueError("invalid verifier Unicode escape")
        codepoint = int(digits,16)
        if codepoint > 0x10ffff or 0xd800 <= codepoint <= 0xdfff:
            raise ValueError("invalid verifier Unicode codepoint")
        result.append(chr(codepoint)); index = end + 1
    return "".join(result)

def sign(draft: Path, cli: str, key: Path, public: str, previous_seq: int | None = None) -> Path:
    if os.environ.get("CI"):
        raise ValueError("author signing is local only and is disabled in CI")
    if len(public) != 64 or any(c not in "0123456789abcdef" for c in public):
        raise ValueError("expected author public key must be 64 lowercase hex characters")
    metadata = json.loads(bounded_text(draft / "announcement.json", 65536))
    seq = metadata.get("seq")
    title = metadata.get("title")
    if type(seq) is not int or not 1 <= seq < 2**64 or not isinstance(title, str) or not title.strip() or len(title.encode()) > 120:
        raise ValueError("invalid draft sequence or title")
    if metadata.get("signed") is not False or metadata.get("published") is not False or metadata.get("body_file") != "announcement-body.txt":
        raise ValueError("use an unsigned, unpublished prepared draft with its fixed body file")
    if previous_seq is not None and (type(previous_seq) is not int or not 0 <= previous_seq < seq):
        raise ValueError("previous sequence must be nonnegative and less than the draft sequence")
    body_path = draft / "announcement-body.txt"
    body = bounded_text(body_path, 1502).rstrip()
    if len(body.encode()) > 1500:
        raise ValueError("announcement body exceeds the native byte limit")
    packet = draft / "signed.ltannounce"
    # Exclusive reservation prevents accidental replacement or concurrent signing
    # of this draft. Failures leave a marker for review, never an automatic retry.
    descriptor = os.open(packet, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        def author(*args):
            result = subprocess.run([cli, "author", *map(str, args)], capture_output=True, text=True, check=False)
            if result.returncode:
                raise ValueError("author CLI refused the operation; review its key/sequence history before retrying")
            return result.stdout
        actual_public = author("pubkey", "--key", key).strip()
        if actual_public != public:
            raise ValueError("signing key does not match the expected author public key")
        # Use the reviewed snapshot, not a file that could change after inspection.
        signing = ["sign", "--key", key, "--seq", seq, "--title", title, "--body", body]
        if previous_seq is not None:
            signing += ["--previous-seq", previous_seq]
        signed = author(*signing)
        if len(signed.encode()) > 8192 or not signed.strip().startswith("ltannounce"):
            raise ValueError("signer returned an invalid packet")
        verification_file = draft / "verification-input.ltannounce"
        created = False
        try:
            with verification_file.open("x", encoding="utf-8") as verify_input:
                created = True
                verify_input.write(signed)
            report = author("verify", "--public-key", public, "--announcement-file", verification_file)
            fields = dict(line.split(": ", 1) for line in report.splitlines())
            if fields.get("verified public key") != public or fields.get("sequence") != str(seq) or report_string(fields.get("title", "")) != title or report_string(fields.get("body", "")) != body:
                raise ValueError("verified signed content differs from the reviewed draft")
        finally:
            # Delete only our own temporary verifier file.
            if created:
                verification_file.unlink()
        output.write(signed)
        output.flush()
        os.fsync(output.fileno())
    receipt = {"signed": True, "published": False, "public_key": public, "seq": seq,
               "title": title, "body_sha256": hashlib.sha256(body.encode()).hexdigest(),
               "packet_sha256": hashlib.sha256(packet.read_bytes()).hexdigest()}
    with (draft / "signed-receipt.json").open("x", encoding="utf-8") as file:
        json.dump(receipt,file,indent=2); file.write("\n")
    return packet


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("draft", type=Path)
    parser.add_argument("--cli", required=True)
    parser.add_argument("--key", type=Path, required=True)
    parser.add_argument("--public-key", required=True)
    parser.add_argument("--previous-seq", type=int, help="one-time assertion for an older key without history")
    args = parser.parse_args()
    try:
        path = sign(args.draft, args.cli, args.key, args.public_key, args.previous_seq)
    except (ValueError, OSError) as error:
        parser.error(str(error))
    print("Signed and verified local packet: " + str(path) + "; not published")

if __name__ == "__main__":
    main()
