import importlib.util
import pathlib
import sys
import tempfile
import unittest
from unittest.mock import patch

TOOLS = pathlib.Path(__file__).resolve().parents[2] / "tools"
sys.path.insert(0, str(TOOLS))
spec = importlib.util.spec_from_file_location("release_announcement", TOOLS / "release-announcement.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class ReleaseAnnouncementTests(unittest.TestCase):
    def test_utf8_budget_preserves_whole_summary_lines_and_link(self):
        text = "BETA: not verified in game.\n" + "\n".join("- BETA: " + "星" * 200 for _ in range(9))
        body = module.bounded_body(text, "https://example.org/release")
        self.assertLessEqual(len(body.encode()), 1500)
        self.assertTrue(body.startswith("BETA: not verified in game."))
        self.assertTrue(body.endswith("Full release notes: https://example.org/release"))
        self.assertEqual(body.count("星") % 200, 0)
    def test_offline_artifacts_are_unsigned_and_existing_directory_is_preserved(self):
        import json
        with tempfile.TemporaryDirectory() as directory:
            out = pathlib.Path(directory) / "draft"
            with patch.object(module.releasekit, "installer_notes", return_value="BETA: not verified in game.\n- BETA: Fix."), patch.object(module.releasekit, "notes", return_value="full exact notes"):
                module.prepare({"NAME":"Lantern", "REPO":"owner/repo"},module.releasekit.Version("v0.1.0-test.1"),7,out)
                data = json.loads((out/"announcement.json").read_text())
                self.assertFalse(data["signed"]); self.assertFalse(data["published"])
                self.assertEqual(data["seq"],7)
                self.assertEqual((out/"release-notes.md").read_text(),"full exact notes")
                before = (out/"announcement-body.txt").read_bytes()
                with self.assertRaises(FileExistsError): module.prepare({"NAME":"Lantern","REPO":"owner/repo"},module.releasekit.Version("v0.1.0-test.1"),8,out)
                self.assertEqual((out/"announcement-body.txt").read_bytes(),before)
    def test_release_entrypoint_prepares_only_and_never_calls_release_or_network_verification(self):
        with patch.object(module.releasekit,"conf",return_value={}), patch.object(module.releasekit,"run",return_value="prepared\n") as runner, patch.object(module.releasekit,"release") as publish, patch.object(module.releasekit,"verify") as verify:
            self.assertEqual(module.releasekit.main(["announcement-draft","v0.1.0","--seq","9","--out","draft with spaces"]),0)
            args = runner.call_args.args
            self.assertEqual(args[-5:],("v0.1.0","--seq","9","--out","draft with spaces"))
            publish.assert_not_called(); verify.assert_not_called()

    def test_local_signing_entrypoint_uses_structured_arguments_without_publishing(self):
        with patch.object(module.releasekit,"conf",return_value={}), patch.object(module.releasekit,"run",return_value="signed locally\n") as runner, patch.object(module.releasekit,"release") as publish, patch.object(module.releasekit,"verify") as verify:
            module.releasekit.main(["announcement-sign","draft with spaces","--cli","native cli","--key","private path","--public-key","a"*64])
            self.assertEqual(runner.call_args.args[-7:],("draft with spaces","--cli","native cli","--key","private path","--public-key","a"*64))
            publish.assert_not_called(); verify.assert_not_called()

    def test_release_draft_is_after_successful_verification_and_skipped_on_failure(self):
        version=module.releasekit.Version("v0.1.0")
        events=[]
        with patch.object(module.releasekit,"verify",side_effect=lambda *args: events.append("verify")), patch.object(module.releasekit,"run",side_effect=lambda *args: events.append("draft") or ""):
            module.releasekit.finish_release({},version,"0.1.0.3",9,pathlib.Path("draft"))
        self.assertEqual(events,["verify","draft"])
        with patch.object(module.releasekit,"verify",side_effect=module.releasekit.Fail("failed")), patch.object(module.releasekit,"run") as runner:
            with self.assertRaises(module.releasekit.Fail): module.releasekit.finish_release({},version,"0.1.0.3",9,pathlib.Path("draft"))
            runner.assert_not_called()
    def test_postrelease_draft_failure_keeps_release_success_clear(self):
        with patch.object(module.releasekit,"verify"), patch.object(module.releasekit,"run",side_effect=SystemExit(1)):
            with self.assertRaisesRegex(module.releasekit.Fail,"already verified.*Retry announcement-draft"):
                module.releasekit.finish_release({},module.releasekit.Version("v0.1.0"),"0.1.0.3",1,pathlib.Path("draft"))

    def test_release_draft_options_refuse_before_any_release_mutation(self):
        with tempfile.TemporaryDirectory() as directory:
            fresh=pathlib.Path(directory)/"draft"
            module.releasekit.announcement_options(1,fresh)
            for seq,out in [(0,fresh),(1,None),(None,fresh),(1,TOOLS.parent),(1,pathlib.Path(directory))]:
                with self.assertRaises(module.releasekit.Fail): module.releasekit.announcement_options(seq,out)
            self.assertFalse(fresh.exists())

    def test_rejects_invalid_sequence_and_overlong_title_before_writing(self):
        with tempfile.TemporaryDirectory() as directory:
            out = pathlib.Path(directory)/"draft"
            for seq in [0,-1,2**64]:
                with self.assertRaises(ValueError): module.prepare({},module.releasekit.Version("v0.1.0"),seq,out)
            with self.assertRaises(ValueError): module.prepare({"NAME":"星"*50},module.releasekit.Version("v0.1.0"),1,out)
            self.assertFalse(out.exists())

if __name__ == "__main__": unittest.main()
