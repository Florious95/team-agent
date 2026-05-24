from __future__ import annotations

import unittest


class CliBoundaryTests(unittest.TestCase):
    def test_package_reexports_legacy_cli_surface(self) -> None:
        from team_agent import cli
        from team_agent.cli import commands, e2e, helpers, parser

        self.assertIs(cli.main, parser.main)
        self.assertIs(cli.TeamAgentArgumentParser, parser.TeamAgentArgumentParser)
        self.assertIs(cli.cmd_status, commands.cmd_status)
        self.assertIs(cli.cmd_send, commands.cmd_send)
        self.assertIs(cli.cmd_e2e, e2e.cmd_e2e)
        self.assertIs(cli._fake_spec, e2e._fake_spec)
        self.assertIs(cli._cli_error_payload, helpers._cli_error_payload)
        self.assertIn("cmd_remove_agent", cli.__all__)
        self.assertIn("_fake_spec", cli.__all__)


if __name__ == "__main__":
    unittest.main()
