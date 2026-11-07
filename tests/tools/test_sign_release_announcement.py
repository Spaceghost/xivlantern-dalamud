import importlib.util
import json
import os
from pathlib import Path
import tempfile
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("sign_news", Path(__file__).resolve().parents[2]/"tools/sign-release-announcement.py")
module = importlib.util.module_from_spec(spec); spec.loader.exec_module(module)
PUBLIC = "a"*64
class SigningTests(unittest.TestCase):
    def draft(self, path):
        (path/"announcement.json").write_text(json.dumps(dict(seq=1,title="Release",body_file="announcement-body.txt",signed=False,published=False)))
        (path/"announcement-body.txt").write_text("Notes\n")
    def fake(self, args, **kwargs):
        self.assertEqual(args[1],"author")
        action=args[2]
        if action=="pubkey": out=PUBLIC+"\n"
        elif action=="sign": out="ltannouncefixture\n"
        elif action=="verify": out=f'verified public key: {PUBLIC}\nsequence: 1\ntitle: "Release"\nbody: "Notes"\n'
        else: self.fail("unexpected operation")
        return types.SimpleNamespace(returncode=0,stdout=out)
    def test_sign_verify_receipt_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ,{"CI":""}), patch.object(module.subprocess,"run",side_effect=self.fake) as runner:
            path=Path(directory); self.draft(path)
            packet=module.sign(path,"cli",Path("key"),PUBLIC)
            before=packet.read_bytes(); receipt=json.loads((path/"signed-receipt.json").read_text())
            self.assertTrue(receipt["signed"]); self.assertFalse(receipt["published"])
            self.assertEqual(runner.call_count,3)
            with self.assertRaises(FileExistsError): module.sign(path,"cli",Path("key"),PUBLIC)
            self.assertEqual(packet.read_bytes(),before); self.assertEqual(runner.call_count,3)
    def test_ci_and_oversized_body_refuse_without_invoking_cli(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(module.subprocess,"run") as runner:
            path=Path(directory); self.draft(path)
            with patch.dict(os.environ,{"CI":"true"}):
                with self.assertRaises(ValueError): module.sign(path,"cli",Path("key"),PUBLIC)
            (path/"announcement-body.txt").write_bytes(b"x"*1503)
            with patch.dict(os.environ,{"CI":""}):
                with self.assertRaises(ValueError): module.sign(path,"cli",Path("key"),PUBLIC)
            runner.assert_not_called(); self.assertFalse((path/"signed.ltannounce").exists())
    def test_key_mismatch_leaves_fail_closed_marker_without_signing(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ,{"CI":""}), patch.object(module.subprocess,"run",return_value=types.SimpleNamespace(returncode=0,stdout="b"*64)) as runner:
            path=Path(directory); self.draft(path)
            with self.assertRaises(ValueError): module.sign(path,"cli",Path("key"),PUBLIC)
            self.assertEqual(runner.call_count,1); self.assertEqual((path/"signed.ltannounce").read_bytes(),b"")
            self.assertFalse((path/"signed-receipt.json").exists())
    def test_migration_is_explicit_and_invalid_previous_values_do_not_run_cli(self):
        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ,{"CI":""}), patch.object(module.subprocess,"run",side_effect=self.fake) as runner:
            path=Path(directory); self.draft(path)
            for previous in [-1,1,True]:
                with self.assertRaises(ValueError): module.sign(path,"cli",Path("key"),PUBLIC,previous)
            runner.assert_not_called()
            module.sign(path,"cli",Path("key"),PUBLIC,0)
            signing = runner.call_args_list[1].args[0]
            self.assertEqual(signing[-2:],["--previous-seq","0"])

    def test_rust_debug_unicode_and_literal_backslashes(self):
        self.assertEqual(module.report_string('"👩\\u{200d}💻\\n"'),"👩\u200d💻\n")
        self.assertEqual(module.report_string('"\\\\u{200d}"'),r"\u{200d}")
if __name__=="__main__": unittest.main()
